//! VST3 audio effects run only inside the supervised native worker.

mod author_state_entry;
mod initial_parameter_values;
#[cfg(test)]
mod initial_parameter_values_tests;
mod native_author;
#[cfg(all(test, target_os = "windows"))]
mod startup_metadata_diagnostics;

use super::{
    IsolatedAudioProcessorSpecResolver, IsolatedAudioProcessorWorker,
    IsolatedAudioProcessorWorkerBlock, IsolatedAudioProcessorWorkerFactory,
    IsolatedAudioProcessorWorkerPrepareRequest, IsolatedAudioProcessorWorkerSpec,
};
use crate::{
    AudioProcessingMode, AudioProcessorAuxiliaryInputBusContract,
    AudioProcessorAuxiliaryInputContract, AudioProcessorExecutionContract, AudioProcessorHostError,
    AudioProcessorPrepareRequest, AudioProcessorTail,
};
use mondrian_core::automation::{
    ParameterInterpolation, ParameterInvalidValuePolicy, ParameterNumericContract, ParameterSchema,
    ParameterUnit, PropertyValue,
};
use mondrian_core::{AudioChannelLayout, ParameterId};
use mondrian_timeline::audio::AudioProcessorDefinitionRef;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use vst3_host::audio::{
    AudioBusLayout, AudioBusRole, BusArrangements, BusAudioBuffers, BusDirection, MediaType,
    NativeAudioBusInfoSet, SpeakerArrangement,
};
use vst3_host::plugin::{Plugin, ProcessMode};
use vst3_host::Vst3Host;

/// Hidden application mode dedicated to the VST3 ABI.
pub const VST3_AUDIO_WORKER_ARGUMENT: &str = "--internal-vst3-audio-worker-v1";
const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;
pub(super) const MAX_STATE_BYTES: usize = 128 * 1024;

/// Actual product consumers, shared by discovery and every processing reload.
pub(super) fn product_plugin_interfaces() -> vst3_host::PluginInterfaceSupport {
    use vst3_host::{PluginInterface as Interface, PluginInterfaceSupport};
    let interfaces = PluginInterfaceSupport::none()
        .with(Interface::Component)
        .with(Interface::AudioProcessor)
        .with(Interface::EditController)
        .with(Interface::ConnectionPoint)
        .with(Interface::ProcessContextRequirements);
    #[cfg(windows)]
    let interfaces = interfaces
        .with(Interface::PlugView)
        .with(Interface::PlugViewContentScaleSupport)
        .with(Interface::DataExchangeReceiver);
    interfaces
}
const MAX_PARAMETERS: usize = 1024;
const MAX_EVENTS_PER_BLOCK: usize = 4096;

/// Stable VST3 parameter facts in normalized VST3 coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Vst3ParameterDescriptor {
    /// VST3 definition-local `ParamID`.
    pub id: u32,
    /// Plugin supplied display name.
    pub name: String,
    /// Default normalized value.
    pub default_normalized: f64,
    /// Number of discrete gaps; zero denotes a continuous parameter.
    pub step_count: i32,
    /// Whether the plugin admits host automation.
    pub can_automate: bool,
    /// Whether the parameter is read-only.
    pub read_only: bool,
    /// Native flags, retained for exact discovery/worker comparison.
    pub flags: u32,
}

impl Vst3ParameterDescriptor {
    /// Stable authoring identity independent of discovery order.
    pub fn parameter_id(&self) -> Result<ParameterId, AudioProcessorHostError> {
        ParameterId::new(format!("vst3.param.{}", self.id))
            .map_err(|error| invalid(format!("invalid VST3 parameter identity: {error}")))
    }

    /// Project the official normalized coordinate into this descriptor's same
    /// canonical author domain. Stepped values use the existing nearest index
    /// convention; continuous values retain exact Float64 bits.
    pub fn author_scalar_from_normalized(
        &self,
        value: f64,
    ) -> Result<f64, AudioProcessorHostError> {
        validate_parameter(self)?;
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(invalid("VST3 normalized parameter value is invalid"));
        }
        Ok(if self.step_count > 0 {
            (value * f64::from(self.step_count)).round()
        } else {
            value
        })
    }

    /// Project this same descriptor's exact canonical scalar into the native
    /// normalized ABI coordinate, without a Float32 or display conversion.
    pub fn normalized_from_author_scalar(
        &self,
        value: f64,
    ) -> Result<f64, AudioProcessorHostError> {
        validate_parameter(self)?;
        let maximum = if self.step_count > 0 {
            f64::from(self.step_count)
        } else {
            1.0
        };
        if !value.is_finite()
            || value < 0.0
            || value > maximum
            || self.step_count > 0 && value.fract() != 0.0
        {
            return Err(invalid("VST3 canonical parameter scalar is invalid"));
        }
        Ok(if self.step_count > 0 {
            value / maximum
        } else {
            value
        })
    }

    /// Host authoring schema. Stepped VST3 values are stored as integer indices.
    pub fn authoring_schema(&self) -> Result<ParameterSchema, AudioProcessorHostError> {
        validate_parameter(self)?;
        let stepped = self.step_count > 0;
        let scalar = self.author_scalar_from_normalized(self.default_normalized)?;
        let default = if stepped {
            PropertyValue::Int(scalar as i64)
        } else {
            PropertyValue::Double(scalar)
        };
        let mut schema = ParameterSchema::v1(self.parameter_id()?, default);
        schema.is_animatable = self.can_automate && !self.read_only;
        if stepped {
            schema.allowed_interpolations = vec![ParameterInterpolation::Hold];
        }
        let numeric = ParameterNumericContract::closed(
            0.0,
            if stepped {
                f64::from(self.step_count)
            } else {
                1.0
            },
            stepped.then_some(1.0),
            ParameterInvalidValuePolicy::Reject,
        )
        .map_err(|error| invalid(format!("invalid VST3 numeric contract: {error}")))?;
        schema = schema.with_numeric_contract(ParameterUnit::Unitless, numeric);
        schema
            .validate()
            .map_err(|error| invalid(format!("invalid VST3 authoring schema: {error}")))?;
        Ok(schema)
    }
}

/// One probed VST3 audio-effect class bound to an exact installed binary.
#[derive(Debug, Clone)]
pub struct Vst3PluginRegistration {
    /// Canonical 32-character hexadecimal class ID.
    pub class_id: String,
    /// Factory vendor, when present in the authoring definition.
    pub vendor: Option<String>,
    /// Absolute path to the selected native binary.
    pub binary_path: PathBuf,
    /// SHA-256 of that native binary.
    pub binary_sha256: [u8; 32],
    /// Exact execution contract probed for the target render configuration.
    pub execution_contract: AudioProcessorExecutionContract,
    /// Ordered auxiliary input ports admitted after restoring the selected native state.
    pub auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    /// Complete parameter metadata, including non-editable parameters.
    pub parameters: Vec<Vst3ParameterDescriptor>,
}

/// Parent-side registry that packages a VST3 definition for supervised execution.
pub struct Vst3AudioProcessorSpecResolver {
    helper_executable: PathBuf,
    plugins: BTreeMap<String, Vst3PluginRegistration>,
}

impl Vst3AudioProcessorSpecResolver {
    /// Admit unique, already probed classes for one helper executable.
    pub fn new(
        helper_executable: PathBuf,
        plugins: impl IntoIterator<Item = Vst3PluginRegistration>,
    ) -> Result<Self, AudioProcessorHostError> {
        if !helper_executable.is_absolute() {
            return Err(invalid("VST3 helper executable must be absolute"));
        }
        let mut by_id = BTreeMap::new();
        for plugin in plugins {
            validate_registration(&plugin)?;
            if by_id.insert(plugin.class_id.clone(), plugin).is_some() {
                return Err(invalid(
                    "duplicate VST3 class ID requires explicit selection",
                ));
            }
        }
        Ok(Self { helper_executable, plugins: by_id })
    }
}

