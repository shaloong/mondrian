//! VST3 audio effects run only inside the supervised native worker.

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

    /// Host authoring schema. Stepped VST3 values are stored as integer indices.
    pub fn authoring_schema(&self) -> Result<ParameterSchema, AudioProcessorHostError> {
        validate_parameter(self)?;
        let stepped = self.step_count > 0;
        let default = if stepped {
            PropertyValue::Int(
                (self.default_normalized * f64::from(self.step_count)).round() as i64,
            )
        } else {
            PropertyValue::Double(self.default_normalized)
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
        .with_dispatch_argument(Some(OsString::from(VST3_AUDIO_WORKER_ARGUMENT)))
        .with_deadlines(
            std::time::Duration::from_secs(15),
            std::time::Duration::from_millis(100),
        )?
        .with_state_entry_timeout(std::time::Duration::from_secs(10))
    }
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

/// Child-only VST3 ABI factory. The editor never loads user plugin code.
pub struct Vst3AudioProcessorWorkerFactory;

impl IsolatedAudioProcessorWorkerFactory for Vst3AudioProcessorWorkerFactory {
    fn prepare(
        &self,
        request: IsolatedAudioProcessorWorkerPrepareRequest,
    ) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError> {
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
        let mut host = Vst3Host::builder()
            .sample_rate(f64::from(render.sample_rate))
            .block_size(render.max_block_frames)
            .input_channels(channels)
            .output_channels(channels)
            .build()
            .map_err(|error| unavailable(format!("VST3 host creation failed: {error}")))?;
        let mut plugin = host
            .load_plugin_class(&payload.binary_path, &payload.class_id)
            .map_err(|error| unavailable(format!("VST3 class load failed: {error}")))?;
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
        plugin
            .set_process_mode(mode)
            .map_err(|error| invalid(format!("VST3 processing mode is unsupported: {error}")))?;
        let input_names = validate_loaded(&mut plugin, &registration, render.channel_layout)?;
        let buffers = plugin
            .create_bus_audio_buffers(render.max_block_frames)
            .map_err(|error| invalid(format!("VST3 bus buffer preparation failed: {error}")))?;
        plugin
            .start_processing()
            .map_err(|error| unavailable(format!("VST3 processing start failed: {error}")))?;
        Ok(Box::new(Vst3Worker {
            plugin: Some(plugin),
            host,
            payload,
            contract: request.plugin_execution_contract(),
            render,
            buffers,
            auxiliary_inputs: registration.auxiliary_inputs,
            input_names,
            last_end: None,
        }))
    }
}

struct Vst3Worker {
    plugin: Option<Plugin>,
    host: Vst3Host,
    payload: Vst3Payload,
    contract: AudioProcessorExecutionContract,
    render: crate::AudioRenderContract,
    buffers: BusAudioBuffers,
    auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    input_names: Vec<crate::AudioProcessorInputName>,
    last_end: Option<i64>,
}

impl IsolatedAudioProcessorWorker for Vst3Worker {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.contract
    }

    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        self.auxiliary_inputs.clone()
    }

    fn auxiliary_input_names(&self) -> Vec<crate::AudioProcessorInputName> {
        self.input_names.clone()
    }

    fn enter_state(&mut self, start_sample: i64) -> Result<(), AudioProcessorHostError> {
        let mut previous = self.plugin.take().ok_or_else(|| {
            AudioProcessorHostError::StateEntry("VST3 processor is closed".to_owned())
        })?;
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
        self.plugin = Some(fresh);
        self.last_end = Some(start_sample);
        Ok(())
    }

    fn process(
        &mut self,
        mut block: IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        let frames = block.frames();
        let end_sample = block.start_sample().checked_add(frames as i64).ok_or_else(|| {
            AudioProcessorHostError::Process("VST3 sample position overflowed".to_owned())
        })?;
        let channels = self.render.channel_count();
        if self.last_end != Some(block.start_sample())
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
                let value = if descriptor.step_count > 0 {
                    if !event.value.is_finite()
                        || event.value.fract() != 0.0
                        || event.value < 0.0
                        || event.value > f64::from(descriptor.step_count)
                    {
                        return Err(AudioProcessorHostError::Process(
                            "VST3 discrete parameter value is invalid".to_owned(),
                        ));
                    }
                    event.value / f64::from(descriptor.step_count)
                } else {
                    event.value
                };
                if event.sample_offset as usize >= frames
                    || !value.is_finite()
                    || !(0.0..=1.0).contains(&value)
                {
                    return Err(AudioProcessorHostError::Process(
                        "VST3 parameter event is out of bounds".to_owned(),
                    ));
                }
                self.plugin
                    .as_mut()
                    .ok_or_else(|| {
                        AudioProcessorHostError::Process("VST3 processor is closed".to_owned())
                    })?
                    .set_parameter_at(native_id, value, event.sample_offset as i32)
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
        self.plugin
            .as_mut()
            .ok_or_else(|| AudioProcessorHostError::Process("VST3 processor is closed".to_owned()))?
            .set_process_timeline_context(vst3_host::audio::ProcessTimelineContext {
                sample_position: block.start_sample(),
                sample_rate: block.sample_rate(),
            })
            .map_err(|error| {
                AudioProcessorHostError::Process(format!("VST3 timeline delivery failed: {error}"))
            })?;
        self.plugin
            .as_mut()
            .ok_or_else(|| AudioProcessorHostError::Process("VST3 processor is closed".to_owned()))?
            .process_bus_audio(&mut self.buffers)
            .map_err(|error| {
                AudioProcessorHostError::Process(format!("VST3 processing failed: {error}"))
            })?;
        for (frame, samples) in block.main_interleaved().chunks_exact_mut(channels).enumerate() {
            for (channel, sample) in samples.iter_mut().enumerate() {
                *sample = self.buffers.outputs[0].channels[channel][frame];
            }
        }
        self.last_end = Some(end_sample);
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

#[cfg(test)]
mod bus_contract_tests {
    use super::*;
    use vst3_host::audio::{AudioBusConfig, NativeAudioBusInfo};

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
