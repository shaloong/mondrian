//! VST3 audio effects run only inside the supervised native worker.

mod author_state_entry;
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
mod controller_projection;
mod initial_parameter_values;
#[cfg(test)]
mod initial_parameter_values_tests;
mod native_author;
mod parameter_observations;
mod reprepare_payload;
pub(super) use reprepare_payload::{
    payload_with_reprepare_configuration, reprepare_codec_bound, reprepare_payload_output_bound,
};
#[cfg(all(test, target_os = "windows"))]
mod startup_metadata_diagnostics;

use super::vst3_processing_metadata::Vst3EventBusContractV1;
use super::{
    IsolatedAudioProcessorSpecResolver, IsolatedAudioProcessorWorker,
    IsolatedAudioProcessorWorkerBlock, IsolatedAudioProcessorWorkerFactory,
    IsolatedAudioProcessorWorkerPrepareRequest, IsolatedAudioProcessorWorkerSpec,
};
use crate::{
    AudioProcessingMode, AudioProcessorAuxiliaryInputBusContract,
    AudioProcessorAuxiliaryInputContract, AudioProcessorAuxiliaryOutputBusContract,
    AudioProcessorAuxiliaryOutputContract, AudioProcessorExecutionContract,
    AudioProcessorHostError, AudioProcessorPrepareRequest, AudioProcessorTail,
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
    AudioBusLayout, AudioBusObservationError, AudioBusObservationRef, AudioBusQueryFrame,
    AudioBusRole, BusArrangements, BusAudioBuffers, BusDirection, MediaType, NativeAudioBusInfoSet,
    SpeakerArrangement,
};
use vst3_host::plugin::{Plugin, ProcessMode};
use vst3_host::{PluginEvent, PluginEventData, Vst3Host};

/// Hidden application mode dedicated to the VST3 ABI.
pub const VST3_AUDIO_WORKER_ARGUMENT: &str = "--internal-vst3-audio-worker-v1";
const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;
pub(super) const MAX_STATE_BYTES: usize = 128 * 1024;
const PAYLOAD_VERSION: u32 = 6;

// VST3 has direction-local ordered bus indices. Keep these immutable keys in
// static storage: the child retains only the admitted count, not another Vec.
const AUDIO_OUTPUT_KEYS: [&str; 31] = [
    "vst3.audio-output.1",
    "vst3.audio-output.2",
    "vst3.audio-output.3",
    "vst3.audio-output.4",
    "vst3.audio-output.5",
    "vst3.audio-output.6",
    "vst3.audio-output.7",
    "vst3.audio-output.8",
    "vst3.audio-output.9",
    "vst3.audio-output.10",
    "vst3.audio-output.11",
    "vst3.audio-output.12",
    "vst3.audio-output.13",
    "vst3.audio-output.14",
    "vst3.audio-output.15",
    "vst3.audio-output.16",
    "vst3.audio-output.17",
    "vst3.audio-output.18",
    "vst3.audio-output.19",
    "vst3.audio-output.20",
    "vst3.audio-output.21",
    "vst3.audio-output.22",
    "vst3.audio-output.23",
    "vst3.audio-output.24",
    "vst3.audio-output.25",
    "vst3.audio-output.26",
    "vst3.audio-output.27",
    "vst3.audio-output.28",
    "vst3.audio-output.29",
    "vst3.audio-output.30",
    "vst3.audio-output.31",
];

pub(super) fn validate_audio_output_count(count: u8) -> Result<(), AudioProcessorHostError> {
    if !(1..=32).contains(&count) {
        return Err(invalid(
            "VST3 requires one primary output and at most 31 additional outputs",
        ));
    }
    Ok(())
}

fn audio_output_contract(
    count: u8,
    layout: AudioChannelLayout,
) -> AudioProcessorAuxiliaryOutputContract {
    AudioProcessorAuxiliaryOutputContract {
        buses: AUDIO_OUTPUT_KEYS
            .iter()
            .take(usize::from(count.saturating_sub(1)))
            .map(|key| AudioProcessorAuxiliaryOutputBusContract {
                bus_key: (*key).to_owned(),
                channel_layout: layout,
            })
            .collect(),
    }
}

fn validate_audio_output_contract(
    count: u8,
    contract: &AudioProcessorAuxiliaryOutputContract,
    layout: AudioChannelLayout,
) -> Result<(), AudioProcessorHostError> {
    validate_audio_output_count(count)?;
    contract.validate(layout)?;
    if contract.buses.len() != usize::from(count - 1)
        || contract
            .buses
            .iter()
            .zip(AUDIO_OUTPUT_KEYS)
            .any(|(bus, key)| bus.bus_key != key)
    {
        return Err(invalid(
            "VST3 output contract must retain the exact native bus order",
        ));
    }
    Ok(())
}

/// Actual product consumers, shared by discovery and every processing reload.
pub(super) fn product_plugin_interfaces() -> vst3_host::PluginInterfaceSupport {
    use vst3_host::{PluginInterface as Interface, PluginInterfaceSupport};
    let interfaces = PluginInterfaceSupport::none()
        .with(Interface::Component)
        .with(Interface::AudioProcessor)
        .with(Interface::EditController)
        .with(Interface::ConnectionPoint)
        .with(Interface::ProcessContextRequirements);
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    let interfaces = interfaces
        .with(Interface::PlugView)
        .with(Interface::PlugViewContentScaleSupport)
        .with(Interface::DataExchangeReceiver)
        .with(Interface::EditControllerHostEditing);
    interfaces
}
const MAX_PARAMETERS: usize = vst3_host::initial_parameter_input::MAX_INITIAL_PARAMETERS;
const MAX_EVENTS_PER_BLOCK: usize = 4096;

// This is only evidence for explicit canonical constants. An absent lane keeps
// its original native-state authority; cached host delivery never fills it in.
fn retained_constants_match_successful_delivery(
    expected: &[Option<f64>],
    delivered: &[Option<f64>],
) -> bool {
    expected.len() == delivered.len()
        && expected.iter().zip(delivered).all(|(expected, actual)| {
            expected.is_none_or(|value| value.is_finite() && *actual == Some(value))
        })
}

/// Admitted processing facts and a captured UI caption in VST3 coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Vst3ParameterDescriptor {
    /// VST3 definition-local `ParamID`.
    pub id: u32,
    /// Captured plugin display name, not a native processing identity.
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

/// Native main-audio-input shape, independent of the graph's working/output PCM.
/// Absent plus empty auxiliaries means zero native audio inputs. An absent Main
/// may also have 1..=31 ordered native Auxiliary inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vst3MainAudioInput {
    Absent,
    Present,
}

impl Vst3MainAudioInput {
    fn port_offset(self) -> usize {
        match self {
            Self::Absent => 0,
            Self::Present => 1,
        }
    }

    pub(super) fn validate_auxiliary(
        self,
        auxiliary: &AudioProcessorAuxiliaryInputContract,
    ) -> Result<(), AudioProcessorHostError> {
        if auxiliary.buses.len() > 31
            || auxiliary.buses.iter().enumerate().any(|(index, bus)| {
                !canonical_audio_input_key_matches(&bus.bus_key, index + self.port_offset())
            })
        {
            return Err(invalid(
                "VST3 auxiliary bus indices must retain native order",
            ));
        }
        Ok(())
    }

    fn validate_note_request(
        self,
        auxiliary: &AudioProcessorAuxiliaryInputContract,
        reference: Option<mondrian_core::MusicalTimeReference>,
        notes: &crate::AudioProcessorNoteInputContract,
    ) -> Result<(), AudioProcessorHostError> {
        if self == Self::Absent
            && auxiliary.buses.is_empty()
            && (reference.is_none() || notes.ports.is_empty())
        {
            return Err(unavailable(
                "VST3 zero audio input requires musical reference and requested Note input",
            ));
        }
        Ok(())
    }
}

// Input Main is optional and can only occupy native zero. Other inputs
// preserve their native Auxiliary indices, including zero when Main is absent.
fn native_main_audio_input(first: Option<AudioBusRole>) -> Vst3MainAudioInput {
    if first == Some(AudioBusRole::Main) {
        Vst3MainAudioInput::Present
    } else {
        Vst3MainAudioInput::Absent
    }
}

fn main_output_prefix(roles: impl Iterator<Item = AudioBusRole>) -> usize {
    roles.take_while(|role| *role == AudioBusRole::Main).count()
}

fn expected_audio_bus_role(
    direction: BusDirection,
    index: usize,
    main_input: Vst3MainAudioInput,
    main_outputs: usize,
) -> AudioBusRole {
    let main_count = match direction {
        BusDirection::Input => main_input.port_offset(),
        // Zero Main outputs fails at output zero, after earlier input errors.
        BusDirection::Output => main_outputs.max(1),
    };
    if index < main_count {
        AudioBusRole::Main
    } else {
        AudioBusRole::Auxiliary
    }
}