impl IsolatedAudioProcessorSpecResolver for Vst3AudioProcessorSpecResolver {
    fn resolve(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<IsolatedAudioProcessorWorkerSpec, AudioProcessorHostError> {
        let AudioProcessorDefinitionRef::Vst3 { class_id, vendor, schema_version, binary_sha256 } =
            request.definition()
        else {
            return Err(unavailable(
                "only VST3 definitions are handled by this resolver",
            ));
        };
        if *schema_version != 1 {
            return Err(unavailable("VST3 definition schema version is unsupported"));
        }
        let plugin = self
            .plugins
            .get(class_id)
            .ok_or_else(|| unavailable(format!("VST3 class {class_id} is not registered")))?;
        if vendor != &plugin.vendor || *binary_sha256 != Some(plugin.binary_sha256) {
            return Err(unavailable(
                "VST3 authored identity differs from the selected installation",
            ));
        }
        plugin.auxiliary_inputs.validate(request.render_contract().channel_layout)?;
        let mut parameter_ids = Vec::with_capacity(request.parameters().len());
        let mut vst3_ids = Vec::with_capacity(request.parameters().len());
        for (id, parameter) in request.parameters() {
            let descriptor = plugin
                .parameters
                .iter()
                .find(|descriptor| descriptor.parameter_id().as_ref().ok() == Some(id))
                .ok_or_else(|| invalid("authored VST3 parameter is absent from the plugin"))?;
            if descriptor.read_only || parameter.schema != descriptor.authoring_schema()? {
                return Err(invalid(
                    "authored VST3 parameter contract differs from the plugin",
                ));
            }
            parameter_ids.push(id.clone());
            vst3_ids.push(descriptor.id);
        }
        let state = request.opaque_state().map(ToOwned::to_owned);
        if state.as_ref().is_some_and(|state| state.len() > MAX_STATE_BYTES) {
            return Err(invalid("VST3 state exceeds the worker payload limit"));
        }
        let payload = serde_json::to_vec(&Vst3Payload {
            binary_path: plugin.binary_path.clone(),
            class_id: plugin.class_id.clone(),
            vendor: plugin.vendor.clone(),
            binary_sha256: plugin.binary_sha256,
            state,
            parameters: plugin.parameters.clone(),
            parameter_ids: vst3_ids,
        })
        .map_err(|error| invalid(format!("VST3 worker payload encoding failed: {error}")))?;
        IsolatedAudioProcessorWorkerSpec::new(
            self.helper_executable.clone(),
            payload,
            plugin.execution_contract,
            plugin.auxiliary_inputs.clone(),
            parameter_ids,
            request.render_contract(),
        )?
        .with_native_worker_lane_resources(native_worker_lane_profile_for_parameters(
            &plugin.parameters,
        )?)
        .with_dispatch_argument(Some(OsString::from(VST3_AUDIO_WORKER_ARGUMENT)))
        .with_deadlines(
            std::time::Duration::from_secs(15),
            std::time::Duration::from_millis(100),
        )?
        .with_state_entry_timeout(std::time::Duration::from_secs(10))
    }
}

// The concrete preparation codec remains the only interpreter for this format.
// This non-RT helper neither loads a plugin nor grants author/install authority.
pub(super) fn payload_with_configuration_seed(
    payload: &[u8],
    configuration: &[u8],
) -> Result<Vec<u8>, AudioProcessorHostError> {
    if configuration.len() > MAX_STATE_BYTES {
        return Err(invalid(
            "native configuration seed exceeds the existing state limit",
        ));
    }
    let mut decoded: Vst3Payload = serde_json::from_slice(payload)
        .map_err(|error| invalid(format!("invalid concrete payload for native seed: {error}")))?;
    // Retire the decoded old seed outside publication/SDK guards before
    // allocating its replacement; preserve all other concrete metadata.
    decoded.state = None;
    let mut seed = Vec::new();
    seed.try_reserve_exact(configuration.len()).map_err(|error| {
        invalid(format!(
            "native configuration seed allocation refused: {error}"
        ))
    })?;
    seed.extend_from_slice(configuration);
    decoded.state = Some(seed);
    super::encode_native_configuration_payload(&decoded)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vst3Payload {
    binary_path: PathBuf,
    class_id: String,
    vendor: Option<String>,
    binary_sha256: [u8; 32],
    state: Option<Vec<u8>>,
    parameters: Vec<Vst3ParameterDescriptor>,
    parameter_ids: Vec<u32>,
}

// Non-RT decoding/identity validation may allocate Rust owners. No foreign
// plugin query or worker launch is part of this preparation request.
fn validated_preparation_payload(
    request: &IsolatedAudioProcessorWorkerPrepareRequest,
) -> Result<Vst3Payload, AudioProcessorHostError> {
    let payload: Vst3Payload = serde_json::from_slice(request.payload())
        .map_err(|error| invalid(format!("invalid VST3 worker payload: {error}")))?;
    let registration = Vst3PluginRegistration {
        class_id: payload.class_id.clone(),
        vendor: payload.vendor.clone(),
        binary_path: payload.binary_path.clone(),
        binary_sha256: payload.binary_sha256,
        execution_contract: request.plugin_execution_contract(),
        auxiliary_inputs: request.auxiliary_inputs().clone(),
        parameters: payload.parameters.clone(),
    };
    validate_registration(&registration)?;
    if payload.state.as_ref().is_some_and(|state| state.len() > MAX_STATE_BYTES)
        || payload.parameter_ids.len() != request.parameter_ids().len()
    {
        return Err(invalid("VST3 state or parameter-lane capacity is invalid"));
    }
    for (id, native_id) in request.parameter_ids().iter().zip(&payload.parameter_ids) {
        let descriptor = payload
            .parameters
            .iter()
            .find(|descriptor| descriptor.id == *native_id)
            .ok_or_else(|| invalid("VST3 parameter lane has no descriptor"))?;
        if descriptor.read_only || descriptor.parameter_id()? != *id {
            return Err(invalid("VST3 parameter lane identity is invalid"));
        }
    }
    Ok(payload)
}

/// Child-only VST3 ABI factory. The editor never loads user plugin code.
pub struct Vst3AudioProcessorWorkerFactory;

impl IsolatedAudioProcessorWorkerFactory for Vst3AudioProcessorWorkerFactory {
    fn native_worker_lane_resources(
        &self,
    ) -> Result<Option<crate::NativeWorkerLaneResourceProfile>, AudioProcessorHostError> {
        native_worker_lane_profile()
    }
    fn native_worker_lane_resources_for_preparation(
        &self,
        request: &IsolatedAudioProcessorWorkerPrepareRequest,
    ) -> Result<Option<crate::NativeWorkerLaneResourceProfile>, AudioProcessorHostError> {
        let payload = validated_preparation_payload(request)?;
        native_worker_lane_profile_for_parameters(&payload.parameters)
    }
    fn prepare(
        &self,
        request: IsolatedAudioProcessorWorkerPrepareRequest,
    ) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError> {
        self.prepare_native(request)
            .map(|worker| Box::new(worker) as Box<dyn IsolatedAudioProcessorWorker>)
    }
}

impl Vst3AudioProcessorWorkerFactory {
    fn prepare_native(
        &self,
        request: IsolatedAudioProcessorWorkerPrepareRequest,
    ) -> Result<Vst3Worker, AudioProcessorHostError> {
        if !request.note_input_contract().ports.is_empty() {
            return Err(unavailable(
                "VST3 Note input is not supported by this adapter",
            ));
        }
        let payload = validated_preparation_payload(&request)?;
        if request.native_worker_lane_resources()
            != native_worker_lane_profile_for_parameters(&payload.parameters)?
        {
            return Err(invalid(
                "VST3 input storage request differs from prepared metadata",
            ));
        }
        let registration = Vst3PluginRegistration {
            class_id: payload.class_id.clone(),
            vendor: payload.vendor.clone(),
            binary_path: payload.binary_path.clone(),
            binary_sha256: payload.binary_sha256,
            execution_contract: request.plugin_execution_contract(),
            auxiliary_inputs: request.auxiliary_inputs().clone(),
            parameters: payload.parameters.clone(),
        };
        if fingerprint(&payload.binary_path)? != payload.binary_sha256 {
            return Err(unavailable("VST3 binary changed since processor admission"));
        }
        let render = request.render_contract();
        let channels = match render.channel_layout {
            AudioChannelLayout::Mono => 1,
            AudioChannelLayout::Stereo => 2,
            _ => return Err(invalid("VST3 channel layout is not admitted")),
        };
        let mode = match render.processing_mode {
            AudioProcessingMode::Realtime => ProcessMode::Realtime,
            AudioProcessingMode::Offline => ProcessMode::Offline,
        };
        #[cfg(target_os = "windows")]
        let native_main = std::rc::Rc::new(
            vst3_host::window::WindowsEditorMainThread::initialize().map_err(|error| {
                unavailable(format!(
                    "VST3 actual native main initialization failed: {error}"
                ))
            })?,
        );
        let mut host = Vst3Host::builder()
            .plugin_interfaces(product_plugin_interfaces())
            .sample_rate(f64::from(render.sample_rate))
            .block_size(render.max_block_frames)
            .input_channels(channels)
            .output_channels(channels)
            .build()
            .map_err(|error| unavailable(format!("VST3 host creation failed: {error}")))?;
        let mut plugin = host
            .load_plugin_class(&payload.binary_path, &payload.class_id)
            .map_err(|error| unavailable(format!("VST3 class load failed: {error}")))?;
        #[cfg(all(test, target_os = "windows"))]
        startup_metadata_diagnostics::observe("after_load", &plugin, &payload.parameters);
        if fingerprint(&payload.binary_path)? != payload.binary_sha256 {
            return Err(unavailable("VST3 binary changed while loading the class"));
        }
        if plugin.info().uid != payload.class_id
            || payload.vendor.as_deref().is_some_and(|vendor| plugin.info().vendor != vendor)
        {
            return Err(invalid(
                "VST3 class or vendor differs from the admitted definition",
            ));
        }
        if let Some(state) = payload.state.as_deref() {
            plugin
                .load_state(state)
                .map_err(|error| unavailable(format!("VST3 state restore failed: {error}")))?;
        }
        #[cfg(all(test, target_os = "windows"))]
        startup_metadata_diagnostics::observe("after_restore", &plugin, &payload.parameters);
        plugin
            .set_process_mode(mode)
            .map_err(|error| invalid(format!("VST3 processing mode is unsupported: {error}")))?;
        #[cfg(all(test, target_os = "windows"))]
        startup_metadata_diagnostics::observe(
            "after_set_process_mode",
            &plugin,
            &payload.parameters,
        );
        let input_names = validate_loaded(&mut plugin, &registration, render.channel_layout)?;
        #[cfg(all(test, target_os = "windows"))]
        startup_metadata_diagnostics::observe("after_validate", &plugin, &payload.parameters);
        let buffers = plugin
            .create_bus_audio_buffers(render.max_block_frames)
            .map_err(|error| invalid(format!("VST3 bus buffer preparation failed: {error}")))?;
        plugin
            .start_processing()
            .map_err(|error| unavailable(format!("VST3 processing start failed: {error}")))?;
        #[cfg(all(test, target_os = "windows"))]
        startup_metadata_diagnostics::observe(
            "after_start_processing",
            &plugin,
            &payload.parameters,
        );
        Ok(Vst3Worker {
            native_lane: request.native_worker_lane_resources(),
            last_parameter_values: vec![None; payload.parameter_ids.len()],
            configuration_phase:
                super::native_configuration_control::NativeConfigurationPhase::Admitted,
            #[cfg(target_os = "windows")]
            native_parent: None,
            plugin: Some(plugin),
            host,
            payload,
            contract: request.plugin_execution_contract(),
            render,
            buffers,
            auxiliary_inputs: registration.auxiliary_inputs,
            input_names,
            last_end: None,
            initial_signal_phase: InitialSignalPhase::Unserviced,
            native_binding: None,
            native_author_delivery: None,
            prepared_author_entry: None,
            #[cfg(target_os = "windows")]
            native_main,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitialSignalPhase {
    Unserviced,
    AwaitingFirstSignal,
    Entered,
}

struct Vst3Worker {
    native_lane: Option<crate::NativeWorkerLaneResourceProfile>,
    last_parameter_values: Vec<Option<f64>>,
    configuration_phase: super::native_configuration_control::NativeConfigurationPhase,
    #[cfg(target_os = "windows")]
    native_parent: Option<vst3_host::window::WindowsPluginEditorParent>,
    plugin: Option<Plugin>,
    host: Vst3Host,
    payload: Vst3Payload,
    contract: AudioProcessorExecutionContract,
    render: crate::AudioRenderContract,
    buffers: BusAudioBuffers,
    auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    input_names: Vec<crate::AudioProcessorInputName>,
    last_end: Option<i64>,
    // One lifecycle authority: Bind may close initialization before first PCM.
    initial_signal_phase: InitialSignalPhase,
    native_binding: Option<crate::NativeAudioObjectBindReceipt>,
    native_author_delivery: Option<native_author::PendingDelivery>,
    prepared_author_entry: Option<author_state_entry::PreparedAuthorEntry>,
    // Last field: same-main OLE survives the plugin, controller, native module
    // and any explicitly removed native parent. It is never sent to DSP.
    #[cfg(target_os = "windows")]
    native_main: std::rc::Rc<vst3_host::window::WindowsEditorMainThread>,
}

#[cfg(target_os = "windows")]
impl Vst3Worker {
    fn native_editor_binding_matches(
        &self,
        binding: crate::NativeAudioObjectBindRequest,
    ) -> Result<bool, AudioProcessorHostError> {
        let actual = self.native_audio_object_observation()?;
        Ok(self.native_binding.is_some_and(|receipt| {
            receipt.binding() == binding.binding && receipt.object() == binding.expected_object
        }) && actual == Some(binding.expected_object))
    }
}

#[cfg(target_os = "windows")]
fn classify_editor_close_error(
    binding: crate::NativeAudioObjectBindRequest,
    error: vst3_host::error::Error,
) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
    match error {
        vst3_host::error::Error::EditorDetachFailed {
            stage: vst3_host::error::EditorDetachStage::FrameClear,
            ..
        } => Ok(crate::NativeAudioEditorOutcome::ClosePending {
            binding,
            stage: crate::NativeAudioEditorCloseStage::ClearHostFrame,
        }),
        error => Err(native_editor_removal_failed(error)),
    }
}

impl IsolatedAudioProcessorWorker for Vst3Worker {
    fn native_worker_lane_resources(&self) -> Option<crate::NativeWorkerLaneResourceProfile> {
        self.native_lane
    }
    fn native_staged_seed_layout(&self) -> crate::NativeHostBufferLayout {
        let bytes = match &self.configuration_phase {
            super::native_configuration_control::NativeConfigurationPhase::ReadyForInstall {
                candidate,
                ..
            } => candidate
                .staged_receipt()
                .map_or(0, |receipt| receipt.child_seed_capacity_bytes),
            _ => 0,
        };
        crate::NativeHostBufferLayout { bytes, alignment: align_of::<u8>() }
    }
    fn uses_native_processing_split(&self) -> bool {
        cfg!(target_os = "windows")
    }

    #[cfg(target_os = "windows")]
    fn split_native_processing(
        &mut self,
    ) -> Result<super::IsolatedAudioProcessorWorkerSplit<'_>, AudioProcessorHostError> {
        let plugin = self
            .plugin
            .as_mut()
            .ok_or_else(|| unavailable("VST3 retained native processor is closed"))?;
        let vst3_host::plugin::PluginEditorProcessingSplit { editor, processor } =
            plugin.split_editor_processing().map_err(|error| {
                unavailable(format!("VST3 actual native thread split failed: {error}"))
            })?;
        let signal = VstSignalFields {
            last_parameter_values: &mut self.last_parameter_values,
            configuration_phase: &self.configuration_phase,
            payload: &self.payload,
            render: self.render,
            buffers: &mut self.buffers,
            auxiliary_inputs: &self.auxiliary_inputs,
            last_end: &mut self.last_end,
        };
        Ok(super::IsolatedAudioProcessorWorkerSplit {
            main: Box::new(VstMainRole {
                editor,
                native_main: &self.native_main,
                native_parent: &mut self.native_parent,
                configuration_phase: &self.configuration_phase,
                native_binding: &self.native_binding,
                native_author_delivery: &self.native_author_delivery,
                prepared_author_entry: &self.prepared_author_entry,
            }),
            processing: Box::new(VstDspRole { native: processor, signal }),
        })
    }

    #[cfg(target_os = "windows")]
    fn service_native_editor_close_request(
        &mut self,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        let Some(receipt) = self.native_binding else {
            return Ok(crate::NativeAudioEditorOutcome::Refused(
                crate::NativeAudioEditorRefusal::NotInstalled,
            ));
        };
        self.close_native_audio_editor(crate::NativeAudioObjectBindRequest {
            binding: receipt.binding(),
            expected_object: receipt.object(),
        })
    }

    #[cfg(target_os = "windows")]
    fn close_native_editor_owner(&mut self) -> Result<(), AudioProcessorHostError> {
        let Some(plugin) = self.plugin.as_mut() else {
            return if self.native_parent.is_some() {
                Err(AudioProcessorHostError::WorkerFailed {
                    operation: "native editor owner removal",
                    detail: "same native parent outlived its processor owner".into(),
                })
            } else {
                Ok(())
            };
        };
        if let Some(parent) = self.native_parent.as_mut() {
            let vst3_host::plugin::PluginEditorProcessingSplit { mut editor, processor } =
                plugin.split_editor_processing().map_err(native_editor_removal_failed)?;
            // This is after the real DSP scope joined. The temporary unused
            // processing borrow is dropped, then the same parent checks its
            // private exact native-object attachment identity before removal.
            drop(processor);
            parent.close(&mut editor).map_err(native_editor_removal_failed)?;
            self.native_parent = None;
        } else {
            // An unsuccessful attach can retain only failed host-frame cleanup.
            // The canonical attachment model decides the exact remaining step;
            // absence of a parent must never invent successful native removal.
            plugin.close_editor().map_err(native_editor_removal_failed)?;
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    fn native_editor_detach_stage(&self) -> Option<crate::NativeAudioEditorCloseStage> {
        self.plugin.as_ref()?.native_editor_pending_detach().map(|stage| match stage {
            vst3_host::error::EditorDetachStage::Removal => {
                crate::NativeAudioEditorCloseStage::RemoveView
            }
            vst3_host::error::EditorDetachStage::FrameClear => {
                crate::NativeAudioEditorCloseStage::ClearHostFrame
            }
        })
    }

    #[cfg(target_os = "windows")]
    fn open_native_audio_editor(
        &mut self,
        request: crate::NativeAudioEditorOpenRequest,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        use crate::{NativeAudioEditorOutcome as Outcome, NativeAudioEditorRefusal as Refusal};
        let binding = request.binding();
        if !self.native_editor_binding_matches(binding)? {
            return Ok(Outcome::Refused(Refusal::OwnerChanged));
        }
        if self.pending_reprepare().is_some() || self.configuration_phase.ready_token().is_some() {
            return Ok(Outcome::Refused(Refusal::ConfigurationPending));
        }
        let plugin = self
            .plugin
            .as_mut()
            .ok_or_else(|| unavailable("native editor processor owner is closed"))?;
        if plugin.native_editor_pending_detach().is_some() {
            return Ok(Outcome::Refused(Refusal::ConfigurationPending));
        }
        if !plugin.has_editor() {
            return Ok(Outcome::Refused(Refusal::NoEditor));
        }
        let vst3_host::plugin::PluginEditorProcessingSplit { mut editor, processor } =
            plugin.split_editor_processing().map_err(native_editor_removal_failed)?;
        drop(processor);
        if self.native_parent.is_none() {
            match vst3_host::window::WindowsPluginEditorParent::open(
                std::rc::Rc::clone(&self.native_main),
                &mut editor,
                request.title(),
            ) {
                Ok(parent) => self.native_parent = Some(parent),
                Err(vst3_host::error::Error::EditorUnavailable) => {
                    return Ok(Outcome::Refused(Refusal::NoEditor))
                }
                Err(vst3_host::error::Error::EditorPlatformUnsupported) => {
                    return Ok(Outcome::Refused(Refusal::UnsupportedAdapter))
                }
                Err(error) => return classify_editor_close_error(binding, error),
            }
        }
        let (width, height) = editor.editor_size().map_err(native_editor_removal_failed)?;
        let width = u32::try_from(width)
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| invalid("native view width is not positive"))?;
        let height = u32::try_from(height)
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| invalid("native view height is not positive"))?;
        Ok(Outcome::Opened { binding, width, height })
    }

    #[cfg(target_os = "windows")]
    fn close_native_audio_editor(
        &mut self,
        binding: crate::NativeAudioObjectBindRequest,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        use crate::{NativeAudioEditorOutcome as Outcome, NativeAudioEditorRefusal as Refusal};
        if !self.native_editor_binding_matches(binding)? {
            return Ok(Outcome::Refused(Refusal::OwnerChanged));
        }
        let plugin = self
            .plugin
            .as_mut()
            .ok_or_else(|| unavailable("native editor processor owner is closed"))?;
        let result = if let Some(parent) = self.native_parent.as_mut() {
            let vst3_host::plugin::PluginEditorProcessingSplit { mut editor, processor } =
                plugin.split_editor_processing().map_err(native_editor_removal_failed)?;
            drop(processor);
            parent.close(&mut editor)
        } else {
            plugin.close_editor()
        };
        match result {
            Ok(()) => {
                self.native_parent = None;
                Ok(Outcome::Closed { binding })
            }
            Err(error) => classify_editor_close_error(binding, error),
        }
    }

    fn native_audio_object_observation(
        &self,
    ) -> Result<Option<crate::NativeAudioObjectObservation>, AudioProcessorHostError> {
        let Some(owner) = self.plugin.as_ref().and_then(Plugin::native_editor_attachment_owner)
        else {
            return Ok(None);
        };
        let process = std::num::NonZeroU32::new(std::process::id())
            .ok_or_else(|| unavailable("native child PID is zero"))?;
        Ok(Some(crate::NativeAudioObjectObservation::new(
            process,
            owner.serial(),
        )))
    }

    fn poll_native_observations(
        &mut self,
        consumption: crate::NativeAudioObservationConsumption,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        let object = self.native_audio_object_observation()?;
        let author = self.observe_native_author(consumption)?;
        Ok(crate::NativeAudioObservations {
            object,
            author,
            editor_cleanup: self.native_editor_detach_stage(),
            pending_reprepare: self.pending_reprepare(),
            ..Default::default()
        })
    }

    fn bind_native_audio_object(
        &mut self,
        request: crate::NativeAudioObjectBindRequest,
    ) -> Result<crate::NativeAudioObjectBindOutcome, AudioProcessorHostError> {
        let Some(actual) = self.native_audio_object_observation()? else {
            return Ok(crate::NativeAudioObjectBindOutcome::Refused(
                crate::NativeAudioObjectBindRefusal::ObjectChanged,
            ));
        };
        if let Some(receipt) = self.native_binding
            && receipt.binding() == request.binding
            && receipt.object() == actual
            && actual == request.expected_object
        {
            return Ok(crate::NativeAudioObjectBindOutcome::Bound(receipt));
        }
        match crate::NativeAudioObjectBindReceipt::acknowledge(request, actual) {
            Ok(receipt) => {
                // Pure object/receipt validation above precedes any initial allocation.
                if !self.service_first_parameter_values() {
                    return Ok(crate::NativeAudioObjectBindOutcome::Refused(
                        crate::NativeAudioObjectBindRefusal::UnsupportedAdapter,
                    ));
                }
                self.native_binding = Some(receipt);
                Ok(crate::NativeAudioObjectBindOutcome::Bound(receipt))
            }
            Err(refusal) => Ok(crate::NativeAudioObjectBindOutcome::Refused(refusal)),
        }
    }

    fn native_configuration_cohort(&self) -> Option<super::NativeRequestCohort> {
        let cohort = self.plugin.as_ref()?.pending_restart_cohort()?;
        if !cohort.pending() {
            return None;
        }
        Some(super::NativeRequestCohort {
            serial: cohort.serial(),
            request: mondrian_core::NativeAudioReprepareRequest::Vst3Restart {
                flags: cohort.flags().bits() as u32,
            },
        })
    }

    fn service_native_configuration(
        &mut self,
        request: super::NativeConfigurationControlRequest<'_>,
    ) -> Result<super::NativeConfigurationControlOutcome, AudioProcessorHostError> {
        use super::native_configuration_control::{
            observed_contract, NativeConfigurationPhase as Phase,
        };
        use super::{
            NativeConfigurationCandidate as Candidate,
            NativeConfigurationControlOutcome as Outcome, NativeConfigurationFormat as Format,
            NativeConfigurationRefusal as Refusal, NativeControlBufferReceipt as Buffers,
        };
        use vst3_host::configuration_capture::ConfigurationCaptureError as CaptureError;
        if self.configuration_phase.ready_token().is_some() {
            return Ok(Outcome::CapabilityRefused(Refusal::AwaitingInstallation));
        }
        let Some(expected_native) = self
            .plugin
            .as_ref()
            .ok_or_else(|| unavailable("VST3 retained processor is closed"))?
            .pending_restart_cohort()
        else {
            return Ok(Outcome::CapabilityRefused(Refusal::UnsupportedAdapter));
        };
        let actual = expected_native.pending().then_some(super::NativeRequestCohort {
            serial: expected_native.serial(),
            request: mondrian_core::NativeAudioReprepareRequest::Vst3Restart {
                flags: expected_native.flags().bits() as u32,
            },
        });
        if actual != Some(request.expected_cohort) {
            return Ok(Outcome::StillPending(actual));
        }
        if self
            .plugin
            .as_ref()
            .ok_or_else(|| unavailable("VST3 retained processor is closed"))?
            .pending_processor_parameter_input()
            .map_err(|error| {
                unavailable(format!(
                    "VST3 configuration input observation failed: {error}"
                ))
            })?
        {
            return Ok(Outcome::CapabilityRefused(
                Refusal::ConfigurationUnavailable,
            ));
        }
        self.configuration_phase = Phase::Blocked(request.expected_cohort.request);
        if request.expected_cohort.exhausted() {
            return Ok(Outcome::CapabilityRefused(Refusal::SerialExhausted));
        }
        let mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags } =
            request.expected_cohort.request
        else {
            return Err(invalid(
                "VST3 configuration control received a different ABI cohort",
            ));
        };
        // This bounded slice admits latency only, not hidden bus/parameter changes.
        if flags != 8 {
            return Ok(Outcome::CapabilityRefused(Refusal::UnsupportedRequest));
        }
        if request.configuration_limit_bytes == 0
            || request.configuration_limit_bytes > MAX_STATE_BYTES
        {
            return Ok(Outcome::CapabilityRefused(Refusal::ConfigurationTooLarge));
        }
        if request.canonical_constant_values.len() != self.last_parameter_values.len()
            || self
                .last_parameter_values
                .iter()
                .zip(request.canonical_constant_values)
                .any(|(actual, expected)| !expected.is_finite() || *actual != Some(*expected))
        {
            // Only successful delivered DSP values prove component configuration.
            // Controller mirrors and setParamNormalized are not such evidence.
            return Ok(Outcome::CapabilityRefused(
                Refusal::UnsupportedConstantProfile,
            ));
        }
        if fingerprint(&self.payload.binary_path)? != self.payload.binary_sha256 {
            return Err(unavailable(
                "VST3 binary changed before retained configuration service",
            ));
        }
        let plugin = self
            .plugin
            .as_mut()
            .ok_or_else(|| unavailable("VST3 retained processor is closed"))?;
        plugin.reactivate_configuration_stopped().map_err(|error| {
            unavailable(format!(
                "VST3 legal configuration activation failed: {error}"
            ))
        })?;
        let inputs = observe_active_audio_inputs(plugin, self.render.channel_layout)?;
        if inputs != self.auxiliary_inputs
            || parameter_descriptors(plugin)? != self.payload.parameters
        {
            return Ok(Outcome::CapabilityRefused(Refusal::MetadataChanged));
        }
        let tail = match plugin.tail_samples() {
            0 => AudioProcessorTail::None,
            u32::MAX => AudioProcessorTail::Infinite,
            frames => AudioProcessorTail::Finite(frames as usize),
        };
        let observed = observed_contract(self.contract, plugin.latency_samples() as usize, tail)?;
        let captured = match plugin.save_state_bounded(
            request.configuration_limit_bytes,
            request.control_buffer_grant_bytes,
        ) {
            Ok(state) => state,
            Err(CaptureError::Unavailable) => {
                return Ok(Outcome::CapabilityRefused(
                    Refusal::ConfigurationUnavailable,
                ))
            }
            Err(CaptureError::StateLimitExceeded { .. } | CaptureError::InvalidLimit { .. }) => {
                return Ok(Outcome::CapabilityRefused(Refusal::ConfigurationTooLarge))
            }
            Err(
                CaptureError::BufferGrantExceeded { .. }
                | CaptureError::AllocationExtent { .. }
                | CaptureError::AllocationFailed,
            ) => return Ok(Outcome::CapabilityRefused(Refusal::BufferGrantExceeded)),
            Err(error) => {
                return Err(unavailable(format!(
                    "VST3 bounded configuration capture failed: {error}"
                )))
            }
        };
        let buffer_receipt = captured.buffer_receipt();
        let candidate = Candidate {
            serviced_cohort: request.expected_cohort,
            control_token: request.control_token,
            observed_contract: observed,
            format: Format::Vst3ComponentAndController,
            configuration: captured.into_bytes(),
            buffers: Buffers {
                request_value_capacity_bytes: 0,
                capture_payload_bytes: buffer_receipt.stream_payload_bytes,
                configuration_capacity_bytes: buffer_receipt.envelope_capacity_bytes,
            },
            parent_configuration_capacity_bytes: 0,
        };
        if !plugin
            .retire_restart_cohort(expected_native)
            .map_err(|error| unavailable(format!("VST3 cohort retirement failed: {error}")))?
        {
            return Ok(Outcome::StillPending(self.native_configuration_cohort()));
        }
        self.configuration_phase = Phase::issued(&candidate);
        Ok(Outcome::ReadyForInstall(candidate))
    }
    fn stage_native_configuration_seed(
        &mut self,
        seed: super::NativeConfigurationOwnedSeed,
    ) -> Result<super::NativeConfigurationSeedOutcome, AudioProcessorHostError> {
        let Some(current) = self.plugin.as_ref().and_then(Plugin::pending_restart_cohort) else {
            return Ok(super::NativeConfigurationSeedOutcome::Refused(
                super::NativeConfigurationSeedRefusal::UnsupportedAdapter,
            ));
        };
        if current.pending()
            || current.serial() != seed.ticket.serviced_cohort.serial
            || self.configuration_phase.seed_ticket() != Some(seed.ticket)
        {
            return Ok(super::NativeConfigurationSeedOutcome::Refused(
                super::NativeConfigurationSeedRefusal::StaleTicket,
            ));
        }
        Ok(self.configuration_phase.stage_seed(seed))
    }

    fn commit_native_configuration_seed(
        &mut self,
        ticket: super::NativeConfigurationSeedTicket,
    ) -> Result<super::NativeConfigurationSeedOutcome, AudioProcessorHostError> {
        if let Err(refusal) = self.configuration_phase.seed_receipt(ticket) {
            return Ok(super::NativeConfigurationSeedOutcome::Refused(refusal));
        }
        let plugin = self
            .plugin
            .as_mut()
            .ok_or_else(|| unavailable("VST3 retained processor is closed"))?;
        if !plugin
            .claim_retired_restart_serial(ticket.serviced_cohort.serial)
            .map_err(|error| {
                unavailable(format!("VST3 seed request serial claim failed: {error}"))
            })?
        {
            return Ok(super::NativeConfigurationSeedOutcome::Refused(
                super::NativeConfigurationSeedRefusal::StaleTicket,
            ));
        }
        let (seed, receipt) = match self.configuration_phase.take_seed(ticket) {
            Ok(value) => value,
            Err(refusal) => return Ok(super::NativeConfigurationSeedOutcome::Refused(refusal)),
        };
        // Future Enter destroys/reloads the component: its actual canonical
        // payload must contain the new seed before accepting a fresh generation.
        let previous = self.payload.state.replace(seed);
        self.contract = ticket.observed_contract;
        self.last_end = None;
        self.configuration_phase =
            super::native_configuration_control::NativeConfigurationPhase::Admitted;
        drop(previous);
        Ok(super::NativeConfigurationSeedOutcome::Accepted(receipt))
    }

    fn pending_reprepare(&self) -> Option<mondrian_core::NativeAudioReprepareRequest> {
        let flags = self.plugin.as_ref()?.pending_restart_flags().bits() as u32;
        (flags != 0)
            .then_some(mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags })
            .or(self.configuration_phase.blocked_request())
    }

    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.contract
    }

    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        self.auxiliary_inputs.clone()
    }

    fn auxiliary_input_names(&self) -> Vec<crate::AudioProcessorInputName> {
        self.input_names.clone()
    }

    fn prepare_native_author_state_entry(
        &mut self,
        request: crate::NativeAudioAuthorStateEntryRequest<'_>,
    ) -> Result<crate::NativeAudioAuthorStateEntryOutcome, AudioProcessorHostError> {
        self.prepare_author_state_entry(request)
    }

    fn enter_state(&mut self, start_sample: i64) -> Result<(), AudioProcessorHostError> {
        if self.enter_prepared_author_state(start_sample)? {
            self.native_binding = None;
            return Ok(());
        }
        let initial_ready = self.service_first_parameter_values();
        if let Some(request) = self.pending_reprepare() {
            return Err(AudioProcessorHostError::ReprepareRequired(request));
        }
        if !initial_ready && self.initial_signal_phase == InitialSignalPhase::Unserviced {
            return Err(AudioProcessorHostError::StateEntry(
                "VST3 initial parameter source was refused".to_owned(),
            ));
        }
        if self.initial_signal_phase == InitialSignalPhase::AwaitingFirstSignal {
            // Anchor the initialized object without Stop, reset, reload, state
            // capture or a synthetic Process. Its source and HWND stay owned.
            self.native_binding = None;
            self.initial_signal_phase = InitialSignalPhase::Entered;
            self.last_end = Some(start_sample);
            return Ok(());
        }
        self.require_delivered_parameter_input()?;
        // Native close/retirement may partially fail; the old author lease must
        // already be revoked once this non-preflight work starts.
        self.native_binding = None;
        self.close_native_editor_owner()?;
        self.prepared_author_entry = None;
        self.rebuild_native_state()?;
        if let Some(request) = self.pending_reprepare() {
            return Err(AudioProcessorHostError::ReprepareRequired(request));
        }
        self.last_end = Some(start_sample);
        Ok(())
    }