pub(super) fn validate_main_audio_output_count(
    total: u8,
    main: u8,
) -> Result<(), AudioProcessorHostError> {
    validate_audio_output_count(total)?;
    if main == 0 || main > total {
        return Err(invalid(
            "VST3 Main output prefix must be nonempty and within the output count",
        ));
    }
    Ok(())
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
    /// Main-input presence revalidated on the same restored/configured component.
    pub main_audio_input: Vst3MainAudioInput,
    /// Total native outputs. Native zero is the graph Main; remaining native
    /// Main/Auxiliary outputs use the existing additional-output keys.
    /// This count is immutable across preparation, state entry and retained service.
    pub audio_output_count: u8,
    /// Nonempty native Main prefix, followed only by native Auxiliary outputs.
    pub main_audio_output_count: u8,
    /// Ordered auxiliary input ports admitted after restoring the selected native state.
    pub auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    /// Complete parameter metadata, including non-editable parameters.
    pub parameters: Vec<Vst3ParameterDescriptor>,
    /// Expected native event rows; revalidated after each actual restore/configuration.
    pub event_bus_contract: Vst3EventBusContractV1,
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
    fn native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        // This registry only interprets already-admitted immutable metadata.
        mondrian_core::NativeChildSettlementRequirement::NotApplicable
    }

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
        let auxiliary_outputs = audio_output_contract(
            plugin.audio_output_count,
            request.render_contract().channel_layout,
        );
        validate_audio_output_contract(
            plugin.audio_output_count,
            &auxiliary_outputs,
            request.render_contract().channel_layout,
        )?;
        let note_input_contract = derived_note_input_contract(
            &plugin.event_bus_contract,
            request.musical_time_reference(),
            request.note_input_requested(),
        )?;
        plugin.main_audio_input.validate_note_request(
            &plugin.auxiliary_inputs,
            request.musical_time_reference(),
            &note_input_contract,
        )?;
        let native_lane = native_worker_lane_profile_for_preparation(
            &plugin.parameters,
            &note_input_contract,
            request.render_contract(),
            &plugin.auxiliary_inputs,
        )?;
        if !note_input_contract.ports.is_empty() && native_lane.is_none() {
            return Err(unavailable(
                "VST3 Note input requires the scoped native lane",
            ));
        }
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
        let state = request.opaque_state();
        if state.is_some_and(|state| state.len() > MAX_STATE_BYTES) {
            return Err(invalid("VST3 state exceeds the worker payload limit"));
        }
        let state = state.map(ToOwned::to_owned);
        let payload = super::encode_native_configuration_payload(&Vst3Payload {
            schema_version: PAYLOAD_VERSION,
            main_audio_input: plugin.main_audio_input,
            audio_output_count: plugin.audio_output_count,
            main_audio_output_count: plugin.main_audio_output_count,
            event_bus_contract: plugin.event_bus_contract,
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
        .with_auxiliary_output_contract(auxiliary_outputs)?
        .with_musical_time_reference(request.musical_time_reference())?
        .with_note_input_contract(note_input_contract)?
        .with_native_worker_lane_resources(native_lane)
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
    let mut decoded = decode_payload(payload)?;
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
    schema_version: u32,
    main_audio_input: Vst3MainAudioInput,
    audio_output_count: u8,
    main_audio_output_count: u8,
    event_bus_contract: Vst3EventBusContractV1,
    binary_path: PathBuf,
    class_id: String,
    vendor: Option<String>,
    binary_sha256: [u8; 32],
    #[serde(default, with = "super::vst3_state_json")]
    state: Option<Vec<u8>>,
    parameters: Vec<Vst3ParameterDescriptor>,
    parameter_ids: Vec<u32>,
}

fn decode_payload(bytes: &[u8]) -> Result<Vst3Payload, AudioProcessorHostError> {
    if bytes.len() > super::MAX_PREPARATION_PAYLOAD_BYTES {
        return Err(invalid("VST3 worker payload exceeds the metadata limit"));
    }
    let payload: Vst3Payload = serde_json::from_slice(bytes)
        .map_err(|error| invalid(format!("invalid VST3 worker payload: {error}")))?;
    if payload.schema_version != PAYLOAD_VERSION {
        return Err(invalid("unsupported VST3 worker payload version"));
    }
    validate_main_audio_output_count(payload.audio_output_count, payload.main_audio_output_count)?;
    payload.event_bus_contract.validate()?;
    Ok(payload)
}

// Non-RT decoding/identity validation may allocate Rust owners. No foreign
// plugin query or worker launch is part of this preparation request.
fn validated_preparation_payload(
    request: &IsolatedAudioProcessorWorkerPrepareRequest,
) -> Result<Vst3Payload, AudioProcessorHostError> {
    let payload = decode_payload(request.payload())?;
    let registration = Vst3PluginRegistration {
        class_id: payload.class_id.clone(),
        vendor: payload.vendor.clone(),
        binary_path: payload.binary_path.clone(),
        binary_sha256: payload.binary_sha256,
        execution_contract: request.plugin_execution_contract(),
        auxiliary_inputs: request.auxiliary_inputs().clone(),
        parameters: payload.parameters.clone(),
        main_audio_input: payload.main_audio_input,
        audio_output_count: payload.audio_output_count,
        main_audio_output_count: payload.main_audio_output_count,
        event_bus_contract: payload.event_bus_contract,
    };
    validate_registration(&registration)?;
    validate_audio_output_contract(
        payload.audio_output_count,
        request.auxiliary_outputs(),
        request.render_contract().channel_layout,
    )?;
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
        validate_note_preparation(request, &payload)?;
        native_worker_lane_profile_for_preparation(
            &payload.parameters,
            request.note_input_contract(),
            request.render_contract(),
            request.auxiliary_inputs(),
        )
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
        let payload = validated_preparation_payload(&request)?;
        validate_note_preparation(&request, &payload)?;
        if request.native_worker_lane_resources()
            != native_worker_lane_profile_for_preparation(
                &payload.parameters,
                request.note_input_contract(),
                request.render_contract(),
                request.auxiliary_inputs(),
            )?
        {
            return Err(invalid(
                "VST3 native storage request differs from prepared metadata",
            ));
        }
        // Reserve once before native load; charge actual capacity before Ready.
        let mut note_events = Vec::new();
        note_events
            .try_reserve_exact(request.note_input_contract().limits.input_events)
            .map_err(|_| unavailable("VST3 Note conversion allocation failed"))?;
        if let Some(profile) = request.native_worker_lane_resources() {
            profile
                .admit_adapter_note_input_storage(
                    crate::NativeHostBufferLayout::for_vector(&note_events).map_err(|error| {
                        invalid(format!("VST3 Note conversion layout failed: {error}"))
                    })?,
                )
                .map_err(AudioProcessorHostError::from)?;
        }
        let registration = Vst3PluginRegistration {
            class_id: payload.class_id.clone(),
            vendor: payload.vendor.clone(),
            binary_path: payload.binary_path.clone(),
            binary_sha256: payload.binary_sha256,
            execution_contract: request.plugin_execution_contract(),
            auxiliary_inputs: request.auxiliary_inputs().clone(),
            parameters: payload.parameters.clone(),
            main_audio_input: payload.main_audio_input,
            audio_output_count: payload.audio_output_count,
            main_audio_output_count: payload.main_audio_output_count,
            event_bus_contract: payload.event_bus_contract,
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
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        let native_main = std::rc::Rc::new(
            vst3_host::window::NativeEditorMainThread::initialize().map_err(|error| {
                unavailable(format!(
                    "VST3 actual native main initialization failed: {error}"
                ))
            })?,
        );
        let mut host_builder = Vst3Host::builder()
            .plugin_interfaces(product_plugin_interfaces())
            .initial_process_mode(mode)
            .sample_rate(f64::from(render.sample_rate))
            .block_size(render.max_block_frames)
            .input_channels(channels)
            .output_channels(channels);
        if request.native_worker_lane_resources().is_some() {
            host_builder = host_builder
                .processing_resource_bounds(processing_audio_buffer_bounds(
                    render,
                    request.auxiliary_inputs(),
                )?)
                .load_preparation(vst3_host::host::PluginLoadPreparation::Deferred);
        }
        let mut host = host_builder
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
        let input_names =
            prepare_loaded_metadata(&mut plugin, &registration, render.channel_layout)?;
        #[cfg(all(test, target_os = "windows"))]
        startup_metadata_diagnostics::observe("after_validate", &plugin, &payload.parameters);
        let buffers = plugin
            .create_bus_audio_buffers(render.max_block_frames)
            .map_err(|error| invalid(format!("VST3 bus buffer preparation failed: {error}")))?;
        plugin
            .start_processing()
            .map_err(|error| unavailable(format!("VST3 processing start failed: {error}")))?;
        validate_started_execution(&plugin, registration.execution_contract)?;
        #[cfg(all(test, target_os = "windows"))]
        startup_metadata_diagnostics::observe(
            "after_start_processing",
            &plugin,
            &payload.parameters,
        );
        Ok(Vst3Worker {
            native_lane: request.native_worker_lane_resources(),
            note_events,
            last_parameter_values: vec![None; payload.parameter_ids.len()],
            configuration_phase:
                super::native_configuration_control::NativeConfigurationPhase::Admitted,
            #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
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
            #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
            native_main,
            note_input_contract: request.note_input_contract,
        })
    }
}

fn derived_note_input_contract(
    raw: &Vst3EventBusContractV1,
    reference: Option<mondrian_core::MusicalTimeReference>,
    requested: bool,
) -> Result<crate::AudioProcessorNoteInputContract, AudioProcessorHostError> {
    if !requested {
        return Ok(crate::AudioProcessorNoteInputContract::default());
    }
    if reference.is_none() {
        return Err(unavailable(
            "VST3 Note input requires a musical time reference",
        ));
    }
    raw.validate()?;
    let main = raw
        .inputs
        .as_slice()
        .first()
        .filter(|row| {
            row.native_index == 0
                && row.role == super::vst3_processing_metadata::Vst3EventBusRole::Main
                && row.host_active
                && row.channel_count > 0
        })
        .filter(|_| raw.outputs.is_empty())
        .ok_or_else(|| {
            unavailable("VST3 Note input requires active Main event bus zero and no event outputs")
        })?;
    Ok(crate::AudioProcessorNoteInputContract {
        completion: crate::AudioNoteCompletionPolicy::SuccessfulOffBlock,
        ports: vec![crate::AudioProcessorNoteInputPort {
            port_key: "vst3.note-input.0".to_owned(),
            native_id: 0,
            native_index: 0,
            channel_count: main.channel_count.min(16) as u8,
        }],
        limits: crate::AudioNoteExecutionLimits {
            input_events: crate::AUDIO_NOTE_EVENTS_PER_BLOCK,
            output_ends: 0,
            retained_records: crate::AUDIO_NOTE_RECORDS,
            live_voices: 0,
        },
    })
}

fn validate_note_preparation(
    request: &IsolatedAudioProcessorWorkerPrepareRequest,
    payload: &Vst3Payload,
) -> Result<(), AudioProcessorHostError> {
    let enabled = !request.note_input_contract().ports.is_empty();
    payload.main_audio_input.validate_note_request(
        request.auxiliary_inputs(),
        request.musical_time_reference(),
        request.note_input_contract(),
    )?;
    let expected = derived_note_input_contract(
        &payload.event_bus_contract,
        request.musical_time_reference(),
        enabled,
    )?;
    if request.note_input_contract() != &expected {
        return Err(invalid(
            "VST3 Note input contract differs from prepared metadata",
        ));
    }
    if enabled && request.native_worker_lane_resources().is_none() {
        return Err(unavailable(
            "VST3 Note input requires the scoped native lane",
        ));
    }
    Ok(())
}

fn note_storage_layout(
    count: usize,
) -> Result<crate::NativeHostBufferLayout, AudioProcessorHostError> {
    let layout = std::alloc::Layout::array::<PluginEvent>(count)
        .map_err(|_| invalid("VST3 Note conversion extent overflowed"))?;
    Ok(crate::NativeHostBufferLayout { bytes: layout.size(), alignment: layout.align() })
}

fn prepare_note_events(
    converted: &mut Vec<PluginEvent>,
    expected: &crate::AudioProcessorNoteInputContract,
    block: &IsolatedAudioProcessorWorkerBlock<'_>,
) -> Result<(), AudioProcessorHostError> {
    if block.note_input_contract() != expected {
        return Err(invalid("VST3 block Note contract changed"));
    }
    let enabled = !expected.ports.is_empty();
    if enabled && block.musical_time_reference().is_none() {
        return Err(unavailable(
            "VST3 Note input requires a musical time reference",
        ));
    }
    let mut previous = 0;
    for (count, event) in block.note_events().enumerate() {
        if !enabled
            || count >= expected.limits.input_events
            || count >= converted.capacity()
            || !expected.accepts_event(event, block.frames())
            || i32::try_from(event.sample_offset).is_err()
            || event.sample_offset < previous
        {
            return Err(invalid(
                "VST3 Note batch exceeds the admitted scalar contract",
            ));
        }
        previous = event.sample_offset;
    }
    converted.clear();
    for event in block.note_events() {
        let channel = i16::from(event.channel);
        let pitch = i16::from(event.key);
        let velocity = event.velocity as f32;
        let data = match event.kind {
            crate::AudioNoteEventKind::On => PluginEventData::NoteOn {
                channel,
                pitch,
                tuning: 0.0,
                velocity,
                length: 0,
                note_id: event.native_note_id,
            },
            crate::AudioNoteEventKind::Off => PluginEventData::NoteOff {
                channel,
                pitch,
                velocity,
                note_id: event.native_note_id,
                tuning: 0.0,
            },
        };
        converted.push(PluginEvent {
            bus_index: 0,
            sample_offset: event.sample_offset as i32,
            // The checked SDK replaces this local scalar with same-reference
            // absolute Note PPQ before exposure. No second projection is cached.
            ppq_position: 0.0,
            flags: 0,
            data,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitialSignalPhase {
    Unserviced,
    AwaitingFirstSignal,
    Entered,
}

struct Vst3Worker {
    native_lane: Option<crate::NativeWorkerLaneResourceProfile>,
    note_events: Vec<PluginEvent>,
    note_input_contract: crate::AudioProcessorNoteInputContract,
    last_parameter_values: Vec<Option<f64>>,
    configuration_phase: super::native_configuration_control::NativeConfigurationPhase,
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    native_parent: Option<vst3_host::window::PluginEditorParent>,
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
    // Last fields: native main services survive the plugin, controller, module
    // and any explicitly removed native parent. They are never sent to DSP.
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    native_main: std::rc::Rc<vst3_host::window::NativeEditorMainThread>,
}

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
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
    fn note_input_contract(&self) -> crate::AudioProcessorNoteInputContract {
        self.note_input_contract.clone()
    }
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
        self.native_lane.is_some()
    }

    fn native_control_only_role(
        &mut self,
    ) -> Result<Option<Box<dyn super::IsolatedAudioProcessorMainRole + '_>>, AudioProcessorHostError>
    {
        let plugin = self
            .plugin
            .as_mut()
            .ok_or_else(|| unavailable("VST3 retained native processor is closed"))?;
        if self.configuration_phase.blocked_request().is_none()
            && plugin.can_split_editor_processing()
        {
            return Ok(None);
        }
        let editor = plugin.borrow_editor_control().map_err(|error| {
            unavailable(format!("VST3 stopped native main borrow failed: {error}"))
        })?;
        Ok(Some(Box::new(VstMainRole {
            editor,
            #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
            native_main: &self.native_main,
            #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
            native_parent: &mut self.native_parent,
            configuration_phase: &self.configuration_phase,
            native_binding: &self.native_binding,
            native_author_delivery: &self.native_author_delivery,
            prepared_author_entry: &self.prepared_author_entry,
        })))
    }

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
            note_input_contract: &self.note_input_contract,
            note_events: &mut self.note_events,
        };
        Ok(super::IsolatedAudioProcessorWorkerSplit {
            main: Box::new(VstMainRole {
                editor,
                #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
                native_main: &self.native_main,
                #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
                native_parent: &mut self.native_parent,
                configuration_phase: &self.configuration_phase,
                native_binding: &self.native_binding,
                native_author_delivery: &self.native_author_delivery,
                prepared_author_entry: &self.prepared_author_entry,
            }),
            processing: Box::new(VstDspRole { native: processor, signal }),
        })
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
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

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    fn close_native_editor_owner(&mut self) -> Result<(), AudioProcessorHostError> {
        let main = std::rc::Rc::clone(&self.native_main);
        main.with_native_scope(|| {
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
                let mut editor =
                    plugin.borrow_editor_control().map_err(native_editor_removal_failed)?;
                // DSP has joined; the same parent checks its private attachment
                // identity without requiring this owner to resume processing.
                parent.close(&mut editor).map_err(native_editor_removal_failed)?;
                self.native_parent = None;
            } else {
                // An unsuccessful attach can retain only failed host-frame cleanup.
                // The canonical attachment model decides the exact remaining step;
                // absence of a parent must never invent successful native removal.
                plugin.close_editor().map_err(native_editor_removal_failed)?;
            }
            Ok(())
        })
        .map_err(native_editor_removal_failed)?
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
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

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    fn open_native_audio_editor(
        &mut self,
        request: crate::NativeAudioEditorOpenRequest,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        let main = std::rc::Rc::clone(&self.native_main);
        main.with_native_scope(|| {
            use crate::{NativeAudioEditorOutcome as Outcome, NativeAudioEditorRefusal as Refusal};
            let binding = request.binding();
            if !self.native_editor_binding_matches(binding)? {
                return Ok(Outcome::Refused(Refusal::OwnerChanged));
            }
            if self.pending_reprepare().is_some()
                || self.configuration_phase.ready_token().is_some()
            {
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
            if !self.project_idle_controller_values(&request)? {
                return Ok(Outcome::Refused(Refusal::ConfigurationPending));
            }
            let projected = !request.static_parameter_values().is_empty();
            let plugin = self
                .plugin
                .as_mut()
                .ok_or_else(|| unavailable("native controller owner closed"))?;
            let mut editor =
                plugin.borrow_editor_control().map_err(native_editor_removal_failed)?;
            if self.native_parent.is_none() {
                match vst3_host::window::PluginEditorParent::open(
                    std::rc::Rc::clone(&self.native_main),
                    &mut editor,
                    request.title(),
                ) {
                    Ok(parent) => self.native_parent = Some(parent),
                    Err(error) if projected => return Err(native_editor_removal_failed(error)),
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
        })
        .map_err(native_editor_removal_failed)?
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    fn close_native_audio_editor(
        &mut self,
        binding: crate::NativeAudioObjectBindRequest,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        let main = std::rc::Rc::clone(&self.native_main);
        main.with_native_scope(|| {
            use crate::{NativeAudioEditorOutcome as Outcome, NativeAudioEditorRefusal as Refusal};
            if !self.native_editor_binding_matches(binding)? {
                return Ok(Outcome::Refused(Refusal::OwnerChanged));
            }
            let plugin = self
                .plugin
                .as_mut()
                .ok_or_else(|| unavailable("native editor processor owner is closed"))?;
            let result = if let Some(parent) = self.native_parent.as_mut() {
                let mut editor =
                    plugin.borrow_editor_control().map_err(native_editor_removal_failed)?;
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
        })
        .map_err(native_editor_removal_failed)?
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

    fn query_native_parameter(
        &mut self,
        binding: crate::NativeAudioObjectBindRequest,
        native_parameter_id: u32,
    ) -> Result<crate::NativeAudioParameterQueryResult, AudioProcessorHostError> {
        use mondrian_core::NativeAudioParameterQueryRefusal as Refusal;
        if !self.author_binding_current(binding)? {
            return Ok(Err(Refusal::OwnerChanged));
        }
        if self.native_author_delivery.is_some()
            || self.prepared_author_entry.is_some()
            || self.configuration_phase.blocked_request().is_some()
            || self.native_editor_detach_stage().is_some()
        {
            return Ok(Err(Refusal::Busy));
        }
        if !self.payload.parameter_ids.contains(&native_parameter_id) {
            return Ok(Err(Refusal::ParameterUnavailable));
        }
        let selected = [native_parameter_id];
        let current = self.plugin.as_mut().and_then(|plugin| {
            let editor = plugin.borrow_editor_control().ok()?;
            parameter_observations::capture(&editor, &self.payload.parameters, &selected)
        });
        // This authenticates the actual owner again after all native getters.
        // Source/cohort equality inside capture preserves every query callback
        // for the existing Poll; a copied request cannot manufacture a fact.
        if !self.author_binding_current(binding)? {
            return Ok(Err(Refusal::OwnerChanged));
        }
        let Some(current) = current else {
            return Ok(Err(Refusal::ParameterUnavailable));
        };
        if current.owner != binding.expected_object.object_serial() || current.facts.len() != 1 {
            return Ok(Err(Refusal::OwnerChanged));
        }
        Ok(Ok((current.revision, current.facts)))
    }
    fn poll_native_observations(
        &mut self,
        consumption: crate::NativeAudioObservationConsumption,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        let object = self.native_audio_object_observation()?;
        let author = self.observe_native_author(consumption)?;
        let mut observations = crate::NativeAudioObservations {
            object,
            author,
            editor_cleanup: self.native_editor_detach_stage(),
            ..Default::default()
        };
        parameter_observations::append_optional(
            &mut observations,
            consumption,
            self.native_author_delivery.is_none()
                && self.prepared_author_entry.is_none()
                && self.configuration_phase.blocked_request().is_none(),
            || {
                let editor = self.plugin.as_mut()?.borrow_editor_control().ok()?;
                parameter_observations::capture(
                    &editor,
                    &self.payload.parameters,
                    &self.payload.parameter_ids,
                )
            },
        );
        // Query callbacks remain in the original source. Read their actual live
        // request after the optional query, with no second author drain.
        observations.pending_reprepare = self.pending_reprepare();
        Ok(observations)
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
        if !retained_constants_match_successful_delivery(
            request.canonical_constant_values,
            &self.last_parameter_values,
        ) {
            // Only explicit constants require successful delivered DSP evidence.
            // None remains owned by the real native state, regardless of any
            // earlier host-delivery cache; do not clear or synthesize that cache.
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
        if !retained_audio_inputs_match(
            plugin,
            self.render.channel_layout,
            self.payload.main_audio_input,
            &self.auxiliary_inputs,
            self.payload.audio_output_count,
            self.payload.main_audio_output_count,
        )? || !retained_parameters_match(plugin, &self.payload.parameters)?
            || Vst3EventBusContractV1::observe(plugin)? != self.payload.event_bus_contract
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

    fn pending_reprepare(&self) -> Option<crate::NativeObservedRequest> {
        self.native_configuration_cohort()
            .map(crate::NativeObservedRequest::Cohort)
            .or(self
                .configuration_phase
                .blocked_request()
                .map(crate::NativeObservedRequest::RequestOnly))
    }

    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.contract
    }

    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        self.auxiliary_inputs.clone()
    }

    fn auxiliary_output_contract(&self) -> AudioProcessorAuxiliaryOutputContract {
        // The Ready/control query returns an owned snapshot. No getter is used
        // by DSP, and no extra output-contract Vec is retained by this Worker.
        audio_output_contract(self.payload.audio_output_count, self.render.channel_layout)
    }

    fn auxiliary_input_names(&self) -> Vec<crate::AudioProcessorInputName> {
        self.input_names.clone()
    }

    fn prepare_native_author_state_entry(
        &mut self,
        request: crate::NativeAudioAuthorStateEntryRequest<'_>,
    ) -> Result<crate::NativeAudioAuthorStateEntryOutcome, AudioProcessorHostError> {
        if !self.note_input_contract.ports.is_empty() {
            return Ok(crate::NativeAudioAuthorStateEntryOutcome::Refused(
                crate::NativeAudioAuthorStateEntryRefusal::UnsupportedAdapter,
            ));
        }
        self.prepare_author_state_entry(request)
    }

    fn enter_state(&mut self, start_sample: i64) -> Result<(), AudioProcessorHostError> {
        let note_reentry = !self.note_input_contract.ports.is_empty()
            && self.initial_signal_phase == InitialSignalPhase::Entered;
        if note_reentry && self.prepared_author_entry.is_some() {
            return Err(AudioProcessorHostError::StateEntry(
                "VST3 Note reentry cannot consume a prepared author transaction".to_owned(),
            ));
        }
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
        if note_reentry {
            // Parent entry revokes its published lease locally, while the child
            // retains this exact old Bind receipt until retirement starts.
            // An unbound direct factory owner cannot authorize replacement.
            let binding = self.native_binding.ok_or_else(|| {
                AudioProcessorHostError::StateEntry(
                    "VST3 Note reentry requires its current bound producer".to_owned(),
                )
            })?;
            if !self.author_binding_current(crate::NativeAudioObjectBindRequest {
                binding: binding.binding(),
                expected_object: binding.object(),
            })? {
                return Err(AudioProcessorHostError::StateEntry(
                    "VST3 Note reentry requires its current bound producer".to_owned(),
                ));
            }
            // Last live check on the actual old producer. Preserve never drains
            // or acknowledges a batch arriving after the parent's preflight.
            // A closed author boundary is distinct from a closed native GUI.
            #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
            if self.native_parent.is_some() || self.native_editor_detach_stage().is_some() {
                return Err(AudioProcessorHostError::StateEntry(
                    "VST3 Note reentry has live editor/author/configuration work".to_owned(),
                ));
            }
            if self.pending_reprepare().is_some() || self.prepared_author_entry.is_some() {
                return Err(AudioProcessorHostError::StateEntry(
                    "VST3 Note reentry has live editor/author/configuration work".to_owned(),
                ));
            }
            let author =
                self.observe_native_author(crate::NativeAudioObservationConsumption::Preserve)?;
            crate::native_audio_author_runtime::closed_source(
                author.as_ref(),
                self.native_author_delivery.is_some(),
            )
            .map_err(|reason| {
                AudioProcessorHostError::StateEntry(format!(
                    "VST3 Note reentry source is not closed: {reason:?}"
                ))
            })?;
        }
        // Native close/retirement may partially fail; the old author lease must
        // already be revoked once this non-preflight work starts.
        self.native_binding = None;
        if !note_reentry {
            self.close_native_editor_owner()?;
            self.prepared_author_entry = None;
        }
        self.rebuild_native_state(None)?;
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
            note_input_contract: &self.note_input_contract,
            note_events: &mut self.note_events,
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
    note_input_contract: &'a crate::AudioProcessorNoteInputContract,
    note_events: &'a mut Vec<PluginEvent>,
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
        reference: Option<vst3_host::audio::ProcessMusicalReference>,
        activity: Option<mondrian_core::TimelineTransportActivity>,
    ) -> vst3_host::error::Result<()>;
    fn process_buses(&mut self, buffers: &mut BusAudioBuffers) -> vst3_host::error::Result<()>;
    fn process_note_buses(
        &mut self,
        buffers: &mut BusAudioBuffers,
        events: &[PluginEvent],
    ) -> vst3_host::error::Result<()>;
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
        reference: Option<vst3_host::audio::ProcessMusicalReference>,
        activity: Option<mondrian_core::TimelineTransportActivity>,
    ) -> vst3_host::error::Result<()> {
        match reference {
            Some(reference) => self.set_process_musical_timeline_context(context, reference),
            None => self.set_process_timeline_context(context),
        }?;
        if let Some(activity) = activity {
            self.set_playing(activity == mondrian_core::TimelineTransportActivity::Playing)?;
        }
        Ok(())
    }
    fn process_buses(&mut self, buffers: &mut BusAudioBuffers) -> vst3_host::error::Result<()> {
        self.process_bus_audio(buffers)
    }
    fn process_note_buses(
        &mut self,
        buffers: &mut BusAudioBuffers,
        events: &[PluginEvent],
    ) -> vst3_host::error::Result<()> {
        self.process_bus_audio_with_note_events(buffers, events)
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
        reference: Option<vst3_host::audio::ProcessMusicalReference>,
        activity: Option<mondrian_core::TimelineTransportActivity>,
    ) -> vst3_host::error::Result<()> {
        match reference {
            Some(reference) => self.set_process_musical_timeline_context(context, reference),
            None => self.set_process_timeline_context(context),
        }?;
        if let Some(activity) = activity {
            self.set_process_playing(
                activity == mondrian_core::TimelineTransportActivity::Playing,
            )?;
        }
        Ok(())
    }
    fn process_buses(&mut self, buffers: &mut BusAudioBuffers) -> vst3_host::error::Result<()> {
        self.process_bus_audio(buffers)
    }
    fn process_note_buses(
        &mut self,
        buffers: &mut BusAudioBuffers,
        events: &[PluginEvent],
    ) -> vst3_host::error::Result<()> {
        self.process_bus_audio_with_note_events(buffers, events)
    }
}

impl VstSignalFields<'_> {
    fn process(
        &mut self,
        native: &mut dyn VstNativeSignal,
        mut block: IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        if let Some(request) = self.configuration_phase.blocked_request() {
            return Err(AudioProcessorHostError::ReprepareRequired(
                crate::NativeObservedRequest::RequestOnly(request),
            ));
        }
        self.payload.main_audio_input.validate_note_request(
            self.auxiliary_inputs,
            block.musical_time_reference(),
            self.note_input_contract,
        )?;
        let frames = block.frames();
        let signed_frames = i64::try_from(frames).map_err(|_| {
            AudioProcessorHostError::Process("VST3 sample extent is not representable".to_owned())
        })?;
        let end_sample = block.start_sample().checked_add(signed_frames).ok_or_else(|| {
            AudioProcessorHostError::Process("VST3 sample position overflowed".to_owned())
        })?;
        let channels = self.render.channel_count();
        let samples = frames.checked_mul(channels).ok_or_else(|| {
            AudioProcessorHostError::Process("VST3 output extent overflowed".to_owned())
        })?;
        if *self.last_end != Some(block.start_sample())
            || frames == 0
            || frames > self.render.max_block_frames
            || block.main_interleaved().len() != samples
            || block.parameter_lane_count() != self.payload.parameter_ids.len()
        {
            return Err(AudioProcessorHostError::Process(
                "VST3 block violates continuity, extent, or parameter lanes".to_owned(),
            ));
        }
        validate_audio_output_count(self.payload.audio_output_count)?;
        if self.buffers.inputs.len()
            != self.payload.main_audio_input.port_offset() + self.auxiliary_inputs.buses.len()
            || self.buffers.inputs.iter().enumerate().any(|(index, bus)| {
                let width = if index < self.payload.main_audio_input.port_offset() {
                    channels
                } else {
                    self.auxiliary_inputs.buses[index - self.payload.main_audio_input.port_offset()]
                        .channel_layout
                        .channel_count()
                };
                !bus.active
                    || bus.channels.len() != width
                    || bus.channels.iter().any(|channel| channel.capacity() < frames)
            })
        {
            return Err(invalid("VST3 prepared input storage changed shape"));
        }
        for bus in &self.auxiliary_inputs.buses {
            if block.auxiliary_input(&bus.bus_key).is_none_or(|pcm| {
                frames.checked_mul(bus.channel_layout.channel_count()) != Some(pcm.len())
            }) {
                return Err(invalid("VST3 auxiliary PCM extent changed"));
            }
        }
        if self.buffers.outputs.len() != usize::from(self.payload.audio_output_count)
            || self.buffers.outputs.iter().any(|bus| {
                !bus.active
                    || bus.channels.len() != channels
                    || bus.channels.iter().any(|channel| channel.capacity() < frames)
            })
        {
            return Err(invalid("VST3 prepared output storage changed shape"));
        }
        // Check every destination, including currently unrouted native outputs,
        // before timeline/event delivery or any native call can consume input.
        let output_keys = &AUDIO_OUTPUT_KEYS[..usize::from(self.payload.audio_output_count - 1)];
        for key in output_keys {
            if block.auxiliary_output(key).is_none_or(|output| output.len() != samples) {
                return Err(invalid("VST3 auxiliary output destination changed extent"));
            }
        }
        // Lower the Sequence reference directly for this original native block.
        // The SDK owns the only installed native copy; no derived clock is cached
        // by this adapter. Refuse the entire signed window/setter before enqueue
        // or PCM scratch writes. Later SDK ingress refusals are not a parameter
        // transaction rollback and still fail this worker operation normally.
        let reference = block
            .musical_time_reference()
            .map(|reference| {
                let zero = reference.quarter_note_zero();
                let duration = reference.quarter_note_duration();
                let native = vst3_host::audio::ProcessMusicalReference::new(
                    zero.numerator(),
                    zero.denominator(),
                    duration.numerator(),
                    duration.denominator(),
                )?;
                match reference.time_signature() {
                    Some(signature) => {
                        native.with_time_signature(signature.numerator(), signature.denominator())
                    }
                    None => Ok(native),
                }
            })
            .transpose()
            .map_err(|error| {
                AudioProcessorHostError::Process(format!(
                    "VST3 musical reference delivery failed: {error:?}"
                ))
            })?;
        prepare_note_events(self.note_events, self.note_input_contract, &block)?;
        native
            .deliver_timeline(
                vst3_host::audio::ProcessTimelineContext {
                    sample_position: block.start_sample(),
                    sample_rate: block.sample_rate(),
                },
                reference,
                block.transport_activity(),
            )
            .map_err(|error| {
                AudioProcessorHostError::Process(format!("VST3 timeline delivery failed: {error}"))
            })?;
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
            let mut events = block.parameter_events(lane).ok_or_else(|| {
                AudioProcessorHostError::Process("VST3 parameter lane is missing".to_owned())
            })?;
            let first = events.next();
            let second = events.next();
            // Author constant curves supply an offset-zero point every block.
            // VST3 carries an unchanged constant implicitly after its first point.
            // Multi-point curves and explicit nonzero-offset events stay intact.
            let implicit_constant = second.is_none()
                && first.is_some_and(|event| {
                    event.sample_offset == 0
                        && self.last_parameter_values[lane]
                            .is_some_and(|value| value.to_bits() == event.value.to_bits())
                });
            for event in first.into_iter().chain(second).chain(events) {
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
                // Omitted native points still consume the original event budget
                // and pass every descriptor, value and sample-offset check above.
                if !implicit_constant {
                    native
                        .deliver_parameter_at(native_id, value, event.sample_offset as i32)
                        .map_err(|error| {
                            AudioProcessorHostError::Process(format!(
                                "VST3 parameter delivery failed: {error}"
                            ))
                        })?;
                }
            }
        }
        for bus in self.buffers.inputs.iter_mut().chain(&mut self.buffers.outputs) {
            for channel in &mut bus.channels {
                channel.resize(frames, 0.0);
                channel.fill(0.0);
            }
        }
        for (index, bus) in self.auxiliary_inputs.buses.iter().enumerate() {
            let bus_channels = bus.channel_layout.channel_count();
            let samples = block.auxiliary_input(&bus.bus_key).ok_or_else(|| {
                AudioProcessorHostError::Process("VST3 auxiliary PCM is missing".to_owned())
            })?;
            if samples.len() != frames * bus_channels {
                return Err(AudioProcessorHostError::Process(
                    "VST3 auxiliary extent changed".to_owned(),
                ));
            }
            for (frame, values) in samples.chunks_exact(bus_channels).enumerate() {
                for (channel, sample) in values.iter().copied().enumerate() {
                    self.buffers.inputs[index + self.payload.main_audio_input.port_offset()]
                        .channels[channel][frame] = sample;
                }
            }
        }
        if self.payload.main_audio_input == Vst3MainAudioInput::Present {
            for (frame, samples) in block.main_interleaved().chunks_exact(channels).enumerate() {
                for (channel, sample) in samples.iter().copied().enumerate() {
                    self.buffers.inputs[0].channels[channel][frame] = sample;
                }
            }
        }
        self.buffers.block_size = frames;
        let result = if self.note_input_contract.ports.is_empty() {
            native.process_buses(self.buffers)
        } else {
            // Empty Note batches also use the checked API with the same reference.
            native.process_note_buses(self.buffers, self.note_events)
        };
        self.note_events.clear(); // A failed batch is never retained for replay.
        result.map_err(|error| {
            AudioProcessorHostError::Process(format!("VST3 processing failed: {error}"))
        })?;
        // Validate all native sources before exposing any Main or Aux PCM.
        // A successful native call with unusable PCM still poisons the parent
        // operation and must not advance this signal cursor.
        if self.buffers.outputs.len() != usize::from(self.payload.audio_output_count)
            || self.buffers.outputs.iter().any(|bus| {
                bus.channels.len() != channels
                    || bus.channels.iter().any(|channel| {
                        channel.len() != frames || channel.iter().any(|sample| !sample.is_finite())
                    })
            })
        {
            return Err(invalid(
                "VST3 output contains invalid extents or nonfinite PCM",
            ));
        }
        for (frame, samples) in block.main_interleaved().chunks_exact_mut(channels).enumerate() {
            for (channel, sample) in samples.iter_mut().enumerate() {
                *sample = self.buffers.outputs[0].channels[channel][frame];
            }
        }
        for (index, key) in output_keys.iter().enumerate() {
            // Native code cannot mutate this parent's frozen shared-output view.
            // The complete destination set was checked before native exposure.
            let output = block.auxiliary_output(key).expect("validated frozen VST3 output view");
            for (frame, values) in output.chunks_exact_mut(channels).enumerate() {
                for (channel, sample) in values.iter_mut().enumerate() {
                    *sample = self.buffers.outputs[index + 1].channels[channel][frame];
                }
            }
        }
        for lane in 0..self.payload.parameter_ids.len() {
            // An empty author lane cedes the value to the native state. Forget
            // prior delivery so a later explicit same-value override is sent.
            // Like the cursor, this evidence changes only after successful DSP
            // and validation of every native output.
            self.last_parameter_values[lane] = block
                .parameter_events(lane)
                .and_then(|events| events.last().map(|event| event.value));
        }
        *self.last_end = Some(end_sample);
        Ok(())
    }
}

fn prepare_loaded_metadata(
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
    let (main_audio_input, metadata, audio_output_count, main_audio_output_count) =
        prepare_audio_inputs(plugin, channel_layout)?;
    if main_audio_input != registration.main_audio_input {
        return Err(invalid("VST3 main audio input changed since probe"));
    }
    if metadata.contract != registration.auxiliary_inputs {
        return Err(invalid("VST3 auxiliary bus contract changed since probe"));
    }
    if audio_output_count != registration.audio_output_count
        || main_audio_output_count != registration.main_audio_output_count
    {
        return Err(invalid("VST3 audio output shape changed since probe"));
    }
    let parameters = parameter_descriptors(plugin)?;
    // Both directories retain their complete shape and valid captions. A
    // restored state can legitimately rename parameters without changing the
    // processing contract. Keep the original numeric PartialEq policy here.
    if parameters.len() != registration.parameters.len()
        || parameters.iter().zip(&registration.parameters).any(|(actual, expected)| {
            validate_parameter(expected).is_err()
                || actual.id != expected.id
                || actual.default_normalized != expected.default_normalized
                || actual.step_count != expected.step_count
                || actual.can_automate != expected.can_automate
                || actual.read_only != expected.read_only
                || actual.flags != expected.flags
        })
    {
        return Err(invalid(
            "VST3 parameter processing metadata changed since contract probe",
        ));
    }
    if Vst3EventBusContractV1::observe(plugin)? != registration.event_bus_contract {
        return Err(invalid("VST3 event-bus contract changed since probe"));
    }
    Ok(metadata.names)
}

/// Read activated execution facts after the original native start, before any
/// Ready publication or process call. Deferred load has no earlier activation.
fn validate_started_execution(
    plugin: &Plugin,
    contract: AudioProcessorExecutionContract,
) -> Result<(), AudioProcessorHostError> {
    if !plugin.is_processing() {
        return Err(invalid("VST3 execution validation requires native start"));
    }
    let tail = match plugin.tail_samples() {
        0 => AudioProcessorTail::None,
        u32::MAX => AudioProcessorTail::Infinite,
        frames => AudioProcessorTail::Finite(frames as usize),
    };
    if plugin.latency_samples() as usize != contract.algorithmic_latency_frames()
        || tail != contract.tail()
    {
        return Err(invalid("VST3 latency or tail changed since contract probe"));
    }
    Ok(())
}

/// Compare stopped, already activated buses without collecting another inventory.
fn retained_audio_inputs_match(
    plugin: &Plugin,
    channel_layout: AudioChannelLayout,
    admitted_main: Vst3MainAudioInput,
    admitted_auxiliary: &AudioProcessorAuxiliaryInputContract,
    admitted_outputs: u8,
    admitted_main_outputs: u8,
) -> Result<bool, AudioProcessorHostError> {
    let mut frame = AudioBusQueryFrame::default();
    let observed = plugin.observe_audio_buses_bounded(&mut frame).map_err(|error| match error {
        AudioBusObservationError::Metadata(error) => {
            invalid(format!("VST3 active bus metadata failed: {error}"))
        }
        AudioBusObservationError::Layout(error) => {
            invalid(format!("VST3 active bus layout failed: {error}"))
        }
        AudioBusObservationError::Arrangements(error) => {
            invalid(format!("VST3 active arrangements failed: {error}"))
        }
    })?;
    audio_bus_observation_matches(
        &observed,
        channel_layout,
        admitted_main,
        admitted_auxiliary,
        admitted_outputs,
        admitted_main_outputs,
    )
}

fn audio_bus_observation_matches(
    observed: &AudioBusObservationRef<'_>,
    channel_layout: AudioChannelLayout,
    admitted_main: Vst3MainAudioInput,
    admitted_auxiliary: &AudioProcessorAuxiliaryInputContract,
    admitted_outputs: u8,
    admitted_main_outputs: u8,
) -> Result<bool, AudioProcessorHostError> {
    // All three SDK stages must finish before domain validation. Preserve the
    // cold contract's shape errors before inactivity, then compare identity.
    if !matches!(
        channel_layout,
        AudioChannelLayout::Mono | AudioChannelLayout::Stereo
    ) {
        return Err(invalid("VST3 audio channel layout is unsupported"));
    }
    if observed.native_inputs.len() > 32
        || !(1..=32).contains(&observed.native_outputs.len())
        || observed.layout_inputs.len() != observed.native_inputs.len()
        || observed.layout_outputs.len() != observed.native_outputs.len()
        || observed.input_arrangements.len() != observed.native_inputs.len()
        || observed.output_arrangements.len() != observed.native_outputs.len()
    {
        return Err(invalid(
            "VST3 requires bounded inputs and a nonempty Main output prefix with bounded additional outputs",
        ));
    }
    let main = native_main_audio_input(observed.native_inputs.first().map(|bus| bus.role));
    let main_outputs = main_output_prefix(observed.native_outputs.iter().map(|bus| bus.role));
    if observed.native_inputs.len() > 31 + main.port_offset() {
        return Err(invalid(
            "VST3 auxiliary inputs exceed the existing 31-port bound",
        ));
    }
    for (direction, native, prepared, speakers) in [
        (
            BusDirection::Input,
            observed.native_inputs,
            observed.layout_inputs,
            observed.input_arrangements,
        ),
        (
            BusDirection::Output,
            observed.native_outputs,
            observed.layout_outputs,
            observed.output_arrangements,
        ),
    ] {
        for (index, ((bus, prepared), arrangement)) in
            native.iter().zip(prepared).zip(speakers).enumerate()
        {
            let role = expected_audio_bus_role(direction, index, main, main_outputs);
            let port_layout =
                negotiated_audio_bus_layout(direction, role, *arrangement, channel_layout);
            let port_speakers = if port_layout == AudioChannelLayout::Mono {
                SpeakerArrangement::MONO
            } else {
                SpeakerArrangement::STEREO
            };
            if bus.role != role
                || bus.channel_count != port_layout.channel_count()
                || prepared.channel_count != bus.channel_count
                || *arrangement != port_speakers
            {
                return Err(audio_bus_shape_refusal(AudioBusShapeFailure {
                    stage: "retained",
                    direction,
                    index,
                    native_role: bus.role,
                    expected_role: role,
                    native_channels: bus.channel_count,
                    prepared_channels: prepared.channel_count,
                    host_active: prepared.active,
                    arrangement: *arrangement,
                    expected_arrangement: port_speakers,
                    expected_layout: port_layout,
                }));
            }
        }
    }
    if observed
        .layout_inputs
        .iter()
        .chain(observed.layout_outputs)
        .any(|bus| !bus.active)
    {
        return Err(invalid(
            "VST3 retained activation left an admitted bus inactive",
        ));
    }
    let offset = main.port_offset();
    let auxiliary_count = observed
        .native_inputs
        .len()
        .checked_sub(offset)
        .ok_or_else(|| invalid("VST3 native input offset exceeds the inventory"))?;
    // Cold preparation admitted these exact keys/layouts. Reconstruct their
    // scalar identity here without allocating keys, a Vec, or a validation set.
    Ok(main == admitted_main
        && observed.native_outputs.len() == usize::from(admitted_outputs)
        && main_outputs == usize::from(admitted_main_outputs)
        && admitted_auxiliary.buses.len() == auxiliary_count
        && admitted_auxiliary.buses.iter().enumerate().all(|(index, bus)| {
            canonical_audio_input_key_matches(&bus.bus_key, index + offset)
                && bus.channel_layout
                    == negotiated_audio_bus_layout(
                        BusDirection::Input,
                        AudioBusRole::Auxiliary,
                        observed.input_arrangements[index + offset],
                        channel_layout,
                    )
        }))
}

// Arrangement supplies semantic channel meaning. Unknown arrangements keep the
// render shape only as a diagnostic expectation and fail the exact-mask check.
fn negotiated_audio_bus_layout(
    direction: BusDirection,
    role: AudioBusRole,
    arrangement: SpeakerArrangement,
    render: AudioChannelLayout,
) -> AudioChannelLayout {
    if direction == BusDirection::Input && role == AudioBusRole::Auxiliary {
        if arrangement == SpeakerArrangement::MONO {
            return AudioChannelLayout::Mono;
        }
        if arrangement == SpeakerArrangement::STEREO {
            return AudioChannelLayout::Stereo;
        }
    }
    render
}

fn canonical_audio_input_key_matches(key: &str, index: usize) -> bool {
    let Some(suffix) = key.strip_prefix("vst3.audio-input.") else {
        return false;
    };
    !suffix.is_empty()
        && suffix.bytes().all(|byte| byte.is_ascii_digit())
        && (suffix.len() == 1 || !suffix.starts_with('0'))
        && suffix.parse::<usize>() == Ok(index)
}

// Scalars from the already completed census, used only by a rejected bus.
// No plugin name, second query or retained diagnostic owner is involved.
struct AudioBusShapeFailure {
    stage: &'static str,
    direction: BusDirection,
    index: usize,
    native_role: AudioBusRole,
    expected_role: AudioBusRole,
    native_channels: usize,
    prepared_channels: usize,
    host_active: bool,
    arrangement: SpeakerArrangement,
    expected_arrangement: SpeakerArrangement,
    expected_layout: AudioChannelLayout,
}

#[cold]
fn audio_bus_shape_refusal(facts: AudioBusShapeFailure) -> AudioProcessorHostError {
    // Match the existing predicate order to identify the first failed field.
    let expected_channels = facts.expected_layout.channel_count();
    let reason = if facts.native_role != facts.expected_role {
        "role"
    } else if facts.native_channels != expected_channels {
        "native_channels"
    } else if facts.prepared_channels != facts.native_channels {
        "prepared_channels"
    } else {
        "arrangement"
    };
    let direction = match facts.direction {
        BusDirection::Input => "input",
        BusDirection::Output => "output",
    };
    invalid(format!(
        "VST3 host requires optional Main input zero, typed Mono/Stereo Auxiliary inputs and a nonempty Main output prefix followed by Auxiliary outputs with the exact render layout; stage={} reason={reason} direction={direction} index={} native_role={:?} expected_role={:?} native_channels={} prepared_channels={} host_active={} actual_arrangement_raw={:016X} expected_arrangement_raw={:016X} expected_layout={:?} expected_channels={expected_channels}",
        facts.stage, facts.index, facts.native_role, facts.expected_role,
        facts.native_channels, facts.prepared_channels, facts.host_active,
        facts.arrangement.raw(), facts.expected_arrangement.raw(), facts.expected_layout,
    ))
}

/// Read roles from the actual restored component and activate admitted audio ports.
pub(super) fn prepare_audio_inputs(
    plugin: &mut Plugin,
    channel_layout: AudioChannelLayout,
) -> Result<(Vst3MainAudioInput, super::NativeAudioInputMetadata, u8, u8), AudioProcessorHostError>
{
    let metadata = plugin
        .native_audio_bus_info()
        .map_err(|error| invalid(format!("VST3 native bus query failed: {error}")))?;
    let layout = plugin
        .audio_bus_layout()
        .map_err(|error| invalid(format!("VST3 prepared bus query failed: {error}")))?;
    let arrangements = plugin
        .bus_arrangements()
        .map_err(|error| invalid(format!("VST3 speaker arrangement query failed: {error}")))?;
    let expected = audio_inputs_from_metadata(
        &metadata,
        &layout,
        &arrangements,
        channel_layout,
        "before_activation",
    )?;
    let audio_output_count = metadata.outputs.len() as u8; // Validated within 1..=32 above.
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
    let current = audio_inputs_from_metadata(
        &current_metadata,
        &active,
        &current_arrangements,
        channel_layout,
        "after_activation",
    )?;
    if active.inputs.iter().chain(&active.outputs).any(|bus| !bus.active)
        || current_metadata.outputs.len() != usize::from(audio_output_count)
        || current != expected
    {
        return Err(invalid("VST3 bus contract changed during activation"));
    }
    let names = current_metadata
        .inputs
        .iter()
        .enumerate()
        .skip(expected.0.port_offset())
        .filter_map(|(index, bus)| {
            crate::AudioProcessorInputName::capture(format!("vst3.audio-input.{index}"), &bus.name)
        })
        .collect();
    Ok((
        expected.0,
        super::NativeAudioInputMetadata { contract: expected.1, names },
        audio_output_count,
        expected.2,
    ))
}

fn audio_inputs_from_metadata(
    metadata: &NativeAudioBusInfoSet,
    layout: &AudioBusLayout,
    arrangements: &BusArrangements,
    channel_layout: AudioChannelLayout,
    stage: &'static str,
) -> Result<(Vst3MainAudioInput, AudioProcessorAuxiliaryInputContract, u8), AudioProcessorHostError>
{
    if !matches!(
        channel_layout,
        AudioChannelLayout::Mono | AudioChannelLayout::Stereo
    ) {
        return Err(invalid("VST3 audio channel layout is unsupported"));
    }
    if metadata.inputs.len() > 32
        || !(1..=32).contains(&metadata.outputs.len())
        || layout.inputs.len() != metadata.inputs.len()
        || layout.outputs.len() != metadata.outputs.len()
        || arrangements.inputs.len() != metadata.inputs.len()
        || arrangements.outputs.len() != metadata.outputs.len()
    {
        return Err(invalid(
            "VST3 requires bounded inputs and a nonempty Main output prefix with bounded additional outputs",
        ));
    }
    let main = native_main_audio_input(metadata.inputs.first().map(|bus| bus.role));
    let main_outputs = main_output_prefix(metadata.outputs.iter().map(|bus| bus.role));
    if metadata.inputs.len() > 31 + main.port_offset() {
        return Err(invalid(
            "VST3 auxiliary inputs exceed the existing 31-port bound",
        ));
    }
    for (direction, native, prepared, speakers) in [
        (
            BusDirection::Input,
            &metadata.inputs,
            &layout.inputs,
            &arrangements.inputs,
        ),
        (
            BusDirection::Output,
            &metadata.outputs,
            &layout.outputs,
            &arrangements.outputs,
        ),
    ] {
        for (index, ((bus, prepared), arrangement)) in
            native.iter().zip(prepared).zip(speakers).enumerate()
        {
            let role = expected_audio_bus_role(direction, index, main, main_outputs);
            let port_layout =
                negotiated_audio_bus_layout(direction, role, *arrangement, channel_layout);
            let port_speakers = if port_layout == AudioChannelLayout::Mono {
                SpeakerArrangement::MONO
            } else {
                SpeakerArrangement::STEREO
            };
            if bus.role != role
                || bus.channel_count != port_layout.channel_count()
                || prepared.channel_count != bus.channel_count
                || *arrangement != port_speakers
            {
                return Err(audio_bus_shape_refusal(AudioBusShapeFailure {
                    stage,
                    direction,
                    index,
                    native_role: bus.role,
                    expected_role: role,
                    native_channels: bus.channel_count,
                    prepared_channels: prepared.channel_count,
                    host_active: prepared.active,
                    arrangement: *arrangement,
                    expected_arrangement: port_speakers,
                    expected_layout: port_layout,
                }));
            }
        }
    }
    let main_audio_input = main;
    let result = AudioProcessorAuxiliaryInputContract {
        buses: (main_audio_input.port_offset()..metadata.inputs.len())
            .map(|index| AudioProcessorAuxiliaryInputBusContract {
                bus_key: format!("vst3.audio-input.{index}"),
                channel_layout: negotiated_audio_bus_layout(
                    BusDirection::Input,
                    AudioBusRole::Auxiliary,
                    arrangements.inputs[index],
                    channel_layout,
                ),
            })
            .collect(),
    };
    result.validate(channel_layout)?;
    main_audio_input.validate_auxiliary(&result)?;
    Ok((main_audio_input, result, main_outputs as u8))
}

/// Compare the stopped retained component without collecting a second inventory.
/// The existing main-inline query slot is reused. Validation remains after the
/// complete census, so a later invalid field still precedes an earlier mismatch.
fn retained_parameters_match(
    plugin: &Plugin,
    admitted: &[Vst3ParameterDescriptor],
) -> Result<bool, AudioProcessorHostError> {
    let mut frame = vst3_host::controller_parameters::ControllerParameterQueryFrame::default();
    let mut valid = true;
    let mut matches = true;
    let count = plugin
        .visit_parameters_bounded(MAX_PARAMETERS, &mut frame, &mut |index, observed| {
            valid &= retained_parameter_fields_valid(
                observed.title_utf16(),
                observed.default_normalized(),
                observed.step_count(),
            );
            matches &= admitted.get(index).is_some_and(|expected| {
                validate_parameter(expected).is_ok()
                    && observed.id() == expected.id
                    // Preserve the descriptor's numeric PartialEq, including ±0.
                    && observed.default_normalized() == expected.default_normalized
                    && observed.step_count() == expected.step_count
                    && observed.can_automate() == expected.can_automate
                    && observed.is_read_only() == expected.read_only
                    && observed.flags() == expected.flags
            });
            // The legacy descriptor discards current values and units. The SDK
            // still queries each successful row's value, without new validation.
        })
        .map_err(|error| invalid(format!("VST3 parameter query failed: {error}")))?;
    if !valid {
        return Err(invalid("VST3 parameter metadata is invalid"));
    }
    Ok(matches && count == admitted.len())
}

fn retained_parameter_fields_valid(title: &[u16; 128], default: f64, steps: i32) -> bool {
    if !default.is_finite() || !(0.0..=1.0).contains(&default) || steps < 0 {
        return false;
    }
    let mut bytes = 0;
    for scalar in char::decode_utf16(title.iter().copied().take_while(|code| *code != 0)) {
        let scalar = scalar.unwrap_or(char::REPLACEMENT_CHARACTER);
        bytes += scalar.len_utf8();
        if bytes > 256 || scalar.is_control() {
            return false;
        }
    }
    bytes != 0
}

// Both cold inventories keep their independent native count/Info/value query.
// Share only the existing admission bound and one inline SDK output slot.
pub(super) fn visit_parameter_inventory(
    plugin: &Plugin,
    visitor: &mut dyn FnMut(usize, vst3_host::controller_parameters::ControllerParameterRef<'_>),
) -> vst3_host::Result<usize> {
    let mut frame = vst3_host::controller_parameters::ControllerParameterQueryFrame::default();
    plugin.visit_parameters_bounded(MAX_PARAMETERS, &mut frame, visitor)
}

pub(super) fn parameter_descriptors(
    plugin: &Plugin,
) -> Result<Vec<Vst3ParameterDescriptor>, AudioProcessorHostError> {
    let mut parameters = Vec::new();
    visit_parameter_inventory(plugin, &mut |_, parameter| {
        let title = parameter.title_utf16();
        let end = title.iter().position(|code| *code == 0).unwrap_or(title.len());
        parameters.push(Vst3ParameterDescriptor {
            id: parameter.id(),
            name: String::from_utf16_lossy(&title[..end]),
            default_normalized: parameter.default_normalized(),
            step_count: parameter.step_count(),
            can_automate: parameter.can_automate(),
            read_only: parameter.is_read_only(),
            flags: parameter.flags(),
        });
    })
    .map_err(|error| invalid(format!("VST3 parameter query failed: {error}")))?;
    for parameter in &parameters {
        validate_parameter(parameter)?;
    }
    Ok(parameters)
}

fn validate_registration(
    registration: &Vst3PluginRegistration,
) -> Result<(), AudioProcessorHostError> {
    validate_main_audio_output_count(
        registration.audio_output_count,
        registration.main_audio_output_count,
    )?;
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
    registration
        .main_audio_input
        .validate_auxiliary(&registration.auxiliary_inputs)?;
    registration.event_bus_contract.validate()?;
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

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
fn native_editor_removal_failed(error: vst3_host::error::Error) -> AudioProcessorHostError {
    AudioProcessorHostError::WorkerFailed {
        operation: "native editor owner removal",
        detail: error.to_string(),
    }
}

/// Same native controller/view storage remains confined to its real main.
/// This role cannot reach the DSP processor or replacement/configuration APIs.
struct VstMainRole<'a> {
    editor: vst3_host::plugin::PluginEditorControl<'a>,
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    native_main: &'a std::rc::Rc<vst3_host::window::NativeEditorMainThread>,
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    native_parent: &'a mut Option<vst3_host::window::PluginEditorParent>,
    configuration_phase: &'a super::native_configuration_control::NativeConfigurationPhase,
    native_binding: &'a Option<crate::NativeAudioObjectBindReceipt>,
    native_author_delivery: &'a Option<native_author::PendingDelivery>,
    prepared_author_entry: &'a Option<author_state_entry::PreparedAuthorEntry>,
}

impl super::IsolatedAudioProcessorMainRole for VstMainRole<'_> {
    fn allocation_layouts(&self) -> Option<[crate::NativeHostBufferLayout; 2]> {
        Some([
            native_layout(std::alloc::Layout::for_value(self)),
            native_layout(self.editor.allocation_layout()),
        ])
    }
    fn service_native_main_thread(&mut self) -> Result<(), AudioProcessorHostError> {
        #[cfg(target_os = "windows")]
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
        #[cfg(target_os = "windows")]
        if let vst3_host::window::WindowsMessageStep::Quit { exit_code } = step {
            return Err(AudioProcessorHostError::WorkerFailed {
                operation: "native main message service",
                detail: format!("actual native WM_QUIT: {exit_code}"),
            });
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.native_main
            .with_native_scope(|| {
                if let Some(parent) = self.native_parent.as_mut() {
                    parent.service(&mut self.editor)
                } else {
                    #[cfg(target_os = "macos")]
                    self.native_main.dispatch_messages()?;
                    self.editor.service_controller()?;
                    self.editor.service_run_loop()
                }
            })
            .and_then(|result| result)
            .map_err(|error| {
                unavailable(format!("VST3 actual native main service failed: {error}"))
            })?;
        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        self.editor
            .service_controller()
            .and_then(|_| self.editor.service_run_loop())
            .map_err(|error| {
                unavailable(format!("VST3 actual controller service failed: {error}"))
            })?;
        Ok(())
    }

    fn finish_processing_scope(&mut self) -> Result<(), AudioProcessorHostError> {
        // Flush processor-to-controller display/DataExchange updates on main
        // only after DSP join. No synthetic author End, frame removal or native
        // state/processor replacement is inferred from temporary role lifetime.
        #[cfg(target_os = "macos")]
        {
            return self
                .native_main
                .with_native_scope(|| {
                    self.editor.service_controller().and_then(|_| self.editor.service_run_loop())
                })
                .and_then(|result| result)
                .map_err(|error| {
                    unavailable(format!(
                        "VST3 joined controller main finish failed: {error}"
                    ))
                });
        }
        #[cfg(not(target_os = "macos"))]
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
    ) -> Result<Option<crate::NativeObservedRequest>, AudioProcessorHostError> {
        let cohort = self.native_configuration_cohort()?;
        Ok(cohort.map(crate::NativeObservedRequest::Cohort).or(self
            .configuration_phase
            .blocked_request()
            .map(crate::NativeObservedRequest::RequestOnly)))
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
            && binding.expected_object.object_serial() == owner.serial()
            && self.native_author_delivery.is_none()
            && !self.prepared_author_entry.as_ref().is_some_and(|entry| entry.awaiting_ack()))
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
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
struct VstDspRole<'a> {
    native: vst3_host::plugin::PluginAudioProcessing<'a>,
    signal: VstSignalFields<'a>,
}

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
        use vst3_host::plugin::{
            PendingParameterFlushOutcome as Outcome, PendingParameterFlushRefusal as Refusal,
        };
        match self
            .native
            .flush_pending_parameter_changes()
            .map_err(|error| unavailable(format!("VST3 pending parameter flush failed: {error}")))?
        {
            Outcome::Flushed => return Ok(crate::NativeAudioAuthorParameterFlushOutcome::Flushed),
            Outcome::Refused(reason) => {
                let reason = match reason {
                    Refusal::Unsupported
                    | Refusal::NotProcessing
                    | Refusal::InvalidContext
                    | Refusal::RestartRequired => {
                        crate::NativeAudioAuthorRefusal::UnsupportedAdapter
                    }
                    Refusal::InvalidBatch => crate::NativeAudioAuthorRefusal::InvalidValue,
                    Refusal::DspFeedbackPending => {
                        crate::NativeAudioAuthorRefusal::DspFeedbackPending
                    }
                    Refusal::StorageBusy | Refusal::SourceChanged => {
                        crate::NativeAudioAuthorRefusal::SourceChanged
                    }
                    Refusal::StorageCapacity => crate::NativeAudioAuthorRefusal::ResourceLimit,
                    Refusal::PendingEvents
                    | Refusal::InvalidSource
                    | Refusal::SourceValuesPending
                    | Refusal::ExistingInput
                    | Refusal::ExistingOutput
                    | Refusal::StoragePoisoned => crate::NativeAudioAuthorRefusal::InvalidSource,
                };
                return Ok(crate::NativeAudioAuthorParameterFlushOutcome::Refused(
                    reason,
                ));
            }
            // Empty host input is not permission to skip the original Source
            // replay path's real flush and validation.
            Outcome::NoPending => {}
        }
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

fn native_worker_lane_profile_for_notes(
    parameters: &[Vst3ParameterDescriptor],
    notes: &crate::AudioProcessorNoteInputContract,
) -> Result<Option<crate::NativeWorkerLaneResourceProfile>, AudioProcessorHostError> {
    let layout = note_storage_layout(notes.limits.input_events)?;
    native_worker_lane_profile_for_parameters(parameters)?
        .map(|profile| {
            profile
                .with_adapter_note_input_storage(layout)
                .map_err(AudioProcessorHostError::from)
        })
        .transpose()
}

fn processing_audio_buffer_bounds(
    render: crate::AudioRenderContract,
    inputs: &AudioProcessorAuxiliaryInputContract,
) -> Result<vst3_host::audio::ProcessingResourceBounds, AudioProcessorHostError> {
    let mut channels = match render.channel_layout {
        AudioChannelLayout::Mono => 1,
        AudioChannelLayout::Stereo => 2,
        _ => return Err(invalid("VST3 channel layout is not admitted")),
    };
    for bus in &inputs.buses {
        let width = match bus.channel_layout {
            AudioChannelLayout::Mono => 1,
            AudioChannelLayout::Stereo => 2,
            _ => return Err(invalid("VST3 auxiliary channel layout is not admitted")),
        };
        channels = channels.max(width);
    }
    let buses = vst3_host::audio::MAX_OBSERVED_AUDIO_BUSES;
    let total = buses
        .checked_mul(2)
        .and_then(|count| count.checked_mul(channels))
        .ok_or_else(|| invalid("VST3 audio-buffer channel bound overflowed"))?;
    vst3_host::audio::ProcessingResourceBounds::new(buses, channels, total, render.max_block_frames)
        .map_err(|error| invalid(format!("VST3 audio-buffer bounds are invalid: {error}")))
}

fn native_worker_lane_profile_for_preparation(
    parameters: &[Vst3ParameterDescriptor],
    notes: &crate::AudioProcessorNoteInputContract,
    render: crate::AudioRenderContract,
    inputs: &AudioProcessorAuxiliaryInputContract,
) -> Result<Option<crate::NativeWorkerLaneResourceProfile>, AudioProcessorHostError> {
    native_worker_lane_profile_for_notes(parameters, notes)?
        .map(|profile| {
            let peak = processing_audio_buffer_bounds(render, inputs)?
                .host_buffer_peak_bytes()
                .map_err(|error| {
                    invalid(format!("VST3 audio-buffer request overflowed: {error}"))
                })?;
            profile
                .with_processing_audio_buffer_peak_bytes(peak)
                .map_err(AudioProcessorHostError::from)
        })
        .transpose()
}

pub(super) fn native_worker_lane_profile(
) -> Result<Option<crate::NativeWorkerLaneResourceProfile>, AudioProcessorHostError> {
    let [sdk_main, sdk_dsp] = vst3_host::plugin::PluginEditorProcessingSplit::allocation_layouts();
    let owners = crate::NativeWorkerLaneOwnedLayouts {
        adapter_resident: native_layout(std::alloc::Layout::new::<Vst3Worker>()),
        reader_slots: native_layout(std::alloc::Layout::new::<
            super::native_worker_ingress::NativeReaderSlots,
        >()),
        processing_slots: native_layout(std::alloc::Layout::new::<
            super::native_worker_processing::NativeProcessingSlots,
        >()),
        // Actual platform layouts are charged on every desktop. Stack/TLS,
        // committed OS pages and native framework allocations remain frontier.
        main_inline: native_layout(std::alloc::Layout::new::<(
            super::native_worker_ingress::NativeCommandIngress<'static>,
            super::worker::WorkerProcessContext<'static>,
            super::native_worker_processing::NativeDspDispatch<'static>,
            crate::NativeAudioObservations,
            vst3_host::controller_parameters::ControllerParameterQueryFrame,
            AudioBusQueryFrame,
            initial_parameter_values::InitialParameterFrame,
            parameter_observations::ParameterPresentationFrame,
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
        reply_storage: crate::NativeHostBufferLayout {
            bytes: byte_limit.bytes,
            alignment: align_of::<mondrian_core::NativeAudioParameterFact>(),
        },
        staged_seed: byte_limit,
    };
    Ok(Some(
        crate::NativeWorkerLaneResourceProfile::scoped_main_dsp(owners, control)
            .map_err(AudioProcessorHostError::from)?
            .with_adapter_note_input_storage(note_storage_layout(0)?)
            .map_err(AudioProcessorHostError::from)?,
    ))
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
    fn rebuild_native_state(
        &mut self,
        author_entry: Option<(&crate::NativeAudioAuthorStateEntryRequest<'_>, Vec<u8>)>,
    ) -> Result<(), AudioProcessorHostError> {
        self.require_delivered_parameter_input()?;
        let (author_request, retired_seed) = match author_entry {
            Some((request, accepted)) => {
                // This is the last old-owner check, before changing the seed,
                // binding, cursor or native incarnation. A borrowed request
                // cannot bypass the actual retained receipt and Source.
                if accepted.as_slice() != request.configuration
                    || self.prepared_author_entry.is_some()
                    || !self.author_binding_current(request.binding)?
                    || !self.author_entry_source_closed(request)?
                {
                    return Err(AudioProcessorHostError::StateEntry(
                        "VST3 author state-entry source changed before retirement".to_owned(),
                    ));
                }
                self.last_end = None;
                (Some(request), self.payload.state.replace(accepted))
            }
            None => (None, None),
        };
        // Pure refusal above preserves the author lease and cold seed. Native
        // retirement starts here; subsequent partial failure is terminal.
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
        prepare_loaded_metadata(
            &mut fresh,
            &Vst3PluginRegistration {
                class_id: self.payload.class_id.clone(),
                vendor: self.payload.vendor.clone(),
                binary_path: self.payload.binary_path.clone(),
                binary_sha256: self.payload.binary_sha256,
                execution_contract: self.contract,
                auxiliary_inputs: self.auxiliary_inputs.clone(),
                parameters: self.payload.parameters.clone(),
                main_audio_input: self.payload.main_audio_input,
                audio_output_count: self.payload.audio_output_count,
                main_audio_output_count: self.payload.main_audio_output_count,
                event_bus_contract: self.payload.event_bus_contract,
            },
            self.render.channel_layout,
        )?;
        fresh.start_processing().map_err(|error| {
            AudioProcessorHostError::StateEntry(format!("VST3 state-entry start failed: {error}"))
        })?;
        validate_started_execution(&fresh, self.contract)?;
        let fresh_owner = fresh.native_editor_attachment_owner();
        if !self.note_input_contract.ports.is_empty()
            && !matches!((previous_owner, fresh_owner), (Some(old), Some(new)) if new.serial() > old.serial())
        {
            return Err(AudioProcessorHostError::StateEntry(
                "VST3 Note entry did not construct a fresh producer".to_owned(),
            ));
        }
        // Install before inspecting callbacks. Refused fresh source/cohorts stay
        // on this actual object rather than being dropped/reloaded to erase them.
        self.plugin = Some(fresh);
        if let Some(request) = author_request {
            let object = self.native_audio_object_observation()?.ok_or_else(|| {
                AudioProcessorHostError::StateEntry(
                    "VST3 author state-entry has no fresh native incarnation".to_owned(),
                )
            })?;
            if object.process_id() != request.binding.expected_object.process_id()
                || object.object_serial() <= request.binding.expected_object.object_serial()
                || !matches!((previous_owner, fresh_owner), (Some(old), Some(new))
                    if old.serial() == request.binding.expected_object.object_serial()
                        && new.serial() == object.object_serial())
            {
                return Err(AudioProcessorHostError::StateEntry(
                    "VST3 author state-entry did not create a fresh same-child incarnation"
                        .to_owned(),
                ));
            }
        }
        if (author_request.is_some()
            || matches!((previous_owner, fresh_owner), (Some(old), Some(new))
                if new.serial() > old.serial()))
            && !self.service_fresh_parameter_values(author_request)
        {
            return Err(AudioProcessorHostError::StateEntry(
                "VST3 fresh initial parameter source was refused".to_owned(),
            ));
        }
        drop(retired_seed);
        Ok(())
    }
}

#[cfg(test)]
mod retained_parameter_contracts {
    use super::*;

    fn descriptor(title: &[u16; 128], default: f64, steps: i32) -> Vst3ParameterDescriptor {
        let end = title.iter().position(|code| *code == 0).unwrap_or(title.len());
        Vst3ParameterDescriptor {
            id: 7,
            name: String::from_utf16_lossy(&title[..end]),
            default_normalized: default,
            step_count: steps,
            can_automate: true,
            read_only: false,
            flags: 1,
        }
    }

    #[test]
    fn retained_parameter_validation_preserves_owned_lossy_text_and_field_rules() {
        let mut titles = vec![
            [0; 128],
            [b'a' as u16; 128],
            [0x00E9; 128], // Exactly 256 UTF-8 bytes, without a terminating NUL.
            [0x20AC; 128], // 384 bytes: character count is not the text bound.
            [0xD800; 128], // The old conversion replaces unpaired surrogates.
            std::array::from_fn(|i| if i % 2 == 0 { 0xD83D } else { 0xDE00 }),
        ];
        let mut title = [0x00E9; 128];
        title[127] = 0xD800; // Lossy replacement makes 257 UTF-8 bytes.
        titles.push(title);
        title = [0xD800; 128];
        title[0] = b'G' as u16;
        title[1] = 0;
        title[2] = b'\n' as u16; // Everything after the first NUL is ignored.
        titles.push(title);
        title[1] = b'\n' as u16;
        titles.push(title);
        for title in &titles {
            for default in [-0.0, 0.0, 0.5, 1.0, -0.5, 1.5, f64::NAN, f64::INFINITY] {
                for steps in [-1, 0, i32::MAX] {
                    assert_eq!(
                        retained_parameter_fields_valid(title, default, steps),
                        validate_parameter(&descriptor(title, default, steps)).is_ok(),
                        "borrowed validation must preserve the original descriptor rules",
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod bus_contract_tests {
    use super::*;
    #[test]
    fn retained_vst3_native_owned_profile_does_not_invent_successful_parameter_delivery() {
        let delivered = [None, Some(0.5), Some(0.75)];
        let before = delivered;
        assert!(retained_constants_match_successful_delivery(
            &[None, Some(0.5), None],
            &delivered
        ));
        assert!(retained_constants_match_successful_delivery(
            &[None, None, None],
            &delivered
        ));
        assert!(!retained_constants_match_successful_delivery(
            &[Some(0.25), None, None],
            &delivered
        ));
        assert!(!retained_constants_match_successful_delivery(
            &[None, Some(0.25), None],
            &delivered
        ));
        assert!(!retained_constants_match_successful_delivery(
            &[None, Some(f64::NAN), None],
            &delivered
        ));
        assert!(!retained_constants_match_successful_delivery(
            &[None, None],
            &delivered
        ));
        assert_eq!(
            delivered, before,
            "None may not erase or synthesize delivery evidence"
        );
    }
    use vst3_host::audio::{AudioBusConfig, NativeAudioBusConstraint, NativeAudioBusInfo};

    fn assert_borrowed_matches_owned(
        native: &NativeAudioBusInfoSet,
        layout: &AudioBusLayout,
        arrangements: &BusArrangements,
        channels: AudioChannelLayout,
        admitted_main: Vst3MainAudioInput,
        admitted_auxiliary: &AudioProcessorAuxiliaryInputContract,
        admitted_outputs: u8,
    ) {
        let owned = audio_inputs_from_metadata(native, layout, arrangements, channels, "retained")
            .and_then(|(main, auxiliary, main_outputs)| {
                if layout.inputs.iter().chain(&layout.outputs).any(|bus| !bus.active) {
                    return Err(invalid(
                        "VST3 retained activation left an admitted bus inactive",
                    ));
                }
                Ok(main == admitted_main
                    && &auxiliary == admitted_auxiliary
                    && native.outputs.len() == usize::from(admitted_outputs)
                    && main_outputs == 1)
            });
        let scalar = |buses: &[NativeAudioBusInfo]| {
            buses
                .iter()
                .map(|bus| NativeAudioBusConstraint {
                    role: bus.role,
                    channel_count: bus.channel_count,
                })
                .collect::<Vec<_>>()
        };
        let inputs = scalar(&native.inputs);
        let outputs = scalar(&native.outputs);
        let observed = AudioBusObservationRef {
            native_inputs: &inputs,
            native_outputs: &outputs,
            layout_inputs: &layout.inputs,
            layout_outputs: &layout.outputs,
            input_arrangements: &arrangements.inputs,
            output_arrangements: &arrangements.outputs,
        };
        assert_eq!(
            audio_bus_observation_matches(
                &observed,
                channels,
                admitted_main,
                admitted_auxiliary,
                admitted_outputs,
                1,
            ),
            owned,
            "borrowed shape, activation and identity must preserve the cold owned contract",
        );
    }

    #[test]
    fn vst3_mixed_inputs_preserve_exact_speaker_meaning_in_cold_and_retained_metadata() {
        for render in [AudioChannelLayout::Mono, AudioChannelLayout::Stereo] {
            let (mut native, mut layout, mut arrangements) = metadata();
            let render_speakers = if render == AudioChannelLayout::Mono {
                SpeakerArrangement::MONO
            } else {
                SpeakerArrangement::STEREO
            };
            for (index, width) in [render.channel_count(), 1, 2].into_iter().enumerate() {
                native.inputs[index].channel_count = width;
                layout.inputs[index].channel_count = width;
                layout.inputs[index].active = true;
            }
            native.outputs[0].channel_count = render.channel_count();
            layout.outputs[0].channel_count = render.channel_count();
            layout.outputs[0].active = true;
            arrangements.inputs = vec![
                render_speakers,
                SpeakerArrangement::MONO,
                SpeakerArrangement::STEREO,
            ];
            arrangements.outputs[0] = render_speakers;
            let (main, inputs, _) =
                audio_inputs_from_metadata(&native, &layout, &arrangements, render, "test")
                    .unwrap();
            assert_eq!(main, Vst3MainAudioInput::Present);
            assert_eq!(
                inputs
                    .buses
                    .iter()
                    .map(|b| (b.bus_key.as_str(), b.channel_layout))
                    .collect::<Vec<_>>(),
                vec![
                    ("vst3.audio-input.1", AudioChannelLayout::Mono),
                    ("vst3.audio-input.2", AudioChannelLayout::Stereo)
                ]
            );
            assert_borrowed_matches_owned(
                &native,
                &layout,
                &arrangements,
                render,
                main,
                &inputs,
                1,
            );
            let mut stale = inputs.clone();
            stale.buses[0].channel_layout = AudioChannelLayout::Stereo;
            assert_borrowed_matches_owned(&native, &layout, &arrangements, render, main, &stale, 1);
            for change in 0..5 {
                let mut n = native.clone();
                let mut l = layout.clone();
                let mut a = arrangements.clone();
                match change {
                    0 => n.inputs[1].channel_count = 2,
                    1 => l.inputs[1].channel_count = 2,
                    2 => a.inputs[2] = SpeakerArrangement::STEREO_SURROUND,
                    3 => n.inputs[1].role = AudioBusRole::Main,
                    4 => {
                        a.outputs[0] = if render == AudioChannelLayout::Mono {
                            SpeakerArrangement::STEREO
                        } else {
                            SpeakerArrangement::MONO
                        }
                    }
                    _ => unreachable!(),
                }
                assert!(audio_inputs_from_metadata(&n, &l, &a, render, "test").is_err());
                assert_borrowed_matches_owned(&n, &l, &a, render, main, &inputs, 1);
            }
        }
    }

    #[test]
    fn vst3_mixed_input_resource_geometry_covers_the_widest_admitted_port() {
        use vst3_host::audio::ProcessingResourceBounds;
        let render = crate::AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Mono,
            max_block_frames: 4,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: usize::MAX,
            public_output_lookahead_budget_frames: usize::MAX,
            compensation_delay_scratch_budget_bytes: usize::MAX,
        };
        let empty = AudioProcessorAuxiliaryInputContract::default();
        let mut inputs = AudioProcessorAuxiliaryInputContract {
            buses: vec![AudioProcessorAuxiliaryInputBusContract {
                bus_key: "vst3.audio-input.1".to_owned(),
                channel_layout: AudioChannelLayout::Stereo,
            }],
        };
        let mono = processing_audio_buffer_bounds(render, &empty).unwrap();
        assert_eq!(
            mono,
            ProcessingResourceBounds::new(32, 1, 64, render.max_block_frames).unwrap()
        );
        let mixed = processing_audio_buffer_bounds(render, &inputs).unwrap();
        assert_eq!(
            mixed,
            ProcessingResourceBounds::new(32, 2, 128, render.max_block_frames).unwrap()
        );
        assert!(mixed.host_buffer_peak_bytes().unwrap() > mono.host_buffer_peak_bytes().unwrap());
        inputs.buses[0].channel_layout = AudioChannelLayout::discrete(2).unwrap();
        assert!(processing_audio_buffer_bounds(render, &inputs).is_err());
    }

    #[test]
    fn retained_vst3_bus_comparison_preserves_owned_shape_activation_and_identity() {
        for channels in [
            AudioChannelLayout::Mono,
            AudioChannelLayout::Stereo,
            AudioChannelLayout::discrete(2).unwrap(),
        ] {
            for count in [0, 1, 3, 32, 33] {
                let speakers = if channels == AudioChannelLayout::Mono {
                    SpeakerArrangement::MONO
                } else {
                    SpeakerArrangement::STEREO
                };
                let main = if count == 0 {
                    Vst3MainAudioInput::Absent
                } else {
                    Vst3MainAudioInput::Present
                };
                let auxiliary = AudioProcessorAuxiliaryInputContract {
                    buses: (main.port_offset()..count)
                        .map(|index| AudioProcessorAuxiliaryInputBusContract {
                            bus_key: format!("vst3.audio-input.{index}"),
                            channel_layout: channels,
                        })
                        .collect(),
                };
                for change in 0..20 {
                    let info = NativeAudioBusInfo {
                        role: AudioBusRole::Main,
                        name: "Display name is not bus identity".to_owned(),
                        channel_count: channels.channel_count(),
                        default_active: false,
                    };
                    let mut native = NativeAudioBusInfoSet {
                        inputs: (0..count)
                            .map(|index| NativeAudioBusInfo {
                                role: if index == 0 {
                                    AudioBusRole::Main
                                } else {
                                    AudioBusRole::Auxiliary
                                },
                                ..info.clone()
                            })
                            .collect(),
                        outputs: vec![info],
                    };
                    let config = AudioBusConfig {
                        channel_count: channels.channel_count(),
                        active: true,
                    };
                    let mut layout =
                        AudioBusLayout { inputs: vec![config; count], outputs: vec![config] };
                    let mut arrangements = BusArrangements {
                        inputs: vec![speakers; count],
                        outputs: vec![speakers],
                    };
                    let mut admitted_main = main;
                    let mut admitted = auxiliary.clone();
                    match change {
                        0 => {}
                        1 => native.outputs[0].role = AudioBusRole::Auxiliary,
                        2 => native.outputs[0].channel_count += 1,
                        3 => layout.outputs[0].channel_count += 1,
                        4 => arrangements.outputs[0] = SpeakerArrangement::STEREO_SURROUND,
                        5 => {
                            layout.inputs.push(config);
                        }
                        6 => {
                            arrangements.inputs.push(speakers);
                        }
                        7 => {
                            native.outputs.clear();
                        }
                        8 => {
                            native.outputs.push(native.outputs[0].clone());
                        }
                        9 => layout.outputs[0].active = false,
                        10 => {
                            // Shape error must beat inactivity and identity mismatch.
                            native.outputs[0].role = AudioBusRole::Auxiliary;
                            layout.outputs[0].active = false;
                            admitted_main = Vst3MainAudioInput::Absent;
                        }
                        11 => {
                            admitted_main = if main == Vst3MainAudioInput::Present {
                                Vst3MainAudioInput::Absent
                            } else {
                                Vst3MainAudioInput::Present
                            }
                        }
                        12 => {
                            admitted.buses.push(AudioProcessorAuxiliaryInputBusContract {
                                bus_key: "vst3.audio-input.31".to_owned(),
                                channel_layout: channels,
                            });
                        }
                        13 => {
                            admitted.buses.pop();
                        }
                        14 if count > 0 => native.inputs[0].role = AudioBusRole::Auxiliary,
                        15 if count > 0 => layout.inputs[count - 1].active = false,
                        16 if count > 1 => native.inputs[1].role = AudioBusRole::Main,
                        17 if !admitted.buses.is_empty() => {
                            admitted.buses[0].bus_key = "vst3.audio-input.01".to_owned()
                        }
                        18 if !admitted.buses.is_empty() => {
                            admitted.buses[0].channel_layout =
                                AudioChannelLayout::discrete(channels.channel_count_u8()).unwrap()
                        }
                        19 => {
                            native.outputs[0].name.clear();
                            native.outputs[0].default_active = true;
                        }
                        _ => {}
                    }
                    assert_borrowed_matches_owned(
                        &native,
                        &layout,
                        &arrangements,
                        channels,
                        admitted_main,
                        &admitted,
                        1,
                    );
                }
            }
        }
    }

    #[test]
    fn retained_vst3_bus_keys_require_exact_canonical_native_indices() {
        for index in [0, 1, 31, usize::MAX] {
            assert!(canonical_audio_input_key_matches(
                &format!("vst3.audio-input.{index}"),
                index
            ));
        }
        for key in [
            "vst3.audio-input.",
            "vst3.audio-input.01",
            "vst3.audio-input.+1",
            "vst3.audio-input.-1",
            "vst3.audio-input. 1",
            "vst3.audio-input.1 ",
            "vst3.audio-input.１",
            "vst3.audio-input.١",
            "vst3.audio-input.2",
            "vst3.audio-input.184467440737095516160",
            "vst3.audio-input.1\0",
            "VST3.audio-input.1",
        ] {
            assert!(
                !canonical_audio_input_key_matches(key, 1),
                "noncanonical key: {key:?}"
            );
        }
    }

    #[test]
    fn vst3_outputs_keep_bounded_ordered_identity_and_authenticate_before_binary_access() {
        let render = crate::AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 4,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: usize::MAX,
            public_output_lookahead_budget_frames: usize::MAX,
            compensation_delay_scratch_budget_bytes: usize::MAX,
        };
        for count in [1, 3, 32] {
            let payload = Vst3Payload {
                schema_version: PAYLOAD_VERSION,
                main_audio_input: Vst3MainAudioInput::Present,
                audio_output_count: count,
                main_audio_output_count: 1,
                event_bus_contract: Vst3EventBusContractV1::empty(),
                binary_path: std::env::temp_dir().join("unopened-vst3-output-shape.dll"),
                class_id: "0123456789ABCDEF0123456789ABCDEF".into(),
                vendor: None,
                binary_sha256: [1; 32],
                state: None,
                parameters: vec![],
                parameter_ids: vec![],
            };
            let outputs = audio_output_contract(count, render.channel_layout);
            assert_eq!(outputs.buses.len(), usize::from(count - 1));
            for (index, bus) in outputs.buses.iter().enumerate() {
                assert_eq!(bus.bus_key, format!("vst3.audio-output.{}", index + 1));
            }
            let request = || IsolatedAudioProcessorWorkerPrepareRequest {
                musical_time_reference: None,
                native_lane: None,
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
                auxiliary_outputs: outputs.clone(),
                note_input_contract: Default::default(),
                parameter_ids: vec![],
                render_contract: render,
            };
            assert!(validated_preparation_payload(&request()).is_ok());
            for change in 0..5 {
                let mut bad = request();
                match change {
                    0 => {
                        bad.auxiliary_outputs.buses.push(AudioProcessorAuxiliaryOutputBusContract {
                            bus_key: format!("vst3.audio-output.{count}"),
                            channel_layout: render.channel_layout,
                        })
                    }
                    1 if count > 1 => {
                        bad.auxiliary_outputs.buses.pop();
                    }
                    2 if count > 1 => {
                        bad.auxiliary_outputs.buses[0].bus_key = "vst3.audio-output.01".into()
                    }
                    3 if count > 1 => {
                        bad.auxiliary_outputs.buses[0].channel_layout = AudioChannelLayout::Mono
                    }
                    4 if count > 1 => bad.auxiliary_outputs.buses.reverse(),
                    _ => continue,
                }
                assert!(validated_preparation_payload(&bad).is_err());
                assert!(
                    matches!(
                        Vst3AudioProcessorWorkerFactory.prepare_native(bad),
                        Err(AudioProcessorHostError::InvalidContract(_))
                    ),
                    "metadata refusal must precede access to the nonexistent DLL"
                );
            }
        }
        for count in [0, 33, u8::MAX] {
            assert!(validate_audio_output_count(count).is_err());
        }
    }

    #[test]
    fn vst3_output_census_checks_every_role_layout_activation_and_retained_count() {
        for count in [1, 3, 32] {
            let (mut native, mut layout, mut arrangements) = metadata();
            layout.inputs.iter_mut().for_each(|bus| bus.active = true);
            let main = native.outputs[0].clone();
            native.outputs = (0..count)
                .map(|index| NativeAudioBusInfo {
                    role: if index == 0 {
                        AudioBusRole::Main
                    } else {
                        AudioBusRole::Auxiliary
                    },
                    ..main.clone()
                })
                .collect();
            layout.outputs = vec![AudioBusConfig { channel_count: 2, active: true }; count];
            arrangements.outputs = vec![SpeakerArrangement::STEREO; count];
            let (main_input, auxiliary, _) = audio_inputs_from_metadata(
                &native,
                &layout,
                &arrangements,
                AudioChannelLayout::Stereo,
                "before_activation",
            )
            .unwrap();
            assert_borrowed_matches_owned(
                &native,
                &layout,
                &arrangements,
                AudioChannelLayout::Stereo,
                main_input,
                &auxiliary,
                count as u8,
            );
            assert_borrowed_matches_owned(
                &native,
                &layout,
                &arrangements,
                AudioChannelLayout::Stereo,
                main_input,
                &auxiliary,
                if count == 1 { 2 } else { 1 },
            );
            for change in 0..7 {
                let mut n = native.clone();
                let mut l = layout.clone();
                let mut a = arrangements.clone();
                match change {
                    0 => n.outputs[count - 1].channel_count = 1,
                    1 => l.outputs[count - 1].channel_count = 1,
                    2 => a.outputs[count - 1] = SpeakerArrangement::STEREO_SURROUND,
                    3 => l.outputs[count - 1].active = false,
                    4 => n.outputs[0].role = AudioBusRole::Auxiliary,
                    5 if count > 1 => n.outputs[1].role = AudioBusRole::Main,
                    6 => {
                        n.outputs.clear();
                        l.outputs.clear();
                        a.outputs.clear();
                    }
                    _ => continue,
                }
                assert_borrowed_matches_owned(
                    &n,
                    &l,
                    &a,
                    AudioChannelLayout::Stereo,
                    main_input,
                    &auxiliary,
                    count as u8,
                );
            }
        }
        let (mut native, mut layout, mut arrangements) = metadata();
        let aux = NativeAudioBusInfo {
            role: AudioBusRole::Auxiliary,
            ..native.outputs[0].clone()
        };
        native.outputs.resize(33, aux);
        layout.outputs.resize(33, layout.outputs[0]);
        arrangements.outputs.resize(33, SpeakerArrangement::STEREO);
        assert!(audio_inputs_from_metadata(
            &native,
            &layout,
            &arrangements,
            AudioChannelLayout::Stereo,
            "before_activation",
        )
        .is_err());
    }

    #[test]
    fn vst3_native_shapes_compose_main_presence_and_output_prefix_without_relabeling() {
        for channels in [AudioChannelLayout::Mono, AudioChannelLayout::Stereo] {
            let speakers = if channels == AudioChannelLayout::Mono {
                SpeakerArrangement::MONO
            } else {
                SpeakerArrangement::STEREO
            };
            for main in [Vst3MainAudioInput::Absent, Vst3MainAudioInput::Present] {
                for auxiliaries in [0, 1, 31] {
                    for (outputs, main_outputs) in [(1, 1), (3, 1), (3, 2), (3, 3), (32, 32)] {
                        let (mut native, _, _) = metadata();
                        let bus = native.outputs[0].clone();
                        native.inputs = (0..auxiliaries + main.port_offset())
                            .map(|index| NativeAudioBusInfo {
                                role: if index < main.port_offset() {
                                    AudioBusRole::Main
                                } else {
                                    AudioBusRole::Auxiliary
                                },
                                channel_count: channels.channel_count(),
                                ..bus.clone()
                            })
                            .collect();
                        native.outputs = (0..outputs)
                            .map(|index| NativeAudioBusInfo {
                                role: if index < main_outputs {
                                    AudioBusRole::Main
                                } else {
                                    AudioBusRole::Auxiliary
                                },
                                channel_count: channels.channel_count(),
                                ..bus.clone()
                            })
                            .collect();
                        let active = AudioBusConfig {
                            channel_count: channels.channel_count(),
                            active: true,
                        };
                        let layout = AudioBusLayout {
                            inputs: vec![active; native.inputs.len()],
                            outputs: vec![active; outputs],
                        };
                        let arrangements = BusArrangements {
                            inputs: vec![speakers; native.inputs.len()],
                            outputs: vec![speakers; outputs],
                        };
                        let (actual_main, auxiliary, actual_prefix) = audio_inputs_from_metadata(
                            &native,
                            &layout,
                            &arrangements,
                            channels,
                            "before_activation",
                        )
                        .unwrap();
                        assert_eq!(actual_main, main);
                        assert_eq!(actual_prefix as usize, main_outputs);
                        assert_eq!(auxiliary.buses.len(), auxiliaries);
                        main.validate_auxiliary(&auxiliary).unwrap();
                        for (index, bus) in auxiliary.buses.iter().enumerate() {
                            assert_eq!(
                                bus.bus_key,
                                format!("vst3.audio-input.{}", index + main.port_offset())
                            );
                        }
                        let constraints = |buses: &[NativeAudioBusInfo]| {
                            buses
                                .iter()
                                .map(|bus| NativeAudioBusConstraint {
                                    role: bus.role,
                                    channel_count: bus.channel_count,
                                })
                                .collect::<Vec<_>>()
                        };
                        let inputs = constraints(&native.inputs);
                        let output_rows = constraints(&native.outputs);
                        let observed = AudioBusObservationRef {
                            native_inputs: &inputs,
                            native_outputs: &output_rows,
                            layout_inputs: &layout.inputs,
                            layout_outputs: &layout.outputs,
                            input_arrangements: &arrangements.inputs,
                            output_arrangements: &arrangements.outputs,
                        };
                        assert!(audio_bus_observation_matches(
                            &observed,
                            channels,
                            main,
                            &auxiliary,
                            outputs as u8,
                            main_outputs as u8
                        )
                        .unwrap());
                        if outputs > 1 {
                            let different_prefix = if main_outputs == 1 { 2 } else { 1 };
                            assert!(
                                !audio_bus_observation_matches(
                                    &observed,
                                    channels,
                                    main,
                                    &auxiliary,
                                    outputs as u8,
                                    different_prefix
                                )
                                .unwrap(),
                                "same output count cannot hide a changed native role boundary"
                            );
                        }
                        let changed_main = if main == Vst3MainAudioInput::Present {
                            Vst3MainAudioInput::Absent
                        } else {
                            Vst3MainAudioInput::Present
                        };
                        assert!(!audio_bus_observation_matches(
                            &observed,
                            channels,
                            changed_main,
                            &auxiliary,
                            outputs as u8,
                            main_outputs as u8
                        )
                        .unwrap());
                    }
                }
            }
        }
    }

    #[test]
    fn vst3_native_shapes_refuse_late_main_roles_and_bound_auxiliary_only_inputs() {
        let (mut native, mut layout, mut arrangements) = metadata();
        layout.inputs.iter_mut().for_each(|bus| bus.active = true);
        // All three outputs have valid stereo storage, but Main after Aux is illegal.
        let main = native.outputs[0].clone();
        let mut auxiliary = main.clone();
        auxiliary.role = AudioBusRole::Auxiliary;
        native.outputs = vec![main.clone(), auxiliary.clone(), main.clone()];
        layout.outputs = vec![layout.outputs[0]; 3];
        arrangements.outputs = vec![SpeakerArrangement::STEREO; 3];
        let error = audio_inputs_from_metadata(
            &native,
            &layout,
            &arrangements,
            AudioChannelLayout::Stereo,
            "before_activation",
        )
        .unwrap_err();
        assert!(error.to_string().contains("reason=role direction=output index=2"));
        native.inputs[1].channel_count = 1;
        let earlier = audio_inputs_from_metadata(
            &native,
            &layout,
            &arrangements,
            AudioChannelLayout::Stereo,
            "before_activation",
        )
        .unwrap_err();
        assert!(earlier.to_string().contains("reason=native_channels direction=input index=1"));
        native.inputs = vec![auxiliary; 32];
        layout.inputs = vec![layout.outputs[0]; 32];
        arrangements.inputs = vec![SpeakerArrangement::STEREO; 32];
        assert_eq!(
            audio_inputs_from_metadata(
                &native,
                &layout,
                &arrangements,
                AudioChannelLayout::Stereo,
                "before_activation"
            )
            .unwrap_err(),
            invalid("VST3 auxiliary inputs exceed the existing 31-port bound")
        );
        for (total, prefix) in [(0, 0), (1, 0), (3, 4), (33, 1)] {
            assert!(validate_main_audio_output_count(total, prefix).is_err());
        }
    }

    #[test]
    fn full_vst3_parameter_capacity_keeps_nonautomatable_input_and_complete_shared_storage() {
        let mut parameters = (0..2855)
            .map(|id| Vst3ParameterDescriptor {
                id,
                name: format!("Parameter{id}"),
                default_normalized: 0.5,
                step_count: 0,
                can_automate: id % 2 == 0,
                read_only: false,
                flags: u32::from(id % 2 == 0),
            })
            .collect::<Vec<_>>();
        let profile = native_worker_lane_profile_for_parameters(&parameters).unwrap().unwrap();
        let full =
            vst3_host::initial_parameter_input::InitialParameterStorage::for_parameter_count(2855)
                .unwrap();
        assert_eq!(
            profile.processor_input_storage(),
            (full.vector_bytes, full.com_payload_bytes),
            "all writable values, including nonautomatable ones, require original COM storage"
        );
        let shared =
            super::super::protocol::SharedBlockLayout::uniform(512, 2, 0, parameters.len())
                .unwrap();
        assert_eq!(shared.event_capacity, 512 * 2855);
        assert_eq!(
            shared.total_bytes - shared.events_offset,
            23_388_160,
            "retain the complete B*L*16 reservation"
        );
        let template = parameters[0].clone();
        parameters
            .extend((2855..4096).map(|id| Vst3ParameterDescriptor { id, ..template.clone() }));
        let maximum = native_worker_lane_profile_for_parameters(&parameters).unwrap().unwrap();
        let full =
            vst3_host::initial_parameter_input::InitialParameterStorage::for_parameter_count(4096)
                .unwrap();
        assert_eq!(
            maximum.processor_input_storage(),
            (full.vector_bytes, full.com_payload_bytes)
        );
        parameters.push(Vst3ParameterDescriptor { id: 4096, ..parameters[0].clone() });
        assert!(native_worker_lane_profile_for_parameters(&parameters).is_err());
    }
    #[test]
    fn retained_vst3_bus_scratch_is_charged_once_in_original_main_inline() {
        use std::alloc::Layout;
        let profile = native_worker_lane_profile_for_parameters(&[Vst3ParameterDescriptor {
            id: 0,
            name: "Gain".to_owned(),
            default_normalized: 0.5,
            step_count: 0,
            can_automate: true,
            read_only: false,
            flags: 1,
        }])
        .unwrap()
        .expect("scoped desktop lane");
        let original = Layout::new::<(
            super::super::native_worker_ingress::NativeCommandIngress<'static>,
            super::super::worker::WorkerProcessContext<'static>,
            super::super::native_worker_processing::NativeDspDispatch<'static>,
            crate::NativeAudioObservations,
            vst3_host::controller_parameters::ControllerParameterQueryFrame,
            initial_parameter_values::InitialParameterFrame,
            parameter_observations::ParameterPresentationFrame,
        )>();
        let current = Layout::new::<(
            super::super::native_worker_ingress::NativeCommandIngress<'static>,
            super::super::worker::WorkerProcessContext<'static>,
            super::super::native_worker_processing::NativeDspDispatch<'static>,
            crate::NativeAudioObservations,
            vst3_host::controller_parameters::ControllerParameterQueryFrame,
            AudioBusQueryFrame,
            initial_parameter_values::InitialParameterFrame,
            parameter_observations::ParameterPresentationFrame,
        )>();
        assert_eq!(profile.owning_layouts().main_inline, native_layout(current));
        let mut old_owners = profile.owning_layouts();
        old_owners.main_inline = native_layout(original);
        let (vectors, com) = profile.processor_input_storage();
        assert!(
            vectors > 0 && com > 0,
            "retain the actual processor-input backing"
        );
        let before = crate::NativeWorkerLaneResourceProfile::scoped_main_dsp(
            old_owners,
            profile.control_overlap(),
        )
        .unwrap()
        .with_adapter_note_input_storage(profile.adapter_note_input_storage())
        .unwrap()
        .with_processor_input_storage(vectors, com)
        .unwrap();
        let delta = current.size().checked_sub(original.size()).unwrap();
        assert!(
            delta > 0,
            "charge the actual tuple layout, including padding"
        );
        assert_eq!(
            profile.known_storage_limit_bytes() - before.known_storage_limit_bytes(),
            delta
        );
        assert_eq!(
            profile.additional_session_reservation_bytes()
                - before.additional_session_reservation_bytes(),
            delta
        );
        assert_eq!(
            profile.requested_stack_reservation_bytes(),
            before.requested_stack_reservation_bytes()
        );
    }

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
            schema_version: PAYLOAD_VERSION,
            main_audio_input: Vst3MainAudioInput::Present,
            audio_output_count: 1,
            main_audio_output_count: 1,
            event_bus_contract: Vst3EventBusContractV1::empty(),
            class_id: "61921CF43A2B4A68AE5AE08241D85E22".to_owned(),
            vendor: None,
            binary_path: std::env::temp_dir().join("unopened-vst3-profile-control.dll"),
            binary_sha256: [1; 32],
            state: None,
            parameters: vec![parameter],
            parameter_ids: vec![0],
        };
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: None,
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
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
            native_worker_lane_profile_for_preparation(
                &payload.parameters,
                request.note_input_contract(),
                request.render_contract(),
                request.auxiliary_inputs(),
            )
            .unwrap()
        );
        {
            assert_ne!(expected, request.native_lane);
            let prepared = expected.unwrap();
            let peak = prepared.processing_audio_buffer_peak_bytes();
            assert!(peak > 0);
            let mut request = request;
            let original_render = request.render_contract;
            let mut larger_render = original_render;
            larger_render.max_block_frames += 1;
            let larger = native_worker_lane_profile_for_preparation(
                &payload.parameters,
                request.note_input_contract(),
                larger_render,
                request.auxiliary_inputs(),
            )
            .unwrap()
            .unwrap();
            assert!(larger.processing_audio_buffer_peak_bytes() > peak);
            assert_eq!(larger.owning_layouts(), prepared.owning_layouts());
            assert_eq!(
                larger.processor_input_storage(),
                prepared.processor_input_storage()
            );
            assert_eq!(
                larger.adapter_note_input_storage(),
                prepared.adapter_note_input_storage()
            );
            assert_eq!(
                larger.additional_session_reservation_bytes()
                    - prepared.additional_session_reservation_bytes(),
                larger.processing_audio_buffer_peak_bytes() - peak,
                "actual requested frames change only the existing peak obligation"
            );
            for wrong in [
                None,
                native_worker_lane_profile().unwrap(),
                Some(prepared.with_processing_audio_buffer_peak_bytes(peak - 1).unwrap()),
                Some(larger),
            ] {
                // A missing, old, one-byte-short or other-geometry profile must
                // fail before even fingerprinting this nonexistent native path.
                request.native_lane = wrong;
                let refused = Vst3AudioProcessorWorkerFactory.prepare_native(
                    IsolatedAudioProcessorWorkerPrepareRequest {
                        musical_time_reference: request.musical_time_reference,
                        native_lane: request.native_lane,
                        payload: request.payload.clone(),
                        plugin_execution_contract: request.plugin_execution_contract,
                        auxiliary_inputs: request.auxiliary_inputs.clone(),
                        auxiliary_outputs: request.auxiliary_outputs.clone(),
                        note_input_contract: request.note_input_contract.clone(),
                        parameter_ids: request.parameter_ids.clone(),
                        render_contract: original_render,
                    },
                );
                assert!(matches!(refused,
                    Err(AudioProcessorHostError::InvalidContract(message))
                        if message == "VST3 native storage request differs from prepared metadata"));
            }
        }
        assert!(native_worker_lane_profile_for_parameters(&vec![
            payload.parameters[0].clone();
            MAX_PARAMETERS + 1
        ])
        .is_err());
    }

    #[test]
    fn payload_v6_seed_keeps_audio_shape_and_event_contract_and_refuses_legacy_or_unknown_metadata()
    {
        use super::super::{Vst3EventBusContractRow, Vst3EventBusContractRows, Vst3EventBusRole};
        let event_bus_contract = Vst3EventBusContractV1 {
            schema_version: 1,
            inputs: Vst3EventBusContractRows::try_from_slice(&[Vst3EventBusContractRow {
                native_index: 0,
                role: Vst3EventBusRole::Main,
                channel_count: 16,
                default_active: true,
                host_active: false,
            }])
            .unwrap(),
            outputs: Vst3EventBusContractV1::empty().outputs,
        };
        let payload = Vst3Payload {
            schema_version: PAYLOAD_VERSION,
            main_audio_input: Vst3MainAudioInput::Present,
            audio_output_count: 3,
            main_audio_output_count: 2,
            event_bus_contract,
            binary_path: std::env::temp_dir().join("never-opened-event-contract.dll"),
            class_id: "0123456789ABCDEF0123456789ABCDEF".into(),
            vendor: None,
            binary_sha256: [1; 32],
            state: None,
            parameters: vec![],
            parameter_ids: vec![],
        };
        let original = serde_json::to_value(&payload).unwrap();
        for state in [
            serde_json::Value::Null,
            serde_json::json!(""),
            serde_json::json!("AQI="),
        ] {
            let mut before = original.clone();
            before["state"] = state;
            let updated =
                payload_with_configuration_seed(&serde_json::to_vec(&before).unwrap(), &[3, 4])
                    .unwrap();
            let decoded = decode_payload(&updated).unwrap();
            assert_eq!(decoded.main_audio_input, Vst3MainAudioInput::Present);
            assert_eq!(decoded.audio_output_count, 3);
            assert_eq!(decoded.main_audio_output_count, 2);
            assert_eq!(decoded.event_bus_contract, event_bus_contract);
            assert_eq!(decoded.state, Some(vec![3, 4]));
            let mut expected = before;
            expected["state"] = serde_json::json!("AwQ=");
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&updated).unwrap(),
                expected
            );
        }
        let mut absent = original.clone();
        absent["main_audio_input"] = serde_json::json!("absent");
        let seeded =
            payload_with_configuration_seed(&serde_json::to_vec(&absent).unwrap(), &[5, 6])
                .unwrap();
        let decoded = decode_payload(&seeded).unwrap();
        assert_eq!(decoded.main_audio_input, Vst3MainAudioInput::Absent);
        assert_eq!(decoded.event_bus_contract, event_bus_contract);
        assert_eq!(decoded.state, Some(vec![5, 6]));
        for field in [
            "schema_version",
            "event_bus_contract",
            "main_audio_input",
            "audio_output_count",
            "main_audio_output_count",
        ] {
            let mut missing = original.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(decode_payload(&serde_json::to_vec(&missing).unwrap()).is_err());
        }
        for (field, value) in [
            ("schema_version", serde_json::json!(1)),
            ("schema_version", serde_json::json!(2)),
            ("schema_version", serde_json::json!(3)),
            ("schema_version", serde_json::json!(4)),
            ("schema_version", serde_json::json!(5)),
            ("schema_version", serde_json::json!(7)),
            ("audio_output_count", serde_json::json!(0)),
            ("audio_output_count", serde_json::json!(33)),
            ("audio_output_count", serde_json::json!(256)),
            ("audio_output_count", serde_json::json!(null)),
            ("audio_output_count", serde_json::json!("3")),
            ("main_audio_output_count", serde_json::json!(0)),
            ("main_audio_output_count", serde_json::json!(4)),
            ("main_audio_output_count", serde_json::json!(256)),
            ("main_audio_output_count", serde_json::json!(null)),
            ("main_audio_output_count", serde_json::json!("1")),
            ("main_audio_input", serde_json::json!(null)),
            ("main_audio_input", serde_json::json!("unknown")),
            ("unknown", serde_json::json!(1)),
        ] {
            let mut bad = original.clone();
            bad[field] = value;
            let bytes = serde_json::to_vec(&bad).unwrap();
            assert!(decode_payload(&bytes).is_err());
            assert!(payload_with_configuration_seed(&bytes, &[]).is_err());
        }
        assert!(payload_with_configuration_seed(
            &serde_json::to_vec(&original).unwrap(),
            &vec![0; MAX_STATE_BYTES + 1]
        )
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
        let expected = audio_inputs_from_metadata(
            &native,
            &layout,
            &arrangements,
            AudioChannelLayout::Stereo,
            "before_activation",
        )
        .expect("native sidechains");
        assert_eq!(
            expected.1.buses.iter().map(|bus| bus.bus_key.as_str()).collect::<Vec<_>>(),
            ["vst3.audio-input.1", "vst3.audio-input.2"]
        );
        native.inputs[1].name = "Localized renamed duplicate".into();
        layout.inputs[1].active = true;
        assert_eq!(
            audio_inputs_from_metadata(
                &native,
                &layout,
                &arrangements,
                AudioChannelLayout::Stereo,
                "before_activation"
            )
            .expect("display and activation are not bus identity"),
            expected
        );
        native.inputs[1].role = AudioBusRole::Main;
        assert!(audio_inputs_from_metadata(
            &native,
            &layout,
            &arrangements,
            AudioChannelLayout::Stereo,
            "before_activation",
        )
        .is_err());
        native.inputs[1].role = AudioBusRole::Auxiliary;
        arrangements.inputs[1] = SpeakerArrangement::STEREO_SURROUND;
        assert!(
            audio_inputs_from_metadata(
                &native,
                &layout,
                &arrangements,
                AudioChannelLayout::Stereo,
                "before_activation"
            )
            .is_err(),
            "two channels do not imply L/R stereo"
        );
        arrangements.inputs[1] = SpeakerArrangement::STEREO;
        native.outputs[0].role = AudioBusRole::Auxiliary;
        assert!(audio_inputs_from_metadata(
            &native,
            &layout,
            &arrangements,
            AudioChannelLayout::Stereo,
            "before_activation",
        )
        .is_err());
    }

    #[test]
    fn vst3_bus_refusal_reports_existing_scalars_and_actual_validation_stage() {
        let (mut native, mut layout, mut arrangements) = metadata();
        native.outputs[0].name = "not diagnostic identity".into();
        native.outputs[0].default_active = false;
        layout.outputs[0].channel_count = 4;
        layout.outputs[0].active = false;
        arrangements.outputs[0] = SpeakerArrangement::MONO;
        for (stage, expected) in [
            ("before_activation", "VST3 host requires optional Main input zero, typed Mono/Stereo Auxiliary inputs and a nonempty Main output prefix followed by Auxiliary outputs with the exact render layout; stage=before_activation reason=prepared_channels direction=output index=0 native_role=Main expected_role=Main native_channels=2 prepared_channels=4 host_active=false actual_arrangement_raw=0000000000080000 expected_arrangement_raw=0000000000000003 expected_layout=Speakers(AudioSpeakerSet(3)) expected_channels=2"),
            ("after_activation", "VST3 host requires optional Main input zero, typed Mono/Stereo Auxiliary inputs and a nonempty Main output prefix followed by Auxiliary outputs with the exact render layout; stage=after_activation reason=prepared_channels direction=output index=0 native_role=Main expected_role=Main native_channels=2 prepared_channels=4 host_active=false actual_arrangement_raw=0000000000080000 expected_arrangement_raw=0000000000000003 expected_layout=Speakers(AudioSpeakerSet(3)) expected_channels=2"),
        ] {
            assert_eq!(
                audio_inputs_from_metadata(&native, &layout, &arrangements,
                    AudioChannelLayout::Stereo, stage).unwrap_err(),
                invalid(expected),
            );
        }
        // The borrowed path has the same facts, but labels its real retained
        // census rather than claiming either activation-stage observation.
        assert_borrowed_matches_owned(
            &native,
            &layout,
            &arrangements,
            AudioChannelLayout::Stereo,
            Vst3MainAudioInput::Absent,
            &AudioProcessorAuxiliaryInputContract::default(),
            1,
        );
    }

    #[test]
    fn vst3_bus_refusal_keeps_first_field_direction_and_native_index_order() {
        let (native, layout, arrangements) = metadata();
        for (change, expected) in [
            (
                0,
                "reason=role direction=input index=1 native_role=Main expected_role=Auxiliary",
            ),
            (1, "reason=native_channels direction=input index=1"),
            (2, "reason=prepared_channels direction=input index=1"),
            (3, "reason=arrangement direction=input index=1"),
            (4, "reason=native_channels direction=input index=2"),
            (5, "reason=role direction=output index=0"),
        ] {
            let mut n = native.clone();
            let mut l = layout.clone();
            let mut a = arrangements.clone();
            // A later output failure may not preempt an earlier input/index.
            n.outputs[0].role = AudioBusRole::Auxiliary;
            n.inputs[2].channel_count = 4;
            if change < 4 {
                a.inputs[1] = SpeakerArrangement::STEREO_SURROUND;
                if change < 3 {
                    l.inputs[1].channel_count = 4;
                }
                if change < 2 {
                    n.inputs[1].channel_count = 1;
                }
                if change == 0 {
                    n.inputs[1].role = AudioBusRole::Main;
                }
            } else if change == 5 {
                n.inputs[2].channel_count = 2;
            }
            let error =
                audio_inputs_from_metadata(&n, &l, &a, AudioChannelLayout::Stereo, "retained")
                    .unwrap_err();
            let AudioProcessorHostError::InvalidContract(detail) = error else {
                panic!("shape refusal must retain InvalidContract");
            };
            assert!(detail.contains(expected), "{detail}");
            // Inactivity and admitted-identity mismatch are deliberately also
            // present. They must remain later than the same first shape error.
            assert_borrowed_matches_owned(
                &n,
                &l,
                &a,
                AudioChannelLayout::Stereo,
                Vst3MainAudioInput::Absent,
                &AudioProcessorAuxiliaryInputContract::default(),
                1,
            );
        }
        let (mut native, layout, arrangements) = metadata();
        native.inputs[0].role = AudioBusRole::Auxiliary;
        native.outputs.clear();
        assert_eq!(
            audio_inputs_from_metadata(
                &native,
                &layout,
                &arrangements,
                AudioChannelLayout::Stereo,
                "before_activation"
            )
            .unwrap_err(),
            invalid("VST3 requires bounded inputs and a nonempty Main output prefix with bounded additional outputs"),
            "inventory extent refusal must still precede bus-field diagnostics",
        );
    }

    #[test]
    fn vst3_zero_audio_input_requires_empty_native_prepared_and_arrangement_inputs() {
        for channel_layout in [AudioChannelLayout::Mono, AudioChannelLayout::Stereo] {
            let (mut native, mut layout, mut arrangements) = metadata();
            native.inputs.truncate(1);
            layout.inputs.truncate(1);
            arrangements.inputs.truncate(1);
            let channels = channel_layout.channel_count();
            native.inputs[0].channel_count = channels;
            native.outputs[0].channel_count = channels;
            layout.inputs[0].channel_count = channels;
            layout.outputs[0].channel_count = channels;
            let speakers = if channels == 1 {
                SpeakerArrangement::MONO
            } else {
                SpeakerArrangement::STEREO
            };
            arrangements.inputs[0] = speakers;
            arrangements.outputs[0] = speakers;
            let present = audio_inputs_from_metadata(
                &native,
                &layout,
                &arrangements,
                channel_layout,
                "before_activation",
            )
            .unwrap();
            assert_eq!(present.0, Vst3MainAudioInput::Present);
            assert!(present.1.buses.is_empty());

            native.inputs.clear();
            layout.inputs.clear();
            arrangements.inputs.clear();
            let absent = audio_inputs_from_metadata(
                &native,
                &layout,
                &arrangements,
                channel_layout,
                "before_activation",
            )
            .unwrap();
            assert_eq!(absent.0, Vst3MainAudioInput::Absent);
            assert!(absent.1.buses.is_empty());
            assert_ne!(
                present, absent,
                "empty auxiliaries cannot hide a main-input shape change"
            );
            for wrong in 0..6 {
                let mut n = native.clone();
                let mut l = layout.clone();
                let mut a = arrangements.clone();
                match wrong {
                    0 => n.inputs.push(n.outputs[0].clone()),
                    1 => l.inputs.push(l.outputs[0]),
                    2 => a.inputs.push(speakers),
                    3 => {
                        n.outputs.clear();
                        l.outputs.clear();
                        a.outputs.clear();
                    }
                    4 => n.outputs[0].role = AudioBusRole::Auxiliary,
                    _ => {
                        let mut auxiliary = n.outputs[0].clone();
                        auxiliary.role = AudioBusRole::Auxiliary;
                        n.outputs.push(auxiliary);
                        n.outputs.push(n.outputs[0].clone());
                        l.outputs.extend([l.outputs[0]; 2]);
                        a.outputs.extend([speakers; 2]);
                    }
                }
                assert!(audio_inputs_from_metadata(
                    &n,
                    &l,
                    &a,
                    channel_layout,
                    "before_activation"
                )
                .is_err());
            }
            native.inputs.push(NativeAudioBusInfo {
                role: AudioBusRole::Auxiliary,
                name: "Aux only".into(),
                channel_count: channels,
                default_active: true,
            });
            layout.inputs.push(layout.outputs[0]);
            arrangements.inputs.push(speakers);
            let auxiliary_only = audio_inputs_from_metadata(
                &native,
                &layout,
                &arrangements,
                channel_layout,
                "before_activation",
            )
            .expect("an absent Main with actual Auxiliary input is admitted");
            assert_eq!(auxiliary_only.0, Vst3MainAudioInput::Absent);
            assert_eq!(auxiliary_only.1.buses[0].bus_key, "vst3.audio-input.0");
            assert_ne!(
                auxiliary_only, absent,
                "zero-input and Auxiliary-only stay distinct"
            );
        }
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
                AudioChannelLayout::Stereo,
                "before_activation",
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

#[cfg(test)]
mod musical_effect_tests {
    use super::super::native_configuration_control::NativeConfigurationPhase;
    use super::super::protocol::SharedParameterEvent;
    use super::*;
    use mondrian_core::{MusicalTimeReference, TimelineTime};
    use vst3_host::audio::{AudioBusConfig, ProcessMusicalReference, ProcessTimelineContext};

    fn render() -> crate::AudioRenderContract {
        crate::AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 4,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: usize::MAX,
            public_output_lookahead_budget_frames: usize::MAX,
            compensation_delay_scratch_budget_bytes: usize::MAX,
        }
    }

    fn payload() -> Vst3Payload {
        Vst3Payload {
            schema_version: PAYLOAD_VERSION,
            main_audio_input: Vst3MainAudioInput::Present,
            audio_output_count: 1,
            main_audio_output_count: 1,
            event_bus_contract: Vst3EventBusContractV1::empty(),
            binary_path: std::env::temp_dir().join("unopened-musical-effect.dll"),
            class_id: "0123456789ABCDEF0123456789ABCDEF".into(),
            vendor: None,
            binary_sha256: [1; 32],
            state: None,
            parameters: vec![Vst3ParameterDescriptor {
                id: 0,
                name: "Gain".into(),
                default_normalized: 0.5,
                step_count: 0,
                can_automate: true,
                read_only: false,
                flags: 1,
            }],
            parameter_ids: vec![0],
        }
    }

    fn buffers() -> BusAudioBuffers {
        let mut buffers = BusAudioBuffers::new(
            &AudioBusLayout {
                inputs: vec![AudioBusConfig { channel_count: 2, active: true }],
                outputs: vec![AudioBusConfig { channel_count: 2, active: true }],
            },
            4,
            48_000.0,
        );
        for bus in buffers.inputs.iter_mut().chain(&mut buffers.outputs) {
            for channel in &mut bus.channels {
                channel.fill(7.0);
            }
        }
        buffers
    }

    #[derive(Default)]
    struct Native {
        calls: Vec<&'static str>,
        parameters: Vec<(u32, f64, i32)>,
        timeline: Option<(ProcessTimelineContext, Option<ProcessMusicalReference>)>,
        transport_activity: Option<mondrian_core::TimelineTransportActivity>,
        reject_timeline: bool,
        reject_notes: bool,
        zero_input: bool,
        multiple_outputs: bool,
        nonfinite_output: Option<usize>,
        reject_process: bool,
        notes: Vec<PluginEvent>,
    }

    impl VstNativeSignal for Native {
        fn deliver_parameter_at(
            &mut self,
            id: u32,
            value: f64,
            offset: i32,
        ) -> vst3_host::error::Result<()> {
            self.calls.push("parameter");
            self.parameters.push((id, value, offset));
            Ok(())
        }
        fn deliver_timeline(
            &mut self,
            context: ProcessTimelineContext,
            reference: Option<ProcessMusicalReference>,
            activity: Option<mondrian_core::TimelineTransportActivity>,
        ) -> vst3_host::error::Result<()> {
            self.calls.push("timeline");
            if self.reject_timeline {
                return Err(vst3_host::error::Error::MusicalTime(
                    vst3_host::error::MusicalTimeError::SampleRate,
                ));
            }
            self.timeline = Some((context, reference));
            self.transport_activity = activity;
            Ok(())
        }
        fn process_buses(&mut self, buffers: &mut BusAudioBuffers) -> vst3_host::error::Result<()> {
            self.calls.push("process");
            if self.multiple_outputs {
                for (index, bus) in buffers.outputs.iter_mut().enumerate() {
                    for (channel, samples) in bus.channels.iter_mut().enumerate() {
                        samples.fill(16.0 * (index + 1) as f32 + 0.5 * channel as f32);
                    }
                }
                if let Some(index) = self.nonfinite_output {
                    buffers.outputs[index].channels[0][0] = f32::NAN;
                }
                return if self.reject_process {
                    Err(vst3_host::error::Error::ProcessFailed(-123))
                } else {
                    Ok(())
                };
            }
            if self.zero_input {
                assert!(
                    buffers.inputs.is_empty(),
                    "zero-input processing has no dummy bus"
                );
                for samples in buffers.outputs[0].channels[0].iter_mut() {
                    *samples = 0.375;
                }
                for samples in buffers.outputs[0].channels[1].iter_mut() {
                    *samples = -0.125;
                }
                return Ok(());
            }
            for (input, output) in
                buffers.inputs[0].channels.iter().zip(&mut buffers.outputs[0].channels)
            {
                output.copy_from_slice(input);
            }
            Ok(())
        }
        fn process_note_buses(
            &mut self,
            buffers: &mut BusAudioBuffers,
            events: &[PluginEvent],
        ) -> vst3_host::error::Result<()> {
            self.notes.extend_from_slice(events);
            if self.reject_notes {
                self.calls.push("notes");
                return Err(vst3_host::error::Error::ProcessFailed(-123));
            }
            self.process_buses(buffers)?;
            self.calls.pop();
            self.calls.push("notes");
            Ok(())
        }
    }

    fn process_author_parameter_block(
        payload: &Vst3Payload,
        native: &mut Native,
        scratch: &mut BusAudioBuffers,
        values: &mut Vec<Option<f64>>,
        last_end: &mut Option<i64>,
        events: &[SharedParameterEvent],
    ) -> Result<(), AudioProcessorHostError> {
        let ids: Vec<_> = payload
            .parameters
            .iter()
            .map(|parameter| parameter.parameter_id().unwrap())
            .collect();
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let mut pcm = [11.0; 8];
        let start = last_end.expect("the test starts at an entered signal cursor");
        VstSignalFields {
            last_parameter_values: values,
            configuration_phase: &NativeConfigurationPhase::Admitted,
            payload,
            render: render(),
            buffers: scratch,
            auxiliary_inputs: &auxiliary,
            last_end,
            note_input_contract: &Default::default(),
            note_events: &mut Vec::new(),
        }
        .process(
            native,
            IsolatedAudioProcessorWorkerBlock::new(
                start,
                4,
                render(),
                &mut pcm,
                &auxiliary,
                &[],
                &ids,
                events,
            ),
        )
    }

    #[test]
    fn vst3_author_constant_points_are_implicit_only_after_success_and_until_native_ownership() {
        // The retained evidence uses canonical author values, including discrete
        // indices; the native queue always receives the descriptor's normalized value.
        for (steps, first, changed, first_normalized, changed_normalized) in
            [(0, 0.25, 0.75, 0.25, 0.75), (4, 1.0, 3.0, 0.25, 0.75)]
        {
            let mut payload = payload();
            payload.parameters[0].step_count = steps;
            let mut native = Native::default();
            let mut scratch = buffers();
            let mut values = vec![None];
            let mut last_end = Some(0);
            let point =
                |offset, value| SharedParameterEvent { lane: 0, sample_offset: offset, value };
            let cases = [
                (
                    vec![point(0, first)],
                    vec![(0, first_normalized, 0)],
                    Some(first),
                ),
                (vec![point(0, first)], vec![], Some(first)),
                (
                    vec![point(0, changed)],
                    vec![(0, changed_normalized, 0)],
                    Some(changed),
                ),
                (
                    vec![point(0, first), point(3, changed)],
                    vec![(0, first_normalized, 0), (0, changed_normalized, 3)],
                    Some(changed),
                ),
                (
                    vec![point(0, changed), point(3, changed)],
                    vec![(0, changed_normalized, 0), (0, changed_normalized, 3)],
                    Some(changed),
                ),
                (
                    vec![point(3, changed)],
                    vec![(0, changed_normalized, 3)],
                    Some(changed),
                ),
                (vec![], vec![], None),
                (
                    vec![point(0, changed)],
                    vec![(0, changed_normalized, 0)],
                    Some(changed),
                ),
            ];
            for (index, (events, expected, retained)) in cases.into_iter().enumerate() {
                native.calls.clear();
                native.parameters.clear();
                process_author_parameter_block(
                    &payload,
                    &mut native,
                    &mut scratch,
                    &mut values,
                    &mut last_end,
                    &events,
                )
                .unwrap();
                assert_eq!(native.parameters, expected);
                assert_eq!(native.calls.first(), Some(&"timeline"));
                assert_eq!(native.calls.last(), Some(&"process"));
                assert_eq!(values, [retained]);
                assert_eq!(last_end, Some((index as i64 + 1) * 4));
            }
        }
    }

    #[test]
    fn vst3_failed_dsp_or_output_validation_never_commits_parameter_evidence() {
        let payload = payload();
        let changed = [SharedParameterEvent { lane: 0, sample_offset: 0, value: 0.75 }];
        for initial in [None, Some(0.25)] {
            for empty in [false, true] {
                for reject in [false, true] {
                    let mut native = Native {
                        multiple_outputs: true,
                        reject_process: reject,
                        nonfinite_output: (!reject).then_some(0),
                        ..Default::default()
                    };
                    let mut scratch = buffers();
                    let mut values = vec![initial];
                    let mut last_end = Some(0);
                    assert!(process_author_parameter_block(
                        &payload,
                        &mut native,
                        &mut scratch,
                        &mut values,
                        &mut last_end,
                        if empty { &[] } else { &changed },
                    )
                    .is_err());
                    assert_eq!(values, [initial]);
                    assert_eq!(last_end, Some(0));
                    native.reject_process = false;
                    native.nonfinite_output = None;
                    // Failed empty input must not revoke prior evidence; a failed
                    // first/change delivery must not make its retry implicit.
                    let retry_value = if empty { initial.unwrap_or(0.75) } else { 0.75 };
                    let retry =
                        [SharedParameterEvent { lane: 0, sample_offset: 0, value: retry_value }];
                    process_author_parameter_block(
                        &payload,
                        &mut native,
                        &mut scratch,
                        &mut values,
                        &mut last_end,
                        &retry,
                    )
                    .unwrap();
                    let expected_count = if empty {
                        usize::from(initial.is_none())
                    } else {
                        2
                    };
                    assert_eq!(native.parameters.len(), expected_count);
                    assert!(native.parameters.iter().all(|&(id, value, offset)| {
                        id == 0 && value == retry_value && offset == 0
                    }));
                    assert_eq!(values, [Some(retry_value)]);
                    assert_eq!(last_end, Some(4));
                }
            }
        }
    }

    #[test]
    fn vst3_implicit_constants_still_validate_the_original_author_value() {
        let payload = payload();
        for invalid_value in [-1.0, 2.0, f64::NAN] {
            let mut native = Native::default();
            let mut scratch = buffers();
            let mut values = vec![Some(invalid_value)];
            let mut last_end = Some(0);
            let events = [SharedParameterEvent { lane: 0, sample_offset: 0, value: invalid_value }];
            assert!(process_author_parameter_block(
                &payload,
                &mut native,
                &mut scratch,
                &mut values,
                &mut last_end,
                &events,
            )
            .is_err());
            assert_eq!(native.calls, ["timeline"]);
            assert!(native.parameters.is_empty());
            assert_eq!(values[0].unwrap().to_bits(), invalid_value.to_bits());
            assert_eq!(last_end, Some(0));
        }
    }

    #[test]
    fn vst3_implicit_constants_still_consume_the_original_event_capacity() {
        let mut payload = payload();
        let prototype = payload.parameters[0].clone();
        payload.parameters = (0..MAX_EVENTS_PER_BLOCK)
            .map(|id| Vst3ParameterDescriptor { id: id as u32, ..prototype.clone() })
            .collect();
        payload.parameter_ids = payload.parameters.iter().map(|parameter| parameter.id).collect();
        let mut events: Vec<_> = (0..MAX_EVENTS_PER_BLOCK)
            .map(|lane| SharedParameterEvent { lane: lane as u32, sample_offset: 0, value: 0.25 })
            .collect();
        let mut native = Native::default();
        let mut scratch = buffers();
        let mut values = vec![Some(0.25); MAX_EVENTS_PER_BLOCK];
        let mut last_end = Some(0);
        // Exactly 4096 unchanged constants fit even though none is re-enqueued.
        process_author_parameter_block(
            &payload,
            &mut native,
            &mut scratch,
            &mut values,
            &mut last_end,
            &events,
        )
        .unwrap();
        assert_eq!(native.calls, ["timeline", "process"]);
        assert!(native.parameters.is_empty());
        // One extra original point exceeds the same budget. The last lane is
        // now multi-point, while all preceding lanes remain implicit constants.
        events.push(SharedParameterEvent {
            lane: (MAX_EVENTS_PER_BLOCK - 1) as u32,
            sample_offset: 3,
            value: 0.75,
        });
        native.calls.clear();
        let error = process_author_parameter_block(
            &payload,
            &mut native,
            &mut scratch,
            &mut values,
            &mut last_end,
            &events,
        )
        .unwrap_err();
        assert_eq!(
            error,
            AudioProcessorHostError::Process("VST3 parameter event capacity exceeded".to_owned())
        );
        assert_eq!(native.calls, ["timeline", "parameter"]);
        assert_eq!(
            native.parameters,
            [(MAX_EVENTS_PER_BLOCK as u32 - 1, 0.25, 0)]
        );
        assert!(values.iter().all(|value| *value == Some(0.25)));
        assert_eq!(last_end, Some(4));
    }

    fn note_reference() -> MusicalTimeReference {
        MusicalTimeReference::new(
            TimelineTime::new(-19, 7).unwrap(),
            TimelineTime::new(3, 4).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn vst3_multiple_output_signal_validates_destinations_before_native_input_and_all_sources_before_publication(
    ) {
        let mut payload = payload();
        payload.audio_output_count = 3;
        let ids = [payload.parameters[0].parameter_id().unwrap()];
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let outputs = audio_output_contract(3, AudioChannelLayout::Stereo);
        let render = render();
        // Missing/short destinations must precede timeline, parameter, Note or
        // process exposure. Failed native PCM must leave all destinations intact.
        for (short, nonfinite, reject) in [
            (true, None, false),
            (false, Some(0), false),
            (false, Some(1), false),
            (false, Some(2), false),
            (false, None, true),
        ] {
            let mut scratch = BusAudioBuffers::new(
                &AudioBusLayout {
                    inputs: vec![AudioBusConfig { channel_count: 2, active: true }],
                    outputs: vec![AudioBusConfig { channel_count: 2, active: true }; 3],
                },
                4,
                48_000.0,
            );
            let mut native = Native {
                multiple_outputs: true,
                nonfinite_output: nonfinite,
                reject_process: reject,
                ..Default::default()
            };
            let mut pcm = [11.0; 8];
            let mut output_pcm = vec![13.0; if short { 15 } else { 16 }];
            let before = output_pcm.clone();
            let mut values = vec![None];
            let mut last_end = Some(0);
            let result = VstSignalFields {
                last_parameter_values: &mut values,
                configuration_phase: &NativeConfigurationPhase::Admitted,
                payload: &payload,
                render,
                buffers: &mut scratch,
                auxiliary_inputs: &auxiliary,
                last_end: &mut last_end,
                note_input_contract: &Default::default(),
                note_events: &mut Vec::new(),
            }
            .process(
                &mut native,
                IsolatedAudioProcessorWorkerBlock::new(
                    0,
                    4,
                    render,
                    &mut pcm,
                    &auxiliary,
                    &[],
                    &ids,
                    &[],
                )
                .with_auxiliary_outputs(&outputs, &mut output_pcm),
            );
            assert!(result.is_err());
            assert_eq!(pcm, [11.0; 8]);
            assert_eq!(output_pcm, before);
            assert_eq!(last_end, Some(0));
            assert_eq!(values, [None]);
            if short {
                assert!(native.calls.is_empty());
            } else {
                assert_eq!(native.calls, ["timeline", "process"]);
            }
        }
        let mut scratch = BusAudioBuffers::new(
            &AudioBusLayout {
                inputs: vec![AudioBusConfig { channel_count: 2, active: true }],
                outputs: vec![AudioBusConfig { channel_count: 2, active: true }; 3],
            },
            4,
            48_000.0,
        );
        let mut native = Native { multiple_outputs: true, ..Default::default() };
        let mut values = vec![None];
        let mut last_end = Some(0);
        let mut start = 0;
        for frames in [4, 1, 3] {
            let mut pcm = vec![11.0; frames * 2];
            let mut output_pcm = vec![13.0; frames * 4];
            VstSignalFields {
                last_parameter_values: &mut values,
                configuration_phase: &NativeConfigurationPhase::Admitted,
                payload: &payload,
                render,
                buffers: &mut scratch,
                auxiliary_inputs: &auxiliary,
                last_end: &mut last_end,
                note_input_contract: &Default::default(),
                note_events: &mut Vec::new(),
            }
            .process(
                &mut native,
                IsolatedAudioProcessorWorkerBlock::new(
                    start,
                    frames,
                    render,
                    &mut pcm,
                    &auxiliary,
                    &[],
                    &ids,
                    &[],
                )
                .with_auxiliary_outputs(&outputs, &mut output_pcm),
            )
            .unwrap();
            assert!(pcm.chunks_exact(2).all(|frame| frame == [16.0, 16.5]));
            assert!(output_pcm[..frames * 2].chunks_exact(2).all(|frame| frame == [32.0, 32.5]));
            assert!(output_pcm[frames * 2..].chunks_exact(2).all(|frame| frame == [48.0, 48.5]));
            start += frames as i64;
            assert_eq!(last_end, Some(start));
        }
        assert_eq!(
            native.calls,
            ["timeline", "process", "timeline", "process", "timeline", "process"]
        );
    }

    fn note_metadata(channels: u32) -> Vst3EventBusContractV1 {
        use super::super::vst3_processing_metadata::{
            Vst3EventBusContractRow, Vst3EventBusContractRows, Vst3EventBusRole,
        };
        Vst3EventBusContractV1 {
            schema_version: 1,
            inputs: Vst3EventBusContractRows::try_from_slice(&[Vst3EventBusContractRow {
                native_index: 0,
                role: Vst3EventBusRole::Main,
                channel_count: channels,
                default_active: true,
                host_active: true,
            }])
            .unwrap(),
            outputs: Vst3EventBusContractRows::try_from_slice(&[]).unwrap(),
        }
    }

    #[test]
    fn vst3_zero_audio_input_cold_admission_requires_some_and_explicit_formal_note_input() {
        use crate::{
            AudioProcessorInsertionPoint, AudioProcessorOccurrence, AudioProcessorOccurrenceOwner,
        };
        use mondrian_core::{AudioProcessorInstanceId, ProgramOutputId};
        let payload = payload();
        let definition = AudioProcessorDefinitionRef::Vst3 {
            class_id: payload.class_id.clone(),
            vendor: None,
            schema_version: 1,
            binary_sha256: Some(payload.binary_sha256),
        };
        let resolver = Vst3AudioProcessorSpecResolver::new(
            std::env::temp_dir().join("unlaunched-zero-input-helper.exe"),
            [Vst3PluginRegistration {
                class_id: payload.class_id,
                vendor: None,
                binary_path: payload.binary_path,
                binary_sha256: payload.binary_sha256,
                execution_contract: AudioProcessorExecutionContract::new(
                    0,
                    AudioProcessorTail::None,
                    true,
                    false,
                    true,
                    0,
                )
                .unwrap(),
                main_audio_input: Vst3MainAudioInput::Absent,
                audio_output_count: 1,
                main_audio_output_count: 1,
                auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
                parameters: payload.parameters,
                event_bus_contract: note_metadata(1),
            }],
        )
        .unwrap();
        let occurrence = AudioProcessorOccurrence {
            instance_id: AudioProcessorInstanceId::new(),
            owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
            insertion: AudioProcessorInsertionPoint::PreFader,
        };
        let parameters = BTreeMap::new();
        for reference in [None, Some(note_reference())] {
            for requested in [false, true] {
                let result = resolver.resolve(
                    AudioProcessorPrepareRequest::new(
                        occurrence,
                        &definition,
                        &parameters,
                        None,
                        render(),
                    )
                    .with_musical_time_reference(reference)
                    .with_note_input_requested(requested),
                );
                if reference.is_none() || !requested {
                    let expected = if reference.is_none() && requested {
                        "VST3 Note input requires a musical time reference"
                    } else {
                        "VST3 zero audio input requires musical reference and requested Note input"
                    };
                    assert_eq!(result.unwrap_err(), unavailable(expected));
                    continue;
                }
                let spec = result.unwrap();
                assert!(
                    spec.native_lane.is_some(),
                    "every desktop admits and charges its native lane"
                );
                let decoded = decode_payload(&spec.preparation_payload).unwrap();
                assert_eq!(decoded.main_audio_input, Vst3MainAudioInput::Absent);
                assert_eq!(spec.note_input_contract.ports.len(), 1);
                assert_eq!(spec.note_input_contract.ports[0].channel_count, 1);
                let child = || IsolatedAudioProcessorWorkerPrepareRequest {
                    musical_time_reference: spec.musical_time_reference,
                    payload: spec.preparation_payload.clone(),
                    plugin_execution_contract: spec.plugin_execution_contract,
                    auxiliary_inputs: spec.auxiliary_inputs.clone(),
                    auxiliary_outputs: spec.auxiliary_outputs.clone(),
                    note_input_contract: spec.note_input_contract.clone(),
                    parameter_ids: spec.parameter_ids.clone(),
                    render_contract: spec.render_contract,
                    native_lane: spec.native_lane,
                };
                assert!(Vst3AudioProcessorWorkerFactory
                    .native_worker_lane_resources_for_preparation(&child())
                    .is_ok());
                for missing in [0, 1] {
                    let mut refused = child();
                    if missing == 0 {
                        refused.musical_time_reference = None;
                    } else {
                        refused.note_input_contract = Default::default();
                    }
                    assert!(
                        matches!(Vst3AudioProcessorWorkerFactory.prepare_native(refused),
                        Err(AudioProcessorHostError::Unavailable(ref reason))
                            if reason == "VST3 zero audio input requires musical reference and requested Note input")
                    );
                }
            }
        }
    }

    #[test]
    fn vst3_auxiliary_only_preparation_preserves_note_policy_and_native_key_zero() {
        use crate::{
            AudioProcessorInsertionPoint, AudioProcessorOccurrence, AudioProcessorOccurrenceOwner,
        };
        use mondrian_core::{AudioProcessorInstanceId, ProgramOutputId};
        let mut payload = payload();
        payload.main_audio_input = Vst3MainAudioInput::Absent;
        payload.event_bus_contract = note_metadata(1);
        let auxiliary = AudioProcessorAuxiliaryInputContract {
            buses: vec![AudioProcessorAuxiliaryInputBusContract {
                bus_key: "vst3.audio-input.0".into(),
                channel_layout: AudioChannelLayout::Stereo,
            }],
        };
        let definition = AudioProcessorDefinitionRef::Vst3 {
            class_id: payload.class_id.clone(),
            vendor: None,
            schema_version: 1,
            binary_sha256: Some(payload.binary_sha256),
        };
        let resolver = Vst3AudioProcessorSpecResolver::new(
            std::env::temp_dir().join("unlaunched-auxiliary-only-helper.exe"),
            [Vst3PluginRegistration {
                class_id: payload.class_id.clone(),
                vendor: None,
                binary_path: payload.binary_path.clone(),
                binary_sha256: payload.binary_sha256,
                execution_contract: AudioProcessorExecutionContract::new(
                    0,
                    AudioProcessorTail::None,
                    true,
                    false,
                    true,
                    0,
                )
                .unwrap(),
                main_audio_input: Vst3MainAudioInput::Absent,
                audio_output_count: 1,
                main_audio_output_count: 1,
                auxiliary_inputs: auxiliary.clone(),
                parameters: payload.parameters.clone(),
                event_bus_contract: payload.event_bus_contract,
            }],
        )
        .unwrap();
        let occurrence = AudioProcessorOccurrence {
            instance_id: AudioProcessorInstanceId::new(),
            owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
            insertion: AudioProcessorInsertionPoint::PreFader,
        };
        let parameters = BTreeMap::new();
        for reference in [None, Some(note_reference())] {
            for requested in [false, true] {
                let result = resolver.resolve(
                    AudioProcessorPrepareRequest::new(
                        occurrence,
                        &definition,
                        &parameters,
                        None,
                        render(),
                    )
                    .with_musical_time_reference(reference)
                    .with_note_input_requested(requested),
                );
                if requested && reference.is_none() {
                    assert_eq!(
                        result.unwrap_err(),
                        unavailable("VST3 Note input requires a musical time reference")
                    );
                    continue;
                }
                let spec = result.unwrap();
                assert!(
                    spec.native_lane.is_some(),
                    "auxiliary-only processing retains its native lane"
                );
                assert_eq!(spec.auxiliary_inputs, auxiliary);
                let child = IsolatedAudioProcessorWorkerPrepareRequest {
                    musical_time_reference: spec.musical_time_reference,
                    payload: spec.preparation_payload.clone(),
                    plugin_execution_contract: spec.plugin_execution_contract,
                    auxiliary_inputs: spec.auxiliary_inputs.clone(),
                    auxiliary_outputs: spec.auxiliary_outputs.clone(),
                    note_input_contract: spec.note_input_contract.clone(),
                    parameter_ids: spec.parameter_ids.clone(),
                    render_contract: spec.render_contract,
                    native_lane: spec.native_lane,
                };
                let decoded = validated_preparation_payload(&child).unwrap();
                validate_note_preparation(&child, &decoded).unwrap();
                assert_eq!(!child.note_input_contract().ports.is_empty(), requested);
            }
        }
        let mut wrong = auxiliary.clone();
        wrong.buses[0].bus_key = "vst3.audio-input.1".into();
        assert!(Vst3MainAudioInput::Absent.validate_auxiliary(&wrong).is_err());
        assert!(Vst3MainAudioInput::Present.validate_auxiliary(&auxiliary).is_err());
    }

    #[test]
    fn vst3_auxiliary_only_signal_copies_native_zero_without_using_graph_main_as_input() {
        use std::cell::Cell;
        let mut payload = payload();
        payload.main_audio_input = Vst3MainAudioInput::Absent;
        payload.event_bus_contract = note_metadata(1);
        let auxiliary = AudioProcessorAuxiliaryInputContract {
            buses: vec![AudioProcessorAuxiliaryInputBusContract {
                bus_key: "vst3.audio-input.0".into(),
                channel_layout: AudioChannelLayout::Stereo,
            }],
        };
        let ids = [payload.parameters[0].parameter_id().unwrap()];
        for reference in [None, Some(note_reference())] {
            for requested in [false, true] {
                if requested && reference.is_none() {
                    continue;
                }
                let contract =
                    derived_note_input_contract(&payload.event_bus_contract, reference, requested)
                        .unwrap();
                for silence in [false, true] {
                    let mut native = Native::default();
                    let mut scratch = buffers();
                    let mut values = vec![None];
                    let mut last_end = Some(0);
                    let mut converted = Vec::with_capacity(contract.limits.input_events);
                    let count = Cell::new(0);
                    let refused = Cell::new(false);
                    for (start, frames) in [(0, 4), (4, 1), (5, 3)] {
                        let mut pcm = vec![99.0; frames * 2];
                        let input = if silence {
                            vec![0.0; frames * 2]
                        } else {
                            (0..frames * 2).map(|i| i as f32 - 3.0).collect()
                        };
                        VstSignalFields {
                            last_parameter_values: &mut values,
                            configuration_phase: &NativeConfigurationPhase::Admitted,
                            payload: &payload,
                            render: render(),
                            buffers: &mut scratch,
                            auxiliary_inputs: &auxiliary,
                            last_end: &mut last_end,
                            note_input_contract: &contract,
                            note_events: &mut converted,
                        }
                        .process(
                            &mut native,
                            IsolatedAudioProcessorWorkerBlock::new(
                                start,
                                frames,
                                render(),
                                &mut pcm,
                                &auxiliary,
                                &input,
                                &ids,
                                &[],
                            )
                            .with_musical_time_reference(reference)
                            .with_note_regions(
                                &contract,
                                &[],
                                &mut [],
                                &count,
                                &refused,
                            ),
                        )
                        .unwrap();
                        assert_eq!(
                            pcm, input,
                            "graph Main sentinel must not replace native Auxiliary zero"
                        );
                        assert_eq!(last_end, Some(start + frames as i64));
                        assert!(converted.is_empty());
                    }
                    let processing = if requested { "notes" } else { "process" };
                    assert_eq!(
                        native.calls,
                        ["timeline", processing, "timeline", processing, "timeline", processing]
                    );
                    assert!(!refused.get());
                }
            }
        }
    }

    #[test]
    fn vst3_zero_audio_input_signal_uses_output_extent_and_checked_empty_note_batches() {
        use super::super::protocol::SharedNoteEvent;
        use std::cell::Cell;
        let mut payload = payload();
        payload.main_audio_input = Vst3MainAudioInput::Absent;
        payload.event_bus_contract = note_metadata(1);
        let contract =
            derived_note_input_contract(&payload.event_bus_contract, Some(note_reference()), true)
                .unwrap();
        let ids = [payload.parameters[0].parameter_id().unwrap()];
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        for rate in [44_100, 48_000] {
            for all_empty in [false, true] {
                let mut native = Native { zero_input: true, ..Default::default() };
                let mut render = render();
                render.sample_rate = rate;
                let mut scratch = buffers();
                scratch.inputs.clear();
                scratch.sample_rate = f64::from(rate);
                let mut values = vec![None];
                let mut last_end = Some(0);
                let mut converted = Vec::with_capacity(contract.limits.input_events);
                let capacity = converted.capacity();
                let count = Cell::new(0);
                let refused = Cell::new(false);
                let mut start = 0;
                for (block_index, frames) in [4, 1, 2].into_iter().enumerate() {
                    let mut pcm = vec![19.0; frames * 2];
                    let events = [SharedNoteEvent {
                        sample_offset: 0,
                        native_id: 7,
                        port_index: 0,
                        channel: 0,
                        key: 60,
                        kind: if block_index == 0 { 1 } else { 0 },
                        velocity: 0.5,
                    }];
                    let notes = if all_empty || block_index == 1 {
                        &[][..]
                    } else {
                        &events[..]
                    };
                    VstSignalFields {
                        last_parameter_values: &mut values,
                        configuration_phase: &NativeConfigurationPhase::Admitted,
                        payload: &payload,
                        render,
                        buffers: &mut scratch,
                        auxiliary_inputs: &auxiliary,
                        last_end: &mut last_end,
                        note_input_contract: &contract,
                        note_events: &mut converted,
                    }
                    .process(
                        &mut native,
                        IsolatedAudioProcessorWorkerBlock::new(
                            start,
                            frames,
                            render,
                            &mut pcm,
                            &auxiliary,
                            &[],
                            &ids,
                            &[],
                        )
                        .with_musical_time_reference(Some(note_reference()))
                        .with_note_regions(
                            &contract,
                            notes,
                            &mut [],
                            &count,
                            &refused,
                        ),
                    )
                    .unwrap();
                    assert!(scratch.inputs.is_empty());
                    assert_eq!(scratch.block_size, frames);
                    for frame in pcm.chunks_exact(2) {
                        assert_eq!(frame[0].to_bits(), 0.375_f32.to_bits());
                        assert_eq!(frame[1].to_bits(), (-0.125_f32).to_bits());
                    }
                    start += frames as i64;
                    assert_eq!(last_end, Some(start));
                    assert!(converted.is_empty());
                    assert_eq!(converted.capacity(), capacity);
                }
                assert_eq!(
                    native.calls,
                    ["timeline", "notes", "timeline", "notes", "timeline", "notes"]
                );
                assert_eq!(native.notes.len(), if all_empty { 0 } else { 2 });
                assert_eq!(count.get(), 0);
                assert!(!refused.get());
            }
        }
    }

    #[test]
    fn vst3_note_contract_requires_some_active_main_zero_and_preserves_raw_metadata() {
        let raw = note_metadata(32);
        let snapshot = raw;
        let contract = derived_note_input_contract(&raw, Some(note_reference()), true).unwrap();
        contract.validate().unwrap();
        assert_eq!(raw, snapshot);
        assert_eq!(
            contract.completion,
            crate::AudioNoteCompletionPolicy::SuccessfulOffBlock
        );
        assert_eq!(
            contract.ports,
            [crate::AudioProcessorNoteInputPort {
                port_key: "vst3.note-input.0".to_owned(),
                native_id: 0,
                native_index: 0,
                channel_count: 16,
            }]
        );
        assert_eq!(
            contract.limits,
            crate::AudioNoteExecutionLimits {
                input_events: 4096,
                output_ends: 0,
                retained_records: 1024,
                live_voices: 0,
            }
        );
        assert_eq!(
            derived_note_input_contract(&note_metadata(2), Some(note_reference()), true)
                .unwrap()
                .ports[0]
                .channel_count,
            2
        );
        assert_eq!(
            derived_note_input_contract(&raw, None, true).unwrap_err(),
            unavailable("VST3 Note input requires a musical time reference")
        );
        assert_eq!(
            derived_note_input_contract(&raw, None, false).unwrap(),
            crate::AudioProcessorNoteInputContract::default()
        );
        for variant in 0..4 {
            let mut bad = note_metadata(16);
            match variant {
                0 => bad.inputs.as_mut_slice()[0].host_active = false,
                1 => bad.inputs.as_mut_slice()[0].channel_count = 0,
                2 => {
                    bad.inputs.as_mut_slice()[0].role =
                        super::super::vst3_processing_metadata::Vst3EventBusRole::Auxiliary
                }
                _ => bad.outputs = bad.inputs,
            }
            assert!(derived_note_input_contract(&bad, Some(note_reference()), true).is_err());
        }
    }

    #[test]
    fn vst3_note_alignment_mismatch_refuses_before_allocation_or_native_binary_access() {
        let mut payload = payload();
        payload.event_bus_contract = note_metadata(16);
        let note_input_contract =
            derived_note_input_contract(&payload.event_bus_contract, Some(note_reference()), true)
                .unwrap();
        let expected = native_worker_lane_profile_for_preparation(
            &payload.parameters,
            &note_input_contract,
            render(),
            &AudioProcessorAuxiliaryInputContract::default(),
        )
        .unwrap()
        .unwrap();
        let storage = expected.adapter_note_input_storage();
        let wrong = expected
            .with_adapter_note_input_storage(crate::NativeHostBufferLayout {
                alignment: storage.alignment * 2,
                ..storage
            })
            .unwrap();
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            musical_time_reference: Some(note_reference()),
            native_lane: Some(wrong),
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
            auxiliary_outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
            note_input_contract,
            parameter_ids: vec![payload.parameters[0].parameter_id().unwrap()],
            render_contract: render(),
        };
        assert_eq!(
            Vst3AudioProcessorWorkerFactory
                .native_worker_lane_resources_for_preparation(&request)
                .unwrap(),
            Some(expected)
        );
        // The payload points to an unopened, unavailable binary. A later
        // fingerprint/load/init error cannot satisfy this exact metadata guard.
        let error = match Vst3AudioProcessorWorkerFactory.prepare_native(request) {
            Ok(_) => panic!("a different prepared backing alignment was accepted"),
            Err(error) => error,
        };
        assert_eq!(
            error,
            invalid("VST3 native storage request differs from prepared metadata")
        );
    }

    #[test]
    fn vst3_note_signal_uses_checked_empty_or_scalar_batch_and_never_replays_failure() {
        use super::super::protocol::SharedNoteEvent;
        use std::cell::Cell;
        let payload = payload();
        let ids = [payload.parameters[0].parameter_id().unwrap()];
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let contract =
            derived_note_input_contract(&note_metadata(2), Some(note_reference()), true).unwrap();
        let notes = [
            SharedNoteEvent {
                sample_offset: 0,
                native_id: 73,
                port_index: 0,
                channel: 1,
                key: 61,
                kind: 1,
                velocity: 0.75,
            },
            SharedNoteEvent {
                sample_offset: 3,
                native_id: 73,
                port_index: 0,
                channel: 1,
                key: 61,
                kind: 0,
                velocity: 0.25,
            },
        ];
        let parameters = [SharedParameterEvent { lane: 0, sample_offset: 0, value: 0.25 }];
        for empty in [false, true] {
            for (reject, retained) in [
                (false, None),
                (false, Some(0.25)),
                (true, None),
                (true, Some(0.25)),
            ] {
                let mut native = Native { reject_notes: reject, ..Default::default() };
                let mut pcm = [11.0; 8];
                let mut scratch = buffers();
                let mut values = vec![retained];
                let mut last_end = Some(-3);
                let mut converted = Vec::with_capacity(contract.limits.input_events);
                let capacity = converted.capacity();
                let count = Cell::new(0);
                let refused = Cell::new(false);
                let result = VstSignalFields {
                    last_parameter_values: &mut values,
                    configuration_phase: &NativeConfigurationPhase::Admitted,
                    payload: &payload,
                    render: render(),
                    buffers: &mut scratch,
                    auxiliary_inputs: &auxiliary,
                    last_end: &mut last_end,
                    note_input_contract: &contract,
                    note_events: &mut converted,
                }
                .process(
                    &mut native,
                    IsolatedAudioProcessorWorkerBlock::new(
                        -3,
                        4,
                        render(),
                        &mut pcm,
                        &auxiliary,
                        &[],
                        &ids,
                        &parameters,
                    )
                    .with_musical_time_reference(Some(note_reference()))
                    .with_note_regions(
                        &contract,
                        if empty { &[] } else { &notes },
                        &mut [],
                        &count,
                        &refused,
                    ),
                );
                assert_eq!(
                    native.calls,
                    if retained.is_none() {
                        vec!["timeline", "parameter", "notes"]
                    } else {
                        vec!["timeline", "notes"]
                    }
                );
                assert_eq!(converted.capacity(), capacity);
                assert!(
                    converted.is_empty(),
                    "no successful or failed batch retained for replay"
                );
                assert_eq!(native.notes.len(), if empty { 0 } else { 2 });
                assert_eq!(
                    count.get(),
                    0,
                    "SuccessfulOffBlock emits no fabricated native Ends"
                );
                if !empty {
                    assert_eq!(native.notes[0].bus_index, 0);
                    assert_eq!(native.notes[0].sample_offset, 0);
                    assert_eq!(native.notes[0].flags, 0);
                    assert_eq!(
                        native.notes[0].data,
                        PluginEventData::NoteOn {
                            channel: 1,
                            pitch: 61,
                            tuning: 0.0,
                            velocity: 0.75,
                            length: 0,
                            note_id: 73,
                        }
                    );
                    assert_eq!(native.notes[1].sample_offset, 3);
                    assert_eq!(
                        native.notes[1].data,
                        PluginEventData::NoteOff {
                            channel: 1,
                            pitch: 61,
                            velocity: 0.25,
                            note_id: 73,
                            tuning: 0.0,
                        }
                    );
                }
                if reject {
                    assert!(matches!(result, Err(AudioProcessorHostError::Process(_))));
                    assert_eq!(values, [retained]);
                    assert_eq!(last_end, Some(-3));
                } else {
                    result.unwrap();
                    assert_eq!(values, [Some(0.25)]);
                    assert_eq!(last_end, Some(1));
                }
                assert_eq!(pcm, [11.0; 8]);
            }
        }
    }

    #[test]
    fn vst3_note_late_invalid_channel_refuses_before_timeline_parameters_or_pcm() {
        use super::super::protocol::SharedNoteEvent;
        use std::cell::Cell;
        let payload = payload();
        let ids = [payload.parameters[0].parameter_id().unwrap()];
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let contract =
            derived_note_input_contract(&note_metadata(2), Some(note_reference()), true).unwrap();
        let notes = [
            SharedNoteEvent {
                sample_offset: 0,
                native_id: 73,
                port_index: 0,
                channel: 1,
                key: 61,
                kind: 1,
                velocity: 0.75,
            },
            SharedNoteEvent {
                sample_offset: 3,
                native_id: 73,
                port_index: 0,
                channel: 2,
                key: 61,
                kind: 0,
                velocity: 0.25,
            },
        ];
        let mut native = Native::default();
        let mut pcm = [11.0; 8];
        let mut scratch = buffers();
        let mut values = vec![None];
        let mut last_end = Some(-3);
        let mut converted = Vec::with_capacity(contract.limits.input_events);
        let result = VstSignalFields {
            last_parameter_values: &mut values,
            configuration_phase: &NativeConfigurationPhase::Admitted,
            payload: &payload,
            render: render(),
            buffers: &mut scratch,
            auxiliary_inputs: &auxiliary,
            last_end: &mut last_end,
            note_input_contract: &contract,
            note_events: &mut converted,
        }
        .process(
            &mut native,
            IsolatedAudioProcessorWorkerBlock::new(
                -3,
                4,
                render(),
                &mut pcm,
                &auxiliary,
                &[],
                &ids,
                &[],
            )
            .with_musical_time_reference(Some(note_reference()))
            .with_note_regions(
                &contract,
                &notes,
                &mut [],
                &Cell::new(0),
                &Cell::new(false),
            ),
        );
        assert_eq!(
            result.unwrap_err(),
            invalid("VST3 Note batch exceeds the admitted scalar contract")
        );
        assert!(native.calls.is_empty());
        assert!(converted.is_empty());
        assert_eq!(pcm, [11.0; 8]);
        assert_eq!(scratch.inputs[0].channels[0], [7.0; 4]);
        assert_eq!(values, [None]);
        assert_eq!(last_end, Some(-3));
    }

    #[test]
    fn vst3_musical_effect_spec_keeps_exact_reference_and_refuses_explicit_note_prepare() {
        use crate::{
            AudioProcessorInsertionPoint, AudioProcessorOccurrence, AudioProcessorOccurrenceOwner,
        };
        use mondrian_core::{AudioProcessorInstanceId, ProgramOutputId};
        let payload = payload();
        let definition = AudioProcessorDefinitionRef::Vst3 {
            class_id: payload.class_id.clone(),
            vendor: None,
            schema_version: 1,
            binary_sha256: Some(payload.binary_sha256),
        };
        let resolver = Vst3AudioProcessorSpecResolver::new(
            std::env::temp_dir().join("unlaunched-musical-helper.exe"),
            [Vst3PluginRegistration {
                class_id: payload.class_id.clone(),
                vendor: None,
                binary_path: payload.binary_path,
                binary_sha256: payload.binary_sha256,
                execution_contract: AudioProcessorExecutionContract::new(
                    0,
                    AudioProcessorTail::None,
                    true,
                    true,
                    true,
                    0,
                )
                .unwrap(),
                auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
                parameters: payload.parameters,
                main_audio_input: payload.main_audio_input,
                audio_output_count: payload.audio_output_count,
                main_audio_output_count: payload.main_audio_output_count,
                event_bus_contract: payload.event_bus_contract,
            }],
        )
        .unwrap();
        let occurrence = AudioProcessorOccurrence {
            instance_id: AudioProcessorInstanceId::new(),
            owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
            insertion: AudioProcessorInsertionPoint::PreFader,
        };
        let parameters = BTreeMap::new();
        for reference in [
            None,
            Some(
                MusicalTimeReference::new(
                    TimelineTime::new(-19, 7).unwrap(),
                    TimelineTime::new(3, 4).unwrap(),
                )
                .unwrap(),
            ),
        ] {
            let request = || {
                AudioProcessorPrepareRequest::new(
                    occurrence,
                    &definition,
                    &parameters,
                    Some(&[1, 2, 3]),
                    render(),
                )
                .with_musical_time_reference(reference)
            };
            let spec = resolver.resolve(request()).unwrap();
            assert_eq!(spec.musical_time_reference, reference);
            assert!(spec.note_input_contract.ports.is_empty());
            assert_eq!(
                decode_payload(&spec.preparation_payload).unwrap().state,
                Some(vec![1, 2, 3])
            );
            assert_eq!(
                resolver.resolve(request().with_note_input_requested(true)).unwrap_err(),
                unavailable(if reference.is_none() {
                    "VST3 Note input requires a musical time reference"
                } else {
                    "VST3 Note input requires active Main event bus zero and no event outputs"
                })
            );
        }
    }

    #[test]
    fn vst3_musical_effect_signal_installs_exact_context_before_parameters_and_pcm() {
        let payload = payload();
        let ids = [payload.parameters[0].parameter_id().unwrap()];
        let events = [SharedParameterEvent { lane: 0, sample_offset: 1, value: 0.25 }];
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        for rate in [44_100, 48_000] {
            for start in [-17, i64::MAX - 32] {
                for (with_music, activity) in [true, false].into_iter().flat_map(|music| {
                    [
                        Some(mondrian_core::TimelineTransportActivity::Playing),
                        Some(mondrian_core::TimelineTransportActivity::Stopped),
                        None,
                    ]
                    .into_iter()
                    .map(move |activity| (music, activity))
                }) {
                    let reference = with_music.then(|| {
                        MusicalTimeReference::new(
                            TimelineTime::new(start - 1, i64::from(rate)).unwrap(),
                            TimelineTime::new(3, 4).unwrap(),
                        )
                        .unwrap()
                    });
                    let mut render = render();
                    render.sample_rate = rate;
                    let mut native = Native::default();
                    let mut pcm = [11.0; 8];
                    let mut scratch = buffers();
                    let mut values = vec![None];
                    let mut last_end = Some(start);
                    VstSignalFields {
                        last_parameter_values: &mut values,
                        configuration_phase: &NativeConfigurationPhase::Admitted,
                        payload: &payload,
                        render,
                        buffers: &mut scratch,
                        auxiliary_inputs: &auxiliary,
                        last_end: &mut last_end,
                        note_input_contract: &crate::AudioProcessorNoteInputContract::default(),
                        note_events: &mut Vec::new(),
                    }
                    .process(
                        &mut native,
                        IsolatedAudioProcessorWorkerBlock::new(
                            start,
                            4,
                            render,
                            &mut pcm,
                            &auxiliary,
                            &[],
                            &ids,
                            &events,
                        )
                        .with_musical_time_reference(reference)
                        .with_transport_activity(activity),
                    )
                    .unwrap();
                    assert_eq!(native.calls, ["timeline", "parameter", "process"]);
                    assert_eq!(native.transport_activity, activity);
                    let (timeline, musical) = native.timeline.unwrap();
                    assert_eq!(
                        timeline,
                        ProcessTimelineContext { sample_position: start, sample_rate: rate }
                    );
                    assert_eq!(
                        musical,
                        reference.map(|reference| {
                            let zero = reference.quarter_note_zero();
                            ProcessMusicalReference::new(zero.numerator(), zero.denominator(), 3, 4)
                                .unwrap()
                        })
                    );
                    if let Some(reference) = musical {
                        let expected = if rate == 44_100 {
                            0x3eff_b3f6_5f17_ec9e
                        } else {
                            0x3efd_208a_5a91_2e32
                        };
                        assert_eq!(
                            reference.quarter_notes_at(start, rate).unwrap().to_bits(),
                            expected
                        );
                    }
                    assert_eq!(pcm, [11.0; 8]);
                    assert_eq!(values, [Some(0.25)]);
                    assert_eq!(last_end, Some(start + 4));
                }
            }
        }
    }

    #[test]
    fn vst3_musical_effect_window_and_setter_refusals_leave_parameters_pcm_and_cursor_untouched() {
        let payload = payload();
        let ids = [payload.parameters[0].parameter_id().unwrap()];
        let events = [SharedParameterEvent { lane: 0, sample_offset: 1, value: 0.25 }];
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let reference = MusicalTimeReference::new(
            TimelineTime::new(-19, 7).unwrap(),
            TimelineTime::new(3, 4).unwrap(),
        )
        .unwrap();
        let mut cases = vec![
            (-17, 4, true, 1, "VST3 timeline delivery failed:"),
            (i64::MAX - 1, 4, false, 0, "VST3 sample position overflowed"),
        ];
        if let Ok(frames) = usize::try_from(i64::MAX as u64 + 1) {
            cases.push((
                i64::MIN,
                frames,
                false,
                0,
                "VST3 sample extent is not representable",
            ));
        }
        for (start, frames, reject_setter, expected_calls, detail) in cases {
            let mut native = Native {
                reject_timeline: reject_setter,
                ..Native::default()
            };
            let mut pcm = [11.0; 8];
            let mut scratch = buffers();
            let before = scratch.clone();
            let mut values = vec![Some(0.75)];
            let mut last_end = Some(start);
            let error = VstSignalFields {
                last_parameter_values: &mut values,
                configuration_phase: &NativeConfigurationPhase::Admitted,
                payload: &payload,
                render: render(),
                buffers: &mut scratch,
                auxiliary_inputs: &auxiliary,
                last_end: &mut last_end,
                note_input_contract: &crate::AudioProcessorNoteInputContract::default(),
                note_events: &mut Vec::new(),
            }
            .process(
                &mut native,
                IsolatedAudioProcessorWorkerBlock::new(
                    start,
                    frames,
                    render(),
                    &mut pcm,
                    &auxiliary,
                    &[],
                    &ids,
                    &events,
                )
                .with_musical_time_reference(Some(reference)),
            )
            .unwrap_err();
            assert!(
                matches!(error, AudioProcessorHostError::Process(message) if message.starts_with(detail))
            );
            assert_eq!(native.calls.len(), expected_calls);
            assert_eq!(
                native.calls,
                if expected_calls == 0 {
                    vec![]
                } else {
                    vec!["timeline"]
                }
            );
            assert_eq!(native.timeline, None);
            assert_eq!(scratch, before);
            assert_eq!(pcm, [11.0; 8]);
            assert_eq!(values, [Some(0.75)]);
            assert_eq!(last_end, Some(start));
        }
    }
}