    fn process(
        &mut self,
        block: IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        let native = self.plugin.as_mut().ok_or_else(|| {
            AudioProcessorHostError::Process("VST3 processor is closed".to_owned())
        })?;
        VstSignalFields {
            last_parameter_values: &mut self.last_parameter_values,
            configuration_phase: &self.configuration_phase,
            payload: &self.payload,
            render: self.render,
            buffers: &mut self.buffers,
            auxiliary_inputs: &self.auxiliary_inputs,
            last_end: &mut self.last_end,
        }
        .process(native, block)
    }
}

/// Borrowed signal state of the same worker, shared by inline and scoped DSP.
/// Parameter domains, auxiliary wiring and sample continuity are interpreted
/// exactly once here; native operations only differ in their thread capability.
struct VstSignalFields<'a> {
    last_parameter_values: &'a mut Vec<Option<f64>>,
    configuration_phase: &'a super::native_configuration_control::NativeConfigurationPhase,
    payload: &'a Vst3Payload,
    render: crate::AudioRenderContract,
    buffers: &'a mut BusAudioBuffers,
    auxiliary_inputs: &'a AudioProcessorAuxiliaryInputContract,
    last_end: &'a mut Option<i64>,
}

trait VstNativeSignal {
    fn deliver_parameter_at(
        &mut self,
        id: u32,
        value: f64,
        sample_offset: i32,
    ) -> vst3_host::error::Result<()>;
    fn deliver_timeline(
        &mut self,
        context: vst3_host::audio::ProcessTimelineContext,
    ) -> vst3_host::error::Result<()>;
    fn process_buses(&mut self, buffers: &mut BusAudioBuffers) -> vst3_host::error::Result<()>;
}

impl VstNativeSignal for Plugin {
    fn deliver_parameter_at(
        &mut self,
        id: u32,
        value: f64,
        sample_offset: i32,
    ) -> vst3_host::error::Result<()> {
        self.set_parameter_at(id, value, sample_offset)
    }
    fn deliver_timeline(
        &mut self,
        context: vst3_host::audio::ProcessTimelineContext,
    ) -> vst3_host::error::Result<()> {
        self.set_process_timeline_context(context)
    }
    fn process_buses(&mut self, buffers: &mut BusAudioBuffers) -> vst3_host::error::Result<()> {
        self.process_bus_audio(buffers)
    }
}

impl VstNativeSignal for vst3_host::plugin::PluginAudioProcessing<'_> {
    fn deliver_parameter_at(
        &mut self,
        id: u32,
        value: f64,
        sample_offset: i32,
    ) -> vst3_host::error::Result<()> {
        self.queue_parameter_at(id, value, sample_offset)
    }
    fn deliver_timeline(
        &mut self,
        context: vst3_host::audio::ProcessTimelineContext,
    ) -> vst3_host::error::Result<()> {
        self.set_process_timeline_context(context)
    }
    fn process_buses(&mut self, buffers: &mut BusAudioBuffers) -> vst3_host::error::Result<()> {
        self.process_bus_audio(buffers)
    }
}

impl VstSignalFields<'_> {
    fn process(
        &mut self,
        native: &mut dyn VstNativeSignal,
        mut block: IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        if let Some(request) = self.configuration_phase.blocked_request() {
            return Err(AudioProcessorHostError::ReprepareRequired(request));
        }
        let frames = block.frames();
        let end_sample = block.start_sample().checked_add(frames as i64).ok_or_else(|| {
            AudioProcessorHostError::Process("VST3 sample position overflowed".to_owned())
        })?;
        let channels = self.render.channel_count();
        if *self.last_end != Some(block.start_sample())
            || frames == 0
            || frames > self.render.max_block_frames
            || block.main_interleaved().len() != frames * channels
            || block.parameter_lane_count() != self.payload.parameter_ids.len()
        {
            return Err(AudioProcessorHostError::Process(
                "VST3 block violates continuity, extent, or parameter lanes".to_owned(),
            ));
        }
        let mut event_count = 0;
        for (lane, native_id) in self.payload.parameter_ids.iter().copied().enumerate() {
            let descriptor = self
                .payload
                .parameters
                .iter()
                .find(|descriptor| descriptor.id == native_id)
                .ok_or_else(|| {
                    AudioProcessorHostError::Process(
                        "VST3 parameter descriptor is missing".to_owned(),
                    )
                })?;
            let events = block.parameter_events(lane).ok_or_else(|| {
                AudioProcessorHostError::Process("VST3 parameter lane is missing".to_owned())
            })?;
            for event in events {
                event_count += 1;
                if event_count > MAX_EVENTS_PER_BLOCK {
                    return Err(AudioProcessorHostError::Process(
                        "VST3 parameter event capacity exceeded".to_owned(),
                    ));
                }
                let value = descriptor.normalized_from_author_scalar(event.value)?;
                if event.sample_offset as usize >= frames
                    || !value.is_finite()
                    || !(0.0..=1.0).contains(&value)
                {
                    return Err(AudioProcessorHostError::Process(
                        "VST3 parameter event is out of bounds".to_owned(),
                    ));
                }
                native
                    .deliver_parameter_at(native_id, value, event.sample_offset as i32)
                    .map_err(|error| {
                        AudioProcessorHostError::Process(format!(
                            "VST3 parameter delivery failed: {error}"
                        ))
                    })?;
            }
        }
        for bus in self.buffers.inputs.iter_mut().chain(&mut self.buffers.outputs) {
            for channel in &mut bus.channels {
                channel.resize(frames, 0.0);
                channel.fill(0.0);
            }
        }
        for (index, bus) in self.auxiliary_inputs.buses.iter().enumerate() {
            let samples = block.auxiliary_input(&bus.bus_key).ok_or_else(|| {
                AudioProcessorHostError::Process("VST3 auxiliary PCM is missing".to_owned())
            })?;
            if samples.len() != frames * channels {
                return Err(AudioProcessorHostError::Process(
                    "VST3 auxiliary extent changed".to_owned(),
                ));
            }
            for (frame, values) in samples.chunks_exact(channels).enumerate() {
                for (channel, sample) in values.iter().copied().enumerate() {
                    self.buffers.inputs[index + 1].channels[channel][frame] = sample;
                }
            }
        }
        for (frame, samples) in block.main_interleaved().chunks_exact(channels).enumerate() {
            for (channel, sample) in samples.iter().copied().enumerate() {
                self.buffers.inputs[0].channels[channel][frame] = sample;
            }
        }
        self.buffers.block_size = frames;
        native
            .deliver_timeline(vst3_host::audio::ProcessTimelineContext {
                sample_position: block.start_sample(),
                sample_rate: block.sample_rate(),
            })
            .map_err(|error| {
                AudioProcessorHostError::Process(format!("VST3 timeline delivery failed: {error}"))
            })?;
        native.process_buses(self.buffers).map_err(|error| {
            AudioProcessorHostError::Process(format!("VST3 processing failed: {error}"))
        })?;
        for (frame, samples) in block.main_interleaved().chunks_exact_mut(channels).enumerate() {
            for (channel, sample) in samples.iter_mut().enumerate() {
                *sample = self.buffers.outputs[0].channels[channel][frame];
            }
        }
        for lane in 0..self.payload.parameter_ids.len() {
            if let Some(events) = block.parameter_events(lane) {
                for event in events {
                    self.last_parameter_values[lane] = Some(event.value);
                }
            }
        }
        *self.last_end = Some(end_sample);
        Ok(())
    }
}

fn validate_loaded(
    plugin: &mut Plugin,
    registration: &Vst3PluginRegistration,
    channel_layout: AudioChannelLayout,
) -> Result<Vec<crate::AudioProcessorInputName>, AudioProcessorHostError> {
    if plugin.info().uid != registration.class_id
        || registration
            .vendor
            .as_deref()
            .is_some_and(|vendor| plugin.info().vendor != vendor)
    {
        return Err(invalid("VST3 class identity changed after preparation"));
    }
    let metadata = prepare_audio_inputs(plugin, channel_layout)?;
    if metadata.contract != registration.auxiliary_inputs {
        return Err(invalid("VST3 auxiliary bus contract changed since probe"));
    }
    let tail = match plugin.tail_samples() {
        0 => AudioProcessorTail::None,
        u32::MAX => AudioProcessorTail::Infinite,
        frames => AudioProcessorTail::Finite(frames as usize),
    };
    if plugin.latency_samples() as usize
        != registration.execution_contract.algorithmic_latency_frames()
        || tail != registration.execution_contract.tail()
    {
        return Err(invalid("VST3 latency or tail changed since contract probe"));
    }
    let parameters = parameter_descriptors(plugin)?;
    if parameters != registration.parameters {
        return Err(invalid(
            "VST3 parameter metadata changed since contract probe",
        ));
    }
    Ok(metadata.names)
}

/// Read already activated bus facts without mutating native bus activation.
fn observe_active_audio_inputs(
    plugin: &mut Plugin,
    channel_layout: AudioChannelLayout,
) -> Result<AudioProcessorAuxiliaryInputContract, AudioProcessorHostError> {
    let metadata = plugin
        .native_audio_bus_info()
        .map_err(|error| invalid(format!("VST3 active bus metadata failed: {error}")))?;
    let layout = plugin
        .audio_bus_layout()
        .map_err(|error| invalid(format!("VST3 active bus layout failed: {error}")))?;
    let arrangements = plugin
        .bus_arrangements()
        .map_err(|error| invalid(format!("VST3 active arrangements failed: {error}")))?;
    let contract = audio_inputs_from_metadata(&metadata, &layout, &arrangements, channel_layout)?;
    if layout.inputs.iter().chain(&layout.outputs).any(|bus| !bus.active) {
        return Err(invalid(
            "VST3 retained activation left an admitted bus inactive",
        ));
    }
    Ok(contract)
}

/// Read roles from the actual restored component and activate admitted audio ports.
pub(super) fn prepare_audio_inputs(
    plugin: &mut Plugin,
    channel_layout: AudioChannelLayout,
) -> Result<super::NativeAudioInputMetadata, AudioProcessorHostError> {
    let metadata = plugin
        .native_audio_bus_info()
        .map_err(|error| invalid(format!("VST3 native bus query failed: {error}")))?;
    let layout = plugin
        .audio_bus_layout()
        .map_err(|error| invalid(format!("VST3 prepared bus query failed: {error}")))?;
    let arrangements = plugin
        .bus_arrangements()
        .map_err(|error| invalid(format!("VST3 speaker arrangement query failed: {error}")))?;
    let expected = audio_inputs_from_metadata(&metadata, &layout, &arrangements, channel_layout)?;
    for (direction, buses) in [
        (BusDirection::Input, &layout.inputs),
        (BusDirection::Output, &layout.outputs),
    ] {
        for (index, bus) in buses.iter().enumerate() {
            if !bus.active {
                plugin.set_bus_active(MediaType::Audio, direction, index as i32, true).map_err(
                    |error| invalid(format!("VST3 audio bus activation failed: {error}")),
                )?;
            }
        }
    }
    let active = plugin
        .audio_bus_layout()
        .map_err(|error| invalid(format!("VST3 activated bus query failed: {error}")))?;
    let current_metadata = plugin
        .native_audio_bus_info()
        .map_err(|error| invalid(format!("VST3 activated metadata query failed: {error}")))?;
    let current_arrangements = plugin
        .bus_arrangements()
        .map_err(|error| invalid(format!("VST3 activated arrangement query failed: {error}")))?;
    if active.inputs.iter().chain(&active.outputs).any(|bus| !bus.active)
        || audio_inputs_from_metadata(
            &current_metadata,
            &active,
            &current_arrangements,
            channel_layout,
        )? != expected
    {
        return Err(invalid("VST3 bus contract changed during activation"));
    }
    let names = current_metadata
        .inputs
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(index, bus)| {
            crate::AudioProcessorInputName::capture(format!("vst3.audio-input.{index}"), &bus.name)
        })
        .collect();
    Ok(super::NativeAudioInputMetadata { contract: expected, names })
}

fn audio_inputs_from_metadata(
    metadata: &NativeAudioBusInfoSet,
    layout: &AudioBusLayout,
    arrangements: &BusArrangements,
    channel_layout: AudioChannelLayout,
) -> Result<AudioProcessorAuxiliaryInputContract, AudioProcessorHostError> {
    let speaker_layout = match channel_layout {
        AudioChannelLayout::Mono => SpeakerArrangement::MONO,
        AudioChannelLayout::Stereo => SpeakerArrangement::STEREO,
        _ => return Err(invalid("VST3 audio channel layout is unsupported")),
    };
    if metadata.inputs.is_empty()
        || metadata.inputs.len() > 32
        || metadata.outputs.len() != 1
        || layout.inputs.len() != metadata.inputs.len()
        || layout.outputs.len() != metadata.outputs.len()
        || arrangements.inputs.len() != metadata.inputs.len()
        || arrangements.outputs.len() != metadata.outputs.len()
    {
        return Err(invalid("VST3 requires bounded inputs and one main output"));
    }
    for (native, prepared, speakers) in [
        (&metadata.inputs, &layout.inputs, &arrangements.inputs),
        (&metadata.outputs, &layout.outputs, &arrangements.outputs),
    ] {
        for (index, ((bus, prepared), arrangement)) in
            native.iter().zip(prepared).zip(speakers).enumerate()
        {
            let role = if index == 0 {
                AudioBusRole::Main
            } else {
                AudioBusRole::Auxiliary
            };
            if bus.role != role
                || bus.channel_count != channel_layout.channel_count()
                || prepared.channel_count != bus.channel_count
                || *arrangement != speaker_layout
            {
                return Err(invalid(
                    "VST3 native bus role, channel count or speaker arrangement is invalid",
                ));
            }
        }
    }
    let result = AudioProcessorAuxiliaryInputContract {
        buses: (1..metadata.inputs.len())
            .map(|index| AudioProcessorAuxiliaryInputBusContract {
                bus_key: format!("vst3.audio-input.{index}"),
                channel_layout,
            })
            .collect(),
    };
    result.validate(channel_layout)?;
    Ok(result)
}

pub(super) fn parameter_descriptors(
    plugin: &Plugin,
) -> Result<Vec<Vst3ParameterDescriptor>, AudioProcessorHostError> {
    let parameters = plugin
        .get_parameters()
        .map_err(|error| invalid(format!("VST3 parameter query failed: {error}")))?
        .into_iter()
        .map(|parameter| Vst3ParameterDescriptor {
            id: parameter.id,
            name: parameter.name,
            default_normalized: parameter.default,
            step_count: parameter.step_count,
            can_automate: parameter.can_automate,
            read_only: parameter.is_read_only,
            flags: parameter.flags,
        })
        .collect::<Vec<_>>();
    for parameter in &parameters {
        validate_parameter(parameter)?;
    }
    Ok(parameters)
}

fn validate_registration(
    registration: &Vst3PluginRegistration,
) -> Result<(), AudioProcessorHostError> {
    if registration.class_id.len() != 32
        || !registration.class_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !registration.binary_path.is_absolute()
        || registration.binary_sha256 == [0; 32]
        || !registration.execution_contract.requires_state_entry()
        || registration.parameters.len() > MAX_PARAMETERS
    {
        return Err(invalid(
            "VST3 class, binary, or execution contract is invalid",
        ));
    }
    if registration.auxiliary_inputs.buses.len() > 31
        || registration
            .auxiliary_inputs
            .buses
            .iter()
            .enumerate()
            .any(|(index, bus)| bus.bus_key != format!("vst3.audio-input.{}", index + 1))
    {
        return Err(invalid(
            "VST3 auxiliary bus indices must retain native order",
        ));
    }
    let mut ids = BTreeSet::new();
    for parameter in &registration.parameters {
        validate_parameter(parameter)?;
        if !ids.insert(parameter.id) {
            return Err(invalid("VST3 parameter IDs must be unique"));
        }
    }
    Ok(())
}

fn validate_parameter(parameter: &Vst3ParameterDescriptor) -> Result<(), AudioProcessorHostError> {
    if parameter.name.is_empty()
        || parameter.name.len() > 256
        || parameter.name.chars().any(char::is_control)
        || !parameter.default_normalized.is_finite()
        || !(0.0..=1.0).contains(&parameter.default_normalized)
        || parameter.step_count < 0
    {
        return Err(invalid("VST3 parameter metadata is invalid"));
    }
    Ok(())
}

pub(super) fn fingerprint(path: &Path) -> Result<[u8; 32], AudioProcessorHostError> {
    let mut file = File::open(path)
        .map_err(|error| unavailable(format!("VST3 binary is unavailable: {error}")))?;
    let expected = file
        .metadata()
        .map_err(|error| unavailable(format!("VST3 binary metadata failed: {error}")))?;
    if !expected.is_file() || expected.len() > MAX_BINARY_BYTES {
        return Err(invalid("VST3 binary must be a bounded regular file"));
    }
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| unavailable(format!("VST3 binary read failed: {error}")))?;
        if count == 0 {
            break;
        }
        bytes = bytes.saturating_add(count as u64);
        if bytes > MAX_BINARY_BYTES {
            return Err(invalid("VST3 binary exceeds the fingerprint limit"));
        }
        hash.update(&buffer[..count]);
    }
    if bytes != expected.len() {
        return Err(unavailable("VST3 binary changed during fingerprinting"));
    }
    Ok(hash.finalize().into())
}

fn invalid(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.into())
}

fn unavailable(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::Unavailable(detail.into())
}

#[cfg(target_os = "windows")]
fn native_editor_removal_failed(error: vst3_host::error::Error) -> AudioProcessorHostError {
    AudioProcessorHostError::WorkerFailed {
        operation: "native editor owner removal",
        detail: error.to_string(),
    }
}

/// Same native controller/view storage remains confined to its real main.
/// This role cannot reach the DSP processor or replacement/configuration APIs.
#[cfg(target_os = "windows")]
struct VstMainRole<'a> {
    editor: vst3_host::plugin::PluginEditorControl<'a>,
    native_main: &'a std::rc::Rc<vst3_host::window::WindowsEditorMainThread>,
    native_parent: &'a mut Option<vst3_host::window::WindowsPluginEditorParent>,
    configuration_phase: &'a super::native_configuration_control::NativeConfigurationPhase,
    native_binding: &'a Option<crate::NativeAudioObjectBindReceipt>,
    native_author_delivery: &'a Option<native_author::PendingDelivery>,
    prepared_author_entry: &'a Option<author_state_entry::PreparedAuthorEntry>,
}

#[cfg(target_os = "windows")]
impl super::IsolatedAudioProcessorMainRole for VstMainRole<'_> {
    fn allocation_layouts(&self) -> Option<[crate::NativeHostBufferLayout; 2]> {
        Some([
            native_layout(std::alloc::Layout::for_value(self)),
            native_layout(self.editor.allocation_layout()),
        ])
    }
    fn service_native_main_thread(&mut self) -> Result<(), AudioProcessorHostError> {
        let step = if let Some(parent) = self.native_parent.as_mut() {
            parent.service(&mut self.editor)
        } else {
            let step = self.native_main.dispatch_messages();
            self.editor
                .service_controller()
                .and_then(|_| self.editor.service_run_loop())
                .map_err(|error| {
                    unavailable(format!(
                        "VST3 actual controller main service failed: {error}"
                    ))
                })?;
            step
        }
        .map_err(|error| {
            unavailable(format!(
                "VST3 actual native message service failed: {error}"
            ))
        })?;
        if let vst3_host::window::WindowsMessageStep::Quit { exit_code } = step {
            return Err(AudioProcessorHostError::WorkerFailed {
                operation: "native main message service",
                detail: format!("actual native WM_QUIT: {exit_code}"),
            });
        }
        Ok(())
    }

    fn finish_processing_scope(&mut self) -> Result<(), AudioProcessorHostError> {
        // Flush processor-to-controller display/DataExchange updates on main
        // only after DSP join. No synthetic author End, frame removal or native
        // state/processor replacement is inferred from temporary role lifetime.
        self.editor
            .service_controller()
            .and_then(|_| self.editor.service_run_loop())
            .map_err(|error| {
                unavailable(format!(
                    "VST3 joined controller main finish failed: {error}"
                ))
            })
    }

    fn native_configuration_cohort(
        &self,
    ) -> Result<Option<super::NativeRequestCohort>, AudioProcessorHostError> {
        let cohort = self.editor.pending_restart_cohort().map_err(|error| {
            unavailable(format!("VST3 main cohort observation failed: {error}"))
        })?;
        Ok(cohort
            .filter(|cohort| cohort.pending())
            .map(|cohort| super::NativeRequestCohort {
                serial: cohort.serial(),
                request: mondrian_core::NativeAudioReprepareRequest::Vst3Restart {
                    flags: cohort.flags().bits() as u32,
                },
            }))
    }

    fn pending_reprepare(
        &self,
    ) -> Result<Option<mondrian_core::NativeAudioReprepareRequest>, AudioProcessorHostError> {
        let cohort = self.native_configuration_cohort()?;
        Ok(cohort
            .map(|cohort| cohort.request)
            .or(self.configuration_phase.blocked_request()))
    }

    fn preserve_native_observations(
        &self,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        let owner = self.editor.attachment_owner().map_err(|error| {
            unavailable(format!(
                "VST3 main native object observation failed: {error}"
            ))
        })?;
        let pid = std::num::NonZeroU32::new(std::process::id())
            .ok_or_else(|| unavailable("actual native main PID is zero"))?;
        let author = Some(crate::NativeAudioAuthorObservation {
            source: native_author::status(self.editor.editor_event_source_status().map_err(
                |error| {
                    unavailable(format!(
                        "VST3 main author source observation failed: {error}"
                    ))
                },
            )?)?,
            delivery: if self
                .prepared_author_entry
                .as_ref()
                .is_some_and(|entry| entry.awaiting_ack())
            {
                crate::NativeAudioAuthorDeliveryState::Refused(
                    crate::NativeAudioAuthorRefusal::AwaitingAcceptance,
                )
            } else {
                native_author::pending_projection(self.native_author_delivery.as_ref())
            },
        });
        // Attachment cleanup is a full-owner transition after this borrow joins.
        // Live role service never closes/replaces the canonical native object.
        Ok(crate::NativeAudioObservations {
            object: Some(crate::NativeAudioObjectObservation::new(
                pid,
                owner.serial(),
            )),
            author,
            ..Default::default()
        })
    }

    fn native_author_binding_matches(
        &self,
        binding: crate::NativeAudioObjectBindRequest,
    ) -> Result<bool, AudioProcessorHostError> {
        let owner = self.editor.attachment_owner().map_err(|error| {
            unavailable(format!("VST3 author object observation failed: {error}"))
        })?;
        let pid = std::process::id();
        Ok(self.native_binding.as_ref().is_some_and(|receipt| {
            receipt.binding() == binding.binding && receipt.object() == binding.expected_object
        }) && binding.expected_object.process_id().get() == pid
            && binding.expected_object.object_serial() == owner.serial())
    }

    fn native_editor_close_requested(&self) -> Result<bool, AudioProcessorHostError> {
        self.native_parent.as_ref().map_or(Ok(false), |parent| {
            parent.closed_by_user().map_err(|error| {
                unavailable(format!(
                    "VST3 same native parent close observation failed: {error}"
                ))
            })
        })
    }
}

/// The one loaded processor and canonical signal fields, exclusively borrowed
/// into the scoped DSP thread. No controller/window/main storage is reachable.
#[cfg(target_os = "windows")]
struct VstDspRole<'a> {
    native: vst3_host::plugin::PluginAudioProcessing<'a>,
    signal: VstSignalFields<'a>,
}

#[cfg(target_os = "windows")]
impl super::IsolatedAudioProcessorProcessingRole for VstDspRole<'_> {
    fn allocation_layouts(&self) -> Option<[crate::NativeHostBufferLayout; 2]> {
        Some([
            native_layout(std::alloc::Layout::for_value(self)),
            native_layout(self.native.allocation_layout()),
        ])
    }
    fn process(
        &mut self,
        block: IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        self.signal.process(&mut self.native, block)
    }
    fn flush_native_author_parameters(
        &mut self,
    ) -> Result<crate::NativeAudioAuthorParameterFlushOutcome, AudioProcessorHostError> {
        if !self.native.queue_unconsumed_editor_values().map_err(|error| {
            unavailable(format!("VST3 same-source parameter replay failed: {error}"))
        })? {
            return Ok(crate::NativeAudioAuthorParameterFlushOutcome::Refused(
                crate::NativeAudioAuthorRefusal::InvalidSource,
            ));
        }
        // Keep bus metadata and actual capacities, but expose no PCM samples.
        // The SDK hides audio I/O for its normative zero-sample control path.
        // Neither canonical sample continuity nor timeline context is changed.
        self.signal.buffers.block_size = 0;
        for bus in self.signal.buffers.inputs.iter_mut().chain(&mut self.signal.buffers.outputs) {
            for channel in &mut bus.channels {
                channel.clear();
            }
        }
        self.native.process_bus_audio(self.signal.buffers).map_err(|error| {
            unavailable(format!("VST3 zero-sample parameter flush failed: {error}"))
        })?;
        Ok(crate::NativeAudioAuthorParameterFlushOutcome::Flushed)
    }
}

#[cfg(target_os = "windows")]
fn native_layout(layout: std::alloc::Layout) -> crate::NativeHostBufferLayout {
    crate::NativeHostBufferLayout { bytes: layout.size(), alignment: layout.align() }
}

pub(super) fn native_worker_lane_profile_for_parameters(
    parameters: &[Vst3ParameterDescriptor],
) -> Result<Option<crate::NativeWorkerLaneResourceProfile>, AudioProcessorHostError> {
    if parameters.len() > MAX_PARAMETERS {
        return Err(invalid("VST3 parameter count exceeds initial bound"));
    }
    let count = parameters.iter().filter(|parameter| !parameter.read_only).count();
    let storage =
        vst3_host::initial_parameter_input::InitialParameterStorage::for_parameter_count(count)
            .map_err(|error| invalid(format!("VST3 input storage request is invalid: {error}")))?;
    native_worker_lane_profile()?
        .map(|profile| {
            profile
                .with_processor_input_storage(storage.vector_bytes, storage.com_payload_bytes)
                .map_err(AudioProcessorHostError::from)
        })
        .transpose()
}

pub(super) fn native_worker_lane_profile(
) -> Result<Option<crate::NativeWorkerLaneResourceProfile>, AudioProcessorHostError> {
    #[cfg(target_os = "windows")]
    {
        let [sdk_main, sdk_dsp] =
            vst3_host::plugin::PluginEditorProcessingSplit::allocation_layouts();
        let owners = crate::NativeWorkerLaneOwnedLayouts {
            reader_slots: native_layout(std::alloc::Layout::new::<
                super::native_worker_ingress::NativeReaderSlots,
            >()),
            processing_slots: native_layout(std::alloc::Layout::new::<
                super::native_worker_processing::NativeProcessingSlots,
            >()),
            // Separately identified logical inline obligations. Compiler stack
            // layout, thread TLS/bookkeeping and committed OS pages are frontier.
            main_inline: native_layout(std::alloc::Layout::new::<(
                super::native_worker_ingress::NativeCommandIngress<'static>,
                super::worker::WorkerProcessContext<'static>,
                super::native_worker_processing::NativeDspDispatch<'static>,
                crate::NativeAudioObservations,
                vst3_host::controller_parameters::ControllerParameterQueryFrame,
                initial_parameter_values::InitialParameterFrame,
            )>()),
            runtime_main_role: native_layout(std::alloc::Layout::new::<VstMainRole<'static>>()),
            runtime_dsp_role: native_layout(std::alloc::Layout::new::<VstDspRole<'static>>()),
            sdk_main_role: native_layout(sdk_main),
            sdk_dsp_role: native_layout(sdk_dsp),
        };
        let byte_limit = crate::NativeHostBufferLayout {
            bytes: super::protocol::MAX_CONTROL_FRAME_BYTES,
            alignment: 1,
        };
        let control = crate::NativeWorkerControlOverlapLayout {
            decoded_input: crate::NativeHostBufferLayout {
                bytes: byte_limit.bytes,
                alignment: align_of::<f64>(),
            },
            reply_storage: byte_limit,
            staged_seed: byte_limit,
        };
        Ok(Some(
            crate::NativeWorkerLaneResourceProfile::scoped_main_dsp(owners, control)
                .map_err(AudioProcessorHostError::from)?,
        ))
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(None)
    }
}

impl Vst3Worker {
    fn require_delivered_parameter_input(&self) -> Result<(), AudioProcessorHostError> {
        if self
            .plugin
            .as_ref()
            .ok_or_else(|| unavailable("VST3 retained processor is closed"))?
            .pending_processor_parameter_input()
            .map_err(|error| {
                unavailable(format!(
                    "VST3 state-entry input observation failed: {error}"
                ))
            })?
        {
            return Err(AudioProcessorHostError::StateEntry(
                "VST3 parameter delivery is still pending".to_owned(),
            ));
        }
        Ok(())
    }

    /// Sole native reload used by ordinary Enter and explicit author preparation.
    fn rebuild_native_state(&mut self) -> Result<(), AudioProcessorHostError> {
        self.require_delivered_parameter_input()?;
        // Pure refusal above preserves the author lease. Native retirement
        // starts here, so even a subsequent partial failure cannot retain it.
        self.native_binding = None;
        self.initial_signal_phase = InitialSignalPhase::Entered;
        self.last_parameter_values.fill(None);
        let mut previous = self.plugin.take().ok_or_else(|| {
            AudioProcessorHostError::StateEntry("VST3 processor is closed".to_owned())
        })?;
        let previous_owner = previous.native_editor_attachment_owner();
        previous.stop_processing().map_err(|error| {
            AudioProcessorHostError::StateEntry(format!("VST3 stop failed: {error}"))
        })?;
        drop(previous);
        if fingerprint(&self.payload.binary_path)? != self.payload.binary_sha256 {
            return Err(AudioProcessorHostError::StateEntry(
                "VST3 binary changed before continuity entry".to_owned(),
            ));
        }
        let mut fresh = self
            .host
            .load_plugin_class(&self.payload.binary_path, &self.payload.class_id)
            .map_err(|error| {
                AudioProcessorHostError::StateEntry(format!(
                    "VST3 state-entry load failed: {error}"
                ))
            })?;
        if fingerprint(&self.payload.binary_path)? != self.payload.binary_sha256 {
            return Err(AudioProcessorHostError::StateEntry(
                "VST3 binary changed while rebuilding the class".to_owned(),
            ));
        }
        if let Some(state) = self.payload.state.as_deref() {
            fresh.load_state(state).map_err(|error| {
                AudioProcessorHostError::StateEntry(format!(
                    "VST3 state-entry restore failed: {error}"
                ))
            })?;
        }
        let mode = match self.render.processing_mode {
            AudioProcessingMode::Realtime => ProcessMode::Realtime,
            AudioProcessingMode::Offline => ProcessMode::Offline,
        };
        fresh.set_process_mode(mode).map_err(|error| {
            AudioProcessorHostError::StateEntry(format!("VST3 state-entry mode failed: {error}"))
        })?;
        validate_loaded(
            &mut fresh,
            &Vst3PluginRegistration {
                class_id: self.payload.class_id.clone(),
                vendor: self.payload.vendor.clone(),
                binary_path: self.payload.binary_path.clone(),
                binary_sha256: self.payload.binary_sha256,
                execution_contract: self.contract,
                auxiliary_inputs: self.auxiliary_inputs.clone(),
                parameters: self.payload.parameters.clone(),
            },
            self.render.channel_layout,
        )?;
        fresh.start_processing().map_err(|error| {
            AudioProcessorHostError::StateEntry(format!("VST3 state-entry start failed: {error}"))
        })?;
        let fresh_owner = fresh.native_editor_attachment_owner();
        // Install before inspecting callbacks. Refused fresh source/cohorts stay
        // on this actual object rather than being dropped/reloaded to erase them.
        self.plugin = Some(fresh);
        if matches!((previous_owner, fresh_owner), (Some(old), Some(new))
            if new.serial() > old.serial())
            && !self.service_fresh_parameter_values()
        {
            return Err(AudioProcessorHostError::StateEntry(
                "VST3 fresh initial parameter source was refused".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod bus_contract_tests {
    use super::*;
    use vst3_host::audio::{AudioBusConfig, NativeAudioBusInfo};

    #[test]
    fn preparation_specific_input_request_is_validated_before_binary_access() {
        let parameter = Vst3ParameterDescriptor {
            id: 0,
            name: "Gain".to_owned(),
            default_normalized: 0.5,
            step_count: 0,
            can_automate: true,
            read_only: false,
            flags: 1,
        };
        let parameter_id = parameter.parameter_id().unwrap();
        let payload = Vst3Payload {
            class_id: "61921CF43A2B4A68AE5AE08241D85E22".to_owned(),
            vendor: None,
            binary_path: std::env::temp_dir().join("unopened-vst3-profile-control.dll"),
            binary_sha256: [1; 32],
            state: None,
            parameters: vec![parameter],
            parameter_ids: vec![0],
        };
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            native_lane: native_worker_lane_profile().unwrap(),
            payload: serde_json::to_vec(&payload).unwrap(),
            plugin_execution_contract: AudioProcessorExecutionContract::new(
                0,
                AudioProcessorTail::None,
                true,
                true,
                true,
                0,
            )
            .unwrap(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            note_input_contract: crate::AudioProcessorNoteInputContract::default(),
            parameter_ids: vec![parameter_id],
            render_contract: crate::AudioRenderContract {
                sample_rate: 48000,
                channel_layout: AudioChannelLayout::Stereo,
                max_block_frames: 8,
                processing_mode: AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes: usize::MAX,
                public_output_lookahead_budget_frames: usize::MAX,
                compensation_delay_scratch_budget_bytes: usize::MAX,
            },
        };
        let expected = Vst3AudioProcessorWorkerFactory
            .native_worker_lane_resources_for_preparation(&request)
            .unwrap();
        assert_eq!(
            expected,
            native_worker_lane_profile_for_parameters(&payload.parameters).unwrap()
        );
        #[cfg(target_os = "windows")]
        {
            assert_ne!(expected, request.native_lane);
            assert!(
                matches!(Vst3AudioProcessorWorkerFactory.prepare_native(request),
                Err(AudioProcessorHostError::InvalidContract(message))
                    if message == "VST3 input storage request differs from prepared metadata")
            );
        }
        assert!(native_worker_lane_profile_for_parameters(&vec![
            payload.parameters[0].clone();
            MAX_PARAMETERS + 1
        ])
        .is_err());
    }

    fn metadata() -> (NativeAudioBusInfoSet, AudioBusLayout, BusArrangements) {
        let main = NativeAudioBusInfo {
            role: AudioBusRole::Main,
            name: "Input".into(),
            channel_count: 2,
            default_active: true,
        };
        let aux = NativeAudioBusInfo {
            role: AudioBusRole::Auxiliary,
            name: "Input".into(),
            channel_count: 2,
            default_active: false,
        };
        (
            NativeAudioBusInfoSet {
                inputs: vec![main.clone(), aux.clone(), aux],
                outputs: vec![main],
            },
            AudioBusLayout {
                inputs: vec![
                    AudioBusConfig { channel_count: 2, active: true },
                    AudioBusConfig { channel_count: 2, active: false },
                    AudioBusConfig { channel_count: 2, active: false },
                ],
                outputs: vec![AudioBusConfig { channel_count: 2, active: true }],
            },
            BusArrangements {
                inputs: vec![SpeakerArrangement::STEREO; 3],
                outputs: vec![SpeakerArrangement::STEREO],
            },
        )
    }

    #[test]
    fn vst3_auxiliary_contract_uses_native_roles_indices_and_exact_speakers() {
        let (mut native, mut layout, mut arrangements) = metadata();
        let expected =
            audio_inputs_from_metadata(&native, &layout, &arrangements, AudioChannelLayout::Stereo)
                .expect("native sidechains");
        assert_eq!(
            expected.buses.iter().map(|bus| bus.bus_key.as_str()).collect::<Vec<_>>(),
            ["vst3.audio-input.1", "vst3.audio-input.2"]
        );
        native.inputs[1].name = "Localized renamed duplicate".into();
        layout.inputs[1].active = true;
        assert_eq!(
            audio_inputs_from_metadata(&native, &layout, &arrangements, AudioChannelLayout::Stereo)
                .expect("display and activation are not bus identity"),
            expected
        );
        native.inputs[1].role = AudioBusRole::Main;
        assert!(audio_inputs_from_metadata(
            &native,
            &layout,
            &arrangements,
            AudioChannelLayout::Stereo
        )
        .is_err());
        native.inputs[1].role = AudioBusRole::Auxiliary;
        arrangements.inputs[1] = SpeakerArrangement::STEREO_SURROUND;
        assert!(
            audio_inputs_from_metadata(&native, &layout, &arrangements, AudioChannelLayout::Stereo)
                .is_err(),
            "two channels do not imply L/R stereo"
        );
        arrangements.inputs[1] = SpeakerArrangement::STEREO;
        native.outputs[0].role = AudioBusRole::Auxiliary;
        assert!(audio_inputs_from_metadata(
            &native,
            &layout,
            &arrangements,
            AudioChannelLayout::Stereo
        )
        .is_err());
    }

    #[test]
    fn vst3_auxiliary_contract_rejects_missing_mismatched_and_unbounded_metadata() {
        let (native, layout, arrangements) = metadata();
        let assert_invalid = |native: &NativeAudioBusInfoSet,
                              layout: &AudioBusLayout,
                              arrangements: &BusArrangements| {
            assert!(audio_inputs_from_metadata(
                native,
                layout,
                arrangements,
                AudioChannelLayout::Stereo
            )
            .is_err());
        };
        let mut changed = native.clone();
        changed.inputs.clear();
        assert_invalid(&changed, &layout, &arrangements);
        changed = native.clone();
        changed.inputs[1].channel_count = 1;
        assert_invalid(&changed, &layout, &arrangements);
        changed = native.clone();
        changed.inputs.resize(33, native.inputs[1].clone());
        assert_invalid(&changed, &layout, &arrangements);
        let mut prepared = layout.clone();
        prepared.inputs[1].channel_count = 1;
        assert_invalid(&native, &prepared, &arrangements);
        prepared = layout.clone();
        prepared.inputs.pop();
        assert_invalid(&native, &prepared, &arrangements);
        let mut speakers = arrangements.clone();
        speakers.outputs.clear();
        assert_invalid(&native, &layout, &speakers);
    }
}
