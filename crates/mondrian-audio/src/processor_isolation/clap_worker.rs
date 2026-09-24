//! CLAP ABI execution inside the supervised audio worker process.

use super::clap_discovery::{fingerprint_clap_binary, list_parameters, validate_parameters};
use super::{
    probe_clap_plugin_registration, scan_clap_library_descriptors, ClapPluginDescriptor,
    IsolatedAudioProcessorSpecResolver, IsolatedAudioProcessorWorker,
    IsolatedAudioProcessorWorkerBlock, IsolatedAudioProcessorWorkerFactory,
    IsolatedAudioProcessorWorkerPrepareRequest, IsolatedAudioProcessorWorkerSpec,
};
use crate::{
    AudioProcessorAuxiliaryInputContract, AudioProcessorExecutionContract, AudioProcessorHostError,
    AudioProcessorPrepareRequest,
};
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfoBuffer, AudioPortType, PluginAudioPorts,
};
use clack_extensions::latency::PluginLatency;
use clack_extensions::state::PluginState;
use clack_extensions::tail::{PluginTail, TailLength};
use clack_host::events::event_types::ParamValueEvent;
use clack_host::prelude::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, ClapId, EventBuffer, HostHandlers, HostInfo,
    InputChannel, OutputEvents, Pckn, PluginAudioConfiguration, PluginAudioProcessor, PluginEntry,
    PluginInstance, SharedHandler,
};
use mondrian_core::{
    AudioChannelLayout, AudioProcessorInstanceId, AuthoringMap, ExactAutomationCurve,
};
use mondrian_timeline::audio::{
    AudioProcessorDefinitionRef, AudioProcessorInstance, AudioProcessorParameter,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub(super) struct ClapHost;

#[derive(Clone)]
pub(super) struct ClapHostShared {
    restart_requested: Arc<AtomicBool>,
    callback_requested: Arc<AtomicBool>,
}

impl ClapHostShared {
    pub(super) fn new() -> Self {
        Self {
            restart_requested: Arc::new(AtomicBool::new(false)),
            callback_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn service_callbacks(
        &self,
        instance: &mut PluginInstance<ClapHost>,
    ) -> Result<(), AudioProcessorHostError> {
        for _ in 0..16 {
            if !self.callback_requested.swap(false, Ordering::AcqRel) {
                return if self.restart_requested.load(Ordering::Acquire) {
                    Err(invalid("CLAP plugin requested a host restart"))
                } else {
                    Ok(())
                };
            }
            instance.call_on_main_thread_callback();
        }
        Err(invalid(
            "CLAP plugin requested unbounded main-thread callbacks",
        ))
    }

    pub(super) fn restart_requested(&self) -> bool {
        self.restart_requested.load(Ordering::Acquire)
    }
}

impl SharedHandler<'_> for ClapHostShared {
    fn request_restart(&self) {
        self.restart_requested.store(true, Ordering::Release);
    }

    fn request_process(&self) {}

    fn request_callback(&self) {
        self.callback_requested.store(true, Ordering::Release);
    }
}

impl HostHandlers for ClapHost {
    type Shared<'a> = ClapHostShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
}

/// Child-only CLAP factory. Plugin binaries are never loaded by the editor process.
pub struct ClapAudioProcessorWorkerFactory;

/// Installed CLAP definition admitted by the application after discovery.
#[derive(Debug, Clone)]
pub struct ClapPluginRegistration {
    /// CLAP descriptor ID, independent of the installed binary path.
    pub plugin_id: String,
    /// Absolute path to the selected installed library.
    pub library_path: PathBuf,
    /// SHA-256 of the exact installed binary admitted by discovery and probe.
    pub binary_sha256: [u8; 32],
    /// Exact scheduling contract supplied by the caller and rechecked in the child.
    pub execution_contract: AudioProcessorExecutionContract,
    /// Definition-local parameter IDs and plain-value contracts from the probe.
    pub parameters: Vec<super::ClapParameterDescriptor>,
}

/// Parent-side registry that packages an installed CLAP reference for the child.
///
/// Registering does not load native code. The child revalidates the descriptor,
/// ports, latency, and tail against this registry before it executes any block.
pub struct ClapAudioProcessorSpecResolver {
    helper_executable: PathBuf,
    plugins: BTreeMap<String, ClapPluginRegistration>,
}

/// CLAP resolver that discovers selected installed libraries and probes each
/// occurrence's exact render contract before worker admission.
pub struct DiscoveredClapAudioProcessorSpecResolver {
    helper_executable: PathBuf,
    plugins: BTreeMap<String, (ClapPluginDescriptor, PathBuf, [u8; 32])>,
}

impl DiscoveredClapAudioProcessorSpecResolver {
    /// Discover explicitly selected installed libraries in isolated children.
    /// Duplicate plugin IDs require the caller to choose one installed binary.
    pub fn discover(
        helper_executable: PathBuf,
        libraries: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, AudioProcessorHostError> {
        if !helper_executable.is_absolute() {
            return Err(invalid("CLAP helper executable must be absolute"));
        }
        let mut plugins = BTreeMap::new();
        for library in libraries {
            let path = std::fs::canonicalize(&library)
                .map_err(|error| unavailable(format!("CLAP library is unavailable: {error}")))?;
            let fingerprint = fingerprint_clap_binary(&path)?;
            let descriptors = scan_clap_library_descriptors(&helper_executable, &path)?;
            if fingerprint_clap_binary(&path)? != fingerprint {
                return Err(unavailable(
                    "CLAP binary changed during descriptor discovery",
                ));
            }
            for descriptor in descriptors {
                if plugins
                    .insert(
                        descriptor.plugin_id.clone(),
                        (descriptor, path.clone(), fingerprint),
                    )
                    .is_some()
                {
                    return Err(invalid(
                        "duplicate installed CLAP plugin ID requires selection",
                    ));
                }
            }
        }
        Ok(Self { helper_executable, plugins })
    }

    /// Stable plugin descriptors available for an insertion picker.
    pub fn descriptors(&self) -> Vec<&ClapPluginDescriptor> {
        self.plugins.values().map(|(descriptor, _, _)| descriptor).collect()
    }

    /// Capture one selected plugin's current plain values into a portable author instance.
    ///
    /// The selected binary is probed again for this render contract and state;
    /// a later Preview or Export preparation repeats the same probe.
    pub fn create_instance(
        &self,
        plugin_id: &str,
        render_contract: crate::AudioRenderContract,
        state: Option<Vec<u8>>,
    ) -> Result<AudioProcessorInstance, AudioProcessorHostError> {
        let (_, library_path, fingerprint) = self
            .plugins
            .get(plugin_id)
            .ok_or_else(|| unavailable(format!("CLAP plugin {plugin_id} is not installed")))?;
        let registration = probe_clap_plugin_registration(
            &self.helper_executable,
            library_path,
            plugin_id,
            render_contract,
            state.as_deref(),
        )?;
        if registration.binary_sha256 != *fingerprint {
            return Err(unavailable(
                "CLAP binary changed after descriptor discovery",
            ));
        }
        let mut parameters = AuthoringMap::new();
        for descriptor in registration.parameters.iter().filter(|parameter| !parameter.read_only) {
            let schema = descriptor.authoring_schema()?;
            let parameter_id = schema.parameter_id.clone();
            let mut parameter = AudioProcessorParameter::from_schema(schema)
                .map_err(|error| invalid(format!("CLAP parameter schema is invalid: {error}")))?;
            parameter
                .set_automation(
                    ExactAutomationCurve::new(parameter_id.clone(), descriptor.current_value)
                        .map_err(|error| {
                            invalid(format!("CLAP current parameter value is invalid: {error}"))
                        })?,
                )
                .map_err(|error| invalid(format!("CLAP parameter value is invalid: {error}")))?;
            parameters.insert(parameter_id, parameter);
        }
        Ok(AudioProcessorInstance {
            id: AudioProcessorInstanceId::new(),
            definition: AudioProcessorDefinitionRef::Clap {
                plugin_id: plugin_id.to_owned(),
                schema_version: 1,
            },
            bypassed: false,
            parameters,
            opaque_state: state.map(|bytes| bytes.into_iter().collect()),
        })
    }
}

impl IsolatedAudioProcessorSpecResolver for DiscoveredClapAudioProcessorSpecResolver {
    fn resolve(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<IsolatedAudioProcessorWorkerSpec, AudioProcessorHostError> {
        let AudioProcessorDefinitionRef::Clap { plugin_id, .. } = request.definition() else {
            return Err(unavailable(
                "only CLAP definitions are handled by this resolver",
            ));
        };
        let (_, library_path, fingerprint) = self
            .plugins
            .get(plugin_id)
            .ok_or_else(|| unavailable(format!("CLAP plugin {plugin_id} is not installed")))?;
        let registration = probe_clap_plugin_registration(
            &self.helper_executable,
            library_path,
            plugin_id,
            request.render_contract(),
            request.opaque_state(),
        )?;
        if registration.binary_sha256 != *fingerprint {
            return Err(unavailable(
                "CLAP binary changed after descriptor discovery",
            ));
        }
        ClapAudioProcessorSpecResolver::new(self.helper_executable.clone(), [registration])?
            .resolve(request)
    }
}

impl ClapAudioProcessorSpecResolver {
    /// Build an exact registry for one application executable and installed set.
    pub fn new(
        helper_executable: PathBuf,
        plugins: impl IntoIterator<Item = ClapPluginRegistration>,
    ) -> Result<Self, AudioProcessorHostError> {
        if !helper_executable.is_absolute() {
            return Err(invalid("CLAP helper executable must be absolute"));
        }
        let mut by_id = BTreeMap::new();
        for plugin in plugins {
            if plugin.plugin_id.is_empty()
                || plugin.plugin_id.contains('\0')
                || !plugin.library_path.is_absolute()
                || plugin.binary_sha256 == [0; 32]
                || !plugin.execution_contract.requires_state_entry()
                || validate_parameters(&plugin.parameters).is_err()
                || by_id.insert(plugin.plugin_id.clone(), plugin).is_some()
            {
                return Err(invalid("CLAP registrations need unique IDs, absolute libraries, and explicit state entry"));
            }
        }
        Ok(Self { helper_executable, plugins: by_id })
    }
}

impl IsolatedAudioProcessorSpecResolver for ClapAudioProcessorSpecResolver {
    fn resolve(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<IsolatedAudioProcessorWorkerSpec, AudioProcessorHostError> {
        let AudioProcessorDefinitionRef::Clap { plugin_id, schema_version } = request.definition()
        else {
            return Err(unavailable(
                "only CLAP definitions are handled by this registry",
            ));
        };
        if *schema_version != 1 {
            return Err(unavailable("CLAP definition schema version is unsupported"));
        }
        let plugin = self
            .plugins
            .get(plugin_id)
            .ok_or_else(|| unavailable(format!("CLAP plugin {plugin_id} is not registered")))?;
        let mut parameter_ids = Vec::with_capacity(request.parameters().len());
        let mut clap_parameter_ids = Vec::with_capacity(request.parameters().len());
        for (parameter_id, parameter) in request.parameters() {
            let descriptor = plugin
                .parameters
                .iter()
                .find(|descriptor| descriptor.parameter_id().as_ref().ok() == Some(parameter_id))
                .ok_or_else(|| invalid("authored CLAP parameter is absent from the plugin"))?;
            if descriptor.read_only || parameter.schema != descriptor.authoring_schema()? {
                return Err(invalid(
                    "authored CLAP parameter contract differs from the plugin",
                ));
            }
            parameter_ids.push(parameter_id.clone());
            clap_parameter_ids.push(descriptor.id);
        }
        let payload = serde_json::to_vec(&ClapPayload {
            library_path: plugin.library_path.clone(),
            plugin_id: plugin.plugin_id.clone(),
            binary_sha256: plugin.binary_sha256,
            state: request.opaque_state().map(ToOwned::to_owned),
            parameters: plugin.parameters.clone(),
            parameter_ids: clap_parameter_ids,
        })
        .map_err(|error| invalid(format!("CLAP worker payload encoding failed: {error}")))?;
        IsolatedAudioProcessorWorkerSpec::new(
            self.helper_executable.clone(),
            payload,
            plugin.execution_contract,
            AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids,
            request.render_contract(),
        )
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ClapPayload {
    library_path: PathBuf,
    plugin_id: String,
    binary_sha256: [u8; 32],
    #[serde(default)]
    state: Option<Vec<u8>>,
    parameters: Vec<super::ClapParameterDescriptor>,
    parameter_ids: Vec<u32>,
}

impl IsolatedAudioProcessorWorkerFactory for ClapAudioProcessorWorkerFactory {
    fn prepare(
        &self,
        request: IsolatedAudioProcessorWorkerPrepareRequest,
    ) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError> {
        if !request.auxiliary_inputs().buses.is_empty() {
            return Err(invalid("CLAP auxiliary buses are not yet supported"));
        }
        let payload: ClapPayload = serde_json::from_slice(request.payload())
            .map_err(|error| invalid(format!("invalid CLAP worker payload: {error}")))?;
        if !payload.library_path.is_absolute() || payload.plugin_id.is_empty() {
            return Err(invalid(
                "CLAP library path must be absolute and plugin ID nonempty",
            ));
        }
        validate_parameters(&payload.parameters)?;
        if payload.parameter_ids.len() != request.parameter_ids().len() {
            return Err(invalid(
                "CLAP parameter lane count differs from the worker request",
            ));
        }
        for (parameter_id, clap_id) in request.parameter_ids().iter().zip(&payload.parameter_ids) {
            let descriptor = payload
                .parameters
                .iter()
                .find(|parameter| parameter.id == *clap_id)
                .ok_or_else(|| invalid("CLAP parameter lane has no descriptor"))?;
            if descriptor.read_only || descriptor.parameter_id()? != *parameter_id {
                return Err(invalid("CLAP parameter lane identity is invalid"));
            }
        }
        if fingerprint_clap_binary(&payload.library_path)? != payload.binary_sha256 {
            return Err(unavailable("CLAP binary changed since processor admission"));
        }
        let plugin_id = CString::new(payload.plugin_id)
            .map_err(|_| invalid("CLAP plugin ID contains a NUL byte"))?;
        let entry = unsafe { PluginEntry::load(payload.library_path.as_os_str()) }
            .map_err(|error| unavailable(format!("CLAP library load failed: {error}")))?;
        prepare_loaded(
            request,
            entry,
            plugin_id.as_c_str(),
            payload.state.as_deref(),
            &payload.parameters,
            &payload.parameter_ids,
        )
    }
}

fn prepare_loaded(
    request: IsolatedAudioProcessorWorkerPrepareRequest,
    entry: PluginEntry,
    plugin_id: &CStr,
    state: Option<&[u8]>,
    expected_parameters: &[super::ClapParameterDescriptor],
    parameter_ids: &[u32],
) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError> {
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| unavailable("CLAP library has no plugin factory"))?;
    if factory
        .plugin_descriptors()
        .filter(|descriptor| descriptor.id() == Some(plugin_id))
        .take(2)
        .count()
        != 1
    {
        return Err(unavailable(
            "CLAP plugin ID is missing or duplicated in the library",
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
    let mut instance =
        PluginInstance::<ClapHost>::new(|_| shared.clone(), |_| (), &entry, plugin_id, &info)
            .map_err(|error| unavailable(format!("CLAP instance creation failed: {error}")))?;
    shared.service_callbacks(&mut instance)?;
    if let Some(state) = state {
        let handle = instance.plugin_handle();
        let extension = handle
            .get_extension::<PluginState>()
            .ok_or_else(|| unavailable("CLAP plugin does not support state restoration"))?;
        let mut reader = state;
        extension
            .load(&handle, &mut reader)
            .map_err(|error| unavailable(format!("CLAP state restore failed: {error}")))?;
    }

    let expected_port_type = match request.render_contract().channel_layout {
        AudioChannelLayout::Mono => AudioPortType::MONO,
        AudioChannelLayout::Stereo => AudioPortType::STEREO,
        _ => return Err(invalid("CLAP channel layout is not yet supported")),
    };
    let channel_count = request.render_contract().channel_count();
    let handle = instance.plugin_handle();
    if list_parameters(&handle)? != expected_parameters {
        return Err(invalid(
            "CLAP parameter metadata changed since contract probe",
        ));
    }
    let ports = handle
        .get_extension::<PluginAudioPorts>()
        .ok_or_else(|| invalid("CLAP plugin does not expose audio ports"))?;
    for is_input in [true, false] {
        if ports.count(&handle, is_input) != 1 {
            return Err(invalid(
                "CLAP plugin must expose exactly one input and output port",
            ));
        }
        let mut buffer = AudioPortInfoBuffer::new();
        let port = ports
            .get(&handle, 0, is_input, &mut buffer)
            .ok_or_else(|| invalid("CLAP main audio port metadata is unavailable"))?;
        if !port.flags.contains(AudioPortFlags::IS_MAIN)
            || usize::try_from(port.channel_count).ok() != Some(channel_count)
            || port.port_type != Some(expected_port_type)
        {
            return Err(invalid(
                "CLAP main audio port layout differs from the render contract",
            ));
        }
    }

    let latency = handle
        .get_extension::<PluginLatency>()
        .map(|extension| extension.get(&handle) as usize)
        .unwrap_or(0);
    if latency != request.plugin_execution_contract().algorithmic_latency_frames() {
        return Err(invalid(
            "CLAP latency differs from the admitted execution contract",
        ));
    }
    let render = request.render_contract();
    let configuration = PluginAudioConfiguration {
        sample_rate: f64::from(render.sample_rate),
        min_frames_count: 1,
        max_frames_count: u32::try_from(render.max_block_frames)
            .map_err(|_| invalid("CLAP block size exceeds u32"))?,
    };
    let mut processor: PluginAudioProcessor<ClapHost> = instance
        .activate(|_, _| (), configuration)
        .map_err(|error| unavailable(format!("CLAP activation failed: {error}")))?
        .into();
    let tail = processor
        .plugin_handle()
        .get_extension::<PluginTail>()
        .map(|extension| extension.get(&processor.plugin_handle()))
        .unwrap_or(TailLength::Finite(0));
    let admitted_tail = match tail {
        TailLength::Finite(0) => crate::AudioProcessorTail::None,
        TailLength::Finite(frames) => crate::AudioProcessorTail::Finite(frames as usize),
        TailLength::Infinite => crate::AudioProcessorTail::Infinite,
    };
    if admitted_tail != request.plugin_execution_contract().tail() {
        processor.ensure_processing_stopped();
        drop(processor);
        let _ = instance.try_deactivate();
        return Err(invalid(
            "CLAP tail differs from the admitted execution contract",
        ));
    }
    let frames = render.max_block_frames;
    let event_capacity = frames
        .checked_mul(parameter_ids.len())
        .filter(|capacity| *capacity <= 1_048_576)
        .ok_or_else(|| invalid("CLAP parameter event capacity exceeds the supported bound"))?;
    let parameter_lanes = parameter_ids
        .iter()
        .map(|id| {
            let descriptor = expected_parameters
                .iter()
                .find(|parameter| parameter.id == *id)
                .ok_or_else(|| invalid("CLAP parameter lane is missing"))?;
            let clap_id = ClapId::from_raw(*id)
                .ok_or_else(|| invalid("CLAP parameter lane ID is invalid"))?;
            Ok((
                clap_id,
                descriptor.min_value,
                descriptor.max_value,
                descriptor.stepped,
            ))
        })
        .collect::<Result<Vec<_>, AudioProcessorHostError>>()?;
    let input = vec![vec![0.0; frames]; channel_count];
    let output = vec![vec![0.0; frames]; channel_count];
    Ok(Box::new(ClapWorker {
        processor: Some(processor),
        instance,
        contract: request.plugin_execution_contract(),
        input,
        output,
        input_ports: AudioPorts::with_capacity(channel_count, 1),
        output_ports: AudioPorts::with_capacity(channel_count, 1),
        parameter_lanes,
        input_events: EventBuffer::with_capacity(event_capacity),
        last_end: None,
        shared,
    }))
}

struct ClapWorker {
    processor: Option<PluginAudioProcessor<ClapHost>>,
    instance: PluginInstance<ClapHost>,
    contract: AudioProcessorExecutionContract,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    input_ports: AudioPorts,
    output_ports: AudioPorts,
    parameter_lanes: Vec<(ClapId, f64, f64, bool)>,
    input_events: EventBuffer,
    last_end: Option<i64>,
    shared: ClapHostShared,
}

impl IsolatedAudioProcessorWorker for ClapWorker {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.contract
    }

    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        AudioProcessorAuxiliaryInputContract { buses: Vec::new() }
    }

    fn enter_state(&mut self, start_sample: i64) -> Result<(), AudioProcessorHostError> {
        self.processor
            .as_mut()
            .ok_or_else(|| {
                AudioProcessorHostError::StateEntry("CLAP processor is closed".to_owned())
            })?
            .reset();
        self.last_end = Some(start_sample);
        Ok(())
    }

    fn process(
        &mut self,
        mut block: IsolatedAudioProcessorWorkerBlock<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        let frames = block.frames();
        let channels = self.input.len();
        if self.last_end != Some(block.start_sample())
            || frames == 0
            || frames > self.input[0].len()
            || block.main_interleaved().len() != frames * channels
        {
            return Err(AudioProcessorHostError::Process(
                "CLAP block violates prepared continuity or extent".to_owned(),
            ));
        }
        if self.shared.restart_requested() {
            return Err(AudioProcessorHostError::Process(
                "CLAP plugin requested a host restart; prepare a new instance".to_owned(),
            ));
        }
        if block.parameter_lane_count() != self.parameter_lanes.len() {
            return Err(AudioProcessorHostError::Process(
                "CLAP parameter lane count changed during processing".to_owned(),
            ));
        }
        self.input_events.clear();
        for (lane, &(clap_id, min, max, stepped)) in self.parameter_lanes.iter().enumerate() {
            let events = block.parameter_events(lane).ok_or_else(|| {
                AudioProcessorHostError::Process("CLAP parameter lane is missing".to_owned())
            })?;
            for event in events {
                if event.sample_offset as usize >= frames
                    || !event.value.is_finite()
                    || event.value < min
                    || event.value > max
                    || (stepped && event.value.fract() != 0.0)
                {
                    return Err(AudioProcessorHostError::Process(
                        "CLAP parameter event violates its admitted contract".to_owned(),
                    ));
                }
                self.input_events.push(&ParamValueEvent::new(
                    event.sample_offset,
                    clap_id,
                    Pckn::match_all(),
                    event.value,
                ));
            }
        }
        self.input_events.sort();
        for (frame, samples) in block.main_interleaved().chunks_exact(channels).enumerate() {
            for (channel, sample) in samples.iter().copied().enumerate() {
                self.input[channel][frame] = sample;
            }
        }
        for channel in &mut self.output {
            channel[..frames].fill(0.0);
        }
        let input_audio = self.input_ports.with_input_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_input_only(
                self.input
                    .iter_mut()
                    .map(|channel| InputChannel::variable(&mut channel[..frames])),
            ),
        }]);
        let mut output_audio = self.output_ports.with_output_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_output_only(
                self.output.iter_mut().map(|channel| &mut channel[..frames]),
            ),
        }]);
        let processor = self
            .processor
            .as_mut()
            .ok_or_else(|| AudioProcessorHostError::Process("CLAP processor is closed".to_owned()))?
            .ensure_processing_started()
            .map_err(|error| {
                AudioProcessorHostError::Process(format!("CLAP start failed: {error}"))
            })?;
        processor
            .process(
                &input_audio,
                &mut output_audio,
                &self.input_events.as_input(),
                &mut OutputEvents::void(),
                u64::try_from(block.start_sample()).ok(),
                None,
            )
            .map_err(|error| {
                AudioProcessorHostError::Process(format!("CLAP process failed: {error}"))
            })?;
        self.shared
            .service_callbacks(&mut self.instance)
            .map_err(|error| AudioProcessorHostError::Process(error.to_string()))?;
        for (frame, samples) in block.main_interleaved().chunks_exact_mut(channels).enumerate() {
            for (channel, sample) in samples.iter_mut().enumerate() {
                let value = self.output[channel][frame];
                if !value.is_finite() {
                    return Err(AudioProcessorHostError::Process(
                        "CLAP produced nonfinite PCM".to_owned(),
                    ));
                }
                *sample = value;
            }
        }
        self.last_end = block.start_sample().checked_add(frames as i64);
        Ok(())
    }
}

impl Drop for ClapWorker {
    fn drop(&mut self) {
        if let Some(mut processor) = self.processor.take() {
            processor.ensure_processing_stopped();
            drop(processor);
        }
        let _ = self.instance.try_deactivate();
    }
}

fn invalid(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.into())
}

fn unavailable(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::Unavailable(detail.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioProcessingMode, AudioProcessorTail, AudioRenderContract};
    use clack_extensions::audio_ports::{
        AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPortsImpl,
    };
    use clack_extensions::params::{
        ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
        PluginMainThreadParams, PluginParams,
    };
    use clack_plugin::events::spaces::CoreEventSpace;
    use clack_plugin::prelude::{
        Audio, ChannelPair, ClapId, DefaultPluginFactory, Events, HostAudioProcessorHandle,
        HostMainThreadHandle, HostSharedHandle, Plugin, PluginAudioConfiguration as PluginConfig,
        PluginDescriptor, PluginError, PluginExtensions, PluginMainThread, PluginShared, Process,
        ProcessStatus, SinglePluginEntry,
    };
    use mondrian_core::AudioChannelLayout;
    use std::sync::atomic::AtomicU32;
    use std::sync::atomic::{AtomicUsize, Ordering as TestOrdering};

    static DEACTIVATIONS: AtomicUsize = AtomicUsize::new(0);

    struct GainPlugin;
    struct GainMain;
    struct GainProcessor;

    struct AutomatedGainPlugin;
    struct AutomatedGainShared(AtomicU32);
    struct AutomatedGainMain<'a>(&'a AutomatedGainShared);
    struct AutomatedGainProcessor<'a>(&'a AutomatedGainShared);

    impl PluginShared<'_> for AutomatedGainShared {}

    impl Plugin for AutomatedGainPlugin {
        type AudioProcessor<'a> = AutomatedGainProcessor<'a>;
        type Shared<'a> = AutomatedGainShared;
        type MainThread<'a> = AutomatedGainMain<'a>;

        fn declare_extensions(
            builder: &mut PluginExtensions<Self>,
            _shared: Option<&AutomatedGainShared>,
        ) {
            builder.register::<PluginAudioPorts>().register::<PluginParams>();
        }
    }

    impl DefaultPluginFactory for AutomatedGainPlugin {
        fn get_descriptor() -> PluginDescriptor {
            PluginDescriptor::new("org.mondrian.test.automated-gain", "Automated Gain Fixture")
        }

        fn new_shared(_host: HostSharedHandle<'_>) -> Result<AutomatedGainShared, PluginError> {
            Ok(AutomatedGainShared(AtomicU32::new(1.0_f32.to_bits())))
        }

        fn new_main_thread<'a>(
            _host: HostMainThreadHandle<'a>,
            shared: &'a AutomatedGainShared,
        ) -> Result<AutomatedGainMain<'a>, PluginError> {
            Ok(AutomatedGainMain(shared))
        }
    }

    impl<'a> PluginMainThread<'a, AutomatedGainShared> for AutomatedGainMain<'a> {}

    impl PluginAudioPortsImpl for AutomatedGainMain<'_> {
        fn count(&self, _is_input: bool) -> u32 {
            1
        }

        fn get(&self, index: u32, _is_input: bool, writer: &mut AudioPortInfoWriter) {
            if index == 0 {
                writer.set(&AudioPortInfo {
                    id: ClapId::new(0),
                    name: b"main",
                    channel_count: 2,
                    flags: AudioPortFlags::IS_MAIN,
                    port_type: Some(AudioPortType::STEREO),
                    in_place_pair: None,
                });
            }
        }
    }

    impl PluginMainThreadParams for AutomatedGainMain<'_> {
        fn count(&self) -> u32 {
            1
        }

        fn get_info(&self, index: u32, writer: &mut ParamInfoWriter) {
            if index == 0 {
                writer.set(&ParamInfo {
                    id: ClapId::new(1),
                    flags: ParamInfoFlags::IS_AUTOMATABLE,
                    cookie: Default::default(),
                    name: b"Gain",
                    module: b"",
                    min_value: 0.0,
                    max_value: 1.0,
                    default_value: 1.0,
                });
            }
        }

        fn get_value(&self, id: ClapId) -> Option<f64> {
            (id == ClapId::new(1)).then(|| f32::from_bits(self.0 .0.load(Ordering::Relaxed)) as f64)
        }

        fn value_to_text(
            &self,
            _id: ClapId,
            _value: f64,
            _writer: &mut ParamDisplayWriter,
        ) -> std::fmt::Result {
            Err(std::fmt::Error)
        }

        fn text_to_value(&self, _id: ClapId, _text: &CStr) -> Option<f64> {
            None
        }

        fn flush(&self, _input: &clack_plugin::prelude::InputEvents, _output: &mut OutputEvents) {}
    }

    impl PluginAudioProcessorParams for AutomatedGainProcessor<'_> {
        fn flush(
            &mut self,
            _input: &clack_plugin::prelude::InputEvents,
            _output: &mut OutputEvents,
        ) {
        }
    }

    impl<'a>
        clack_plugin::plugin::PluginAudioProcessor<'a, AutomatedGainShared, AutomatedGainMain<'a>>
        for AutomatedGainProcessor<'a>
    {
        fn activate(
            _host: HostAudioProcessorHandle<'a>,
            _main: &AutomatedGainMain<'a>,
            shared: &'a AutomatedGainShared,
            _config: PluginConfig,
        ) -> Result<Self, PluginError> {
            Ok(Self(shared))
        }

        fn process(
            &mut self,
            _process: Process,
            mut audio: Audio,
            events: Events,
        ) -> Result<ProcessStatus, PluginError> {
            let mut gains = [f32::from_bits(self.0 .0.load(Ordering::Relaxed)); 4];
            let mut gain = gains[0];
            let mut next = events.input.iter().peekable();
            for (frame, slot) in gains.iter_mut().enumerate() {
                while next.peek().is_some_and(|event| event.header().time() as usize == frame) {
                    let Some(event) = next.next() else { break };
                    if let Some(CoreEventSpace::ParamValue(value)) = event.as_core_event()
                        && value.param_id() == Some(ClapId::new(1))
                    {
                        gain = value.value() as f32;
                    }
                }
                *slot = gain;
            }
            self.0 .0.store(gain.to_bits(), Ordering::Relaxed);
            let mut pair = audio.port_pair(0).ok_or(PluginError::Message("missing main port"))?;
            let mut channels =
                pair.channels()?.into_f32().ok_or(PluginError::Message("missing f32"))?;
            for channel in channels.iter_mut() {
                match channel {
                    ChannelPair::InputOutput(input, output) => {
                        for ((source, destination), gain) in
                            input.iter().zip(output.iter_mut()).zip(gains)
                        {
                            *destination = *source * gain;
                        }
                    }
                    ChannelPair::InPlace(samples) => {
                        for (sample, gain) in samples.iter_mut().zip(gains) {
                            *sample *= gain;
                        }
                    }
                    _ => return Err(PluginError::Message("unpaired channel")),
                }
            }
            Ok(ProcessStatus::Continue)
        }
    }

    impl Plugin for GainPlugin {
        type AudioProcessor<'a> = GainProcessor;
        type Shared<'a> = ();
        type MainThread<'a> = GainMain;

        fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&()>) {
            builder.register::<PluginAudioPorts>();
        }
    }

    impl DefaultPluginFactory for GainPlugin {
        fn get_descriptor() -> PluginDescriptor {
            PluginDescriptor::new("org.mondrian.test.gain", "Mondrian Test Gain")
        }

        fn new_shared(_host: HostSharedHandle<'_>) -> Result<(), PluginError> {
            Ok(())
        }

        fn new_main_thread<'a>(
            _host: HostMainThreadHandle<'a>,
            _shared: &'a (),
        ) -> Result<GainMain, PluginError> {
            Ok(GainMain)
        }
    }

    impl PluginMainThread<'_, ()> for GainMain {}

    impl PluginAudioPortsImpl for GainMain {
        fn count(&self, _is_input: bool) -> u32 {
            1
        }

        fn get(&self, index: u32, _is_input: bool, writer: &mut AudioPortInfoWriter) {
            if index == 0 {
                writer.set(&AudioPortInfo {
                    id: ClapId::new(0),
                    name: b"main",
                    channel_count: 2,
                    flags: AudioPortFlags::IS_MAIN,
                    port_type: Some(AudioPortType::STEREO),
                    in_place_pair: None,
                });
            }
        }
    }

    impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, (), GainMain> for GainProcessor {
        fn activate(
            _host: HostAudioProcessorHandle<'a>,
            _main_thread: &GainMain,
            _shared: &'a (),
            _config: PluginConfig,
        ) -> Result<Self, PluginError> {
            Ok(Self)
        }

        fn process(
            &mut self,
            _process: Process,
            mut audio: Audio,
            _events: Events,
        ) -> Result<ProcessStatus, PluginError> {
            let mut pair = audio.port_pair(0).ok_or(PluginError::Message("missing main port"))?;
            let mut channels = pair
                .channels()?
                .into_f32()
                .ok_or(PluginError::Message("missing Float32 channels"))?;
            for channel in channels.iter_mut() {
                match channel {
                    ChannelPair::InputOutput(input, output) => {
                        for (source, target) in input.iter().zip(output.iter_mut()) {
                            *target = *source * 2.0;
                        }
                    }
                    ChannelPair::InPlace(samples) => {
                        for sample in samples.iter_mut() {
                            *sample *= 2.0;
                        }
                    }
                    _ => return Err(PluginError::Message("unpaired channel")),
                }
            }
            Ok(ProcessStatus::Continue)
        }

        fn deactivate(self, _main_thread: &GainMain) {
            DEACTIVATIONS.fetch_add(1, TestOrdering::SeqCst);
        }
    }

    fn contract() -> AudioProcessorExecutionContract {
        AudioProcessorExecutionContract::new(0, AudioProcessorTail::None, true, true, true, 0)
            .expect("valid CLAP contract")
    }

    #[test]
    fn registration_rejects_duplicate_identity_and_relative_paths() {
        let helper = std::env::current_exe().expect("test executable path");
        let registration = ClapPluginRegistration {
            plugin_id: "org.example.gain".to_owned(),
            library_path: helper.clone(),
            binary_sha256: [1; 32],
            execution_contract: contract(),
            parameters: Vec::new(),
        };
        assert!(ClapAudioProcessorSpecResolver::new(
            helper.clone(),
            [registration.clone(), registration.clone()],
        )
        .is_err());
        assert!(ClapAudioProcessorSpecResolver::new(
            helper,
            [ClapPluginRegistration {
                library_path: PathBuf::from("gain.clap"),
                ..registration
            }],
        )
        .is_err());
    }

    #[test]
    fn worker_rejects_malformed_payload_before_native_loading() {
        let render_contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 512,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            payload: b"not json".to_vec(),
            plugin_execution_contract: contract(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: Vec::new(),
            render_contract,
        };
        assert!(matches!(
            ClapAudioProcessorWorkerFactory.prepare(request),
            Err(AudioProcessorHostError::InvalidContract(_))
        ));
    }

    #[test]
    fn worker_rejects_changed_binary_before_native_loading() {
        let temp = tempfile::tempdir().expect("temporary plugin directory");
        let library = temp.path().join("gain.clap");
        std::fs::write(&library, b"replacement binary").expect("write fake plugin");
        let payload = serde_json::to_vec(&ClapPayload {
            library_path: library,
            plugin_id: "org.example.gain".to_owned(),
            binary_sha256: [7; 32],
            state: None,
            parameters: Vec::new(),
            parameter_ids: Vec::new(),
        })
        .expect("payload");
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            payload,
            plugin_execution_contract: contract(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: Vec::new(),
            render_contract: AudioRenderContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                max_block_frames: 512,
                processing_mode: AudioProcessingMode::Offline,
                processor_session_scratch_budget_bytes: 1024 * 1024,
                public_output_lookahead_budget_frames: 4096,
                compensation_delay_scratch_budget_bytes: 1024 * 1024,
            },
        };
        assert!(matches!(
            ClapAudioProcessorWorkerFactory.prepare(request),
            Err(AudioProcessorHostError::Unavailable(message)) if message.contains("changed")
        ));
    }

    #[test]
    fn clap_abi_processes_stereo_blocks_with_continuity() {
        let deactivations_before = DEACTIVATIONS.load(TestOrdering::SeqCst);
        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<GainPlugin>>(c"/test/gain")
            .expect("static test CLAP entry");
        let render_contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 4,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            payload: Vec::new(),
            plugin_execution_contract: contract(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: Vec::new(),
            render_contract,
        };
        let mut worker = prepare_loaded(request, entry, c"org.mondrian.test.gain", None, &[], &[])
            .expect("prepare test CLAP plugin");
        worker.enter_state(12).expect("enter CLAP state");
        let mut samples = [0.25, -0.5, 1.0, 0.125];
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let block = IsolatedAudioProcessorWorkerBlock::new(
            12,
            2,
            render_contract,
            &mut samples,
            &auxiliary,
            &[],
            &[],
            &[],
        );
        worker.process(block).expect("process stereo CLAP block");
        assert_eq!(samples, [0.5, -1.0, 2.0, 0.25]);
        let bad_block = IsolatedAudioProcessorWorkerBlock::new(
            12,
            2,
            render_contract,
            &mut samples,
            &auxiliary,
            &[],
            &[],
            &[],
        );
        assert!(matches!(
            worker.process(bad_block),
            Err(AudioProcessorHostError::Process(_))
        ));
        drop(worker);
        assert_eq!(
            DEACTIVATIONS.load(TestOrdering::SeqCst),
            deactivations_before + 1
        );
    }

    #[test]
    #[ignore = "requires a built Mondrian executable and the Clack gain reference DLL"]
    fn discovered_resolver_lists_installed_reference_plugin() {
        let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_HELPER to mondrian executable");
        let plugin = std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_PLUGIN to Clack gain DLL");
        let resolver = DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin])
            .expect("discover installed CLAP plugin");
        assert_eq!(resolver.descriptors().len(), 1);
        assert_eq!(
            resolver.descriptors()[0].plugin_id,
            "org.rust-audio.clack.gain"
        );
    }

    #[test]
    #[ignore = "requires a built Mondrian executable and the Clack gain reference DLL"]
    fn discovered_resolver_probes_and_restores_author_state() {
        use crate::{
            AudioProcessingMode, AudioProcessorInsertionPoint, AudioProcessorOccurrence,
            AudioProcessorOccurrenceOwner,
        };
        use mondrian_core::{AudioProcessorInstanceId, ProgramOutputId};

        let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_HELPER to mondrian executable");
        let plugin = std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_PLUGIN to Clack gain DLL");
        let resolver = DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin])
            .expect("discover installed CLAP plugin");
        let definition = AudioProcessorDefinitionRef::Clap {
            plugin_id: "org.rust-audio.clack.gain".to_owned(),
            schema_version: 1,
        };
        let state = 0.5_f32.to_le_bytes();
        let instance = resolver
            .create_instance(
                "org.rust-audio.clack.gain",
                AudioRenderContract {
                    sample_rate: 48_000,
                    channel_layout: AudioChannelLayout::Stereo,
                    max_block_frames: 512,
                    processing_mode: AudioProcessingMode::Realtime,
                    processor_session_scratch_budget_bytes: 1024 * 1024,
                    public_output_lookahead_budget_frames: 4096,
                    compensation_delay_scratch_budget_bytes: 1024 * 1024,
                },
                Some(state.to_vec()),
            )
            .expect("capture installed plugin parameters");
        let parameters = instance
            .parameters
            .iter()
            .map(|(id, parameter)| (id.clone(), parameter.clone()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(parameters.len(), 1);
        assert_eq!(
            parameters[&mondrian_core::ParameterId::new("clap.param.1").expect("volume ID")]
                .automation
                .default_value,
            0.5
        );
        let request = AudioProcessorPrepareRequest::new(
            AudioProcessorOccurrence {
                instance_id: AudioProcessorInstanceId::new(),
                owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
                insertion: AudioProcessorInsertionPoint::PreFader,
            },
            &definition,
            &parameters,
            Some(&state),
            AudioRenderContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                max_block_frames: 512,
                processing_mode: AudioProcessingMode::Realtime,
                processor_session_scratch_budget_bytes: 1024 * 1024,
                public_output_lookahead_budget_frames: 4096,
                compensation_delay_scratch_budget_bytes: 1024 * 1024,
            },
        );
        let spec = resolver.resolve(request).expect("probe installed processor contract");
        let payload: ClapPayload =
            serde_json::from_slice(&spec.preparation_payload).expect("CLAP worker payload");
        assert_eq!(payload.state, Some(state.to_vec()));
        assert_eq!(payload.parameter_ids, vec![1]);
        assert_eq!(
            spec.plugin_execution_contract.algorithmic_latency_frames(),
            0
        );
    }

    #[test]
    #[ignore = "requires a built Mondrian executable and the Clack gain reference DLL"]
    fn reference_clap_receives_multiple_parameter_events() {
        use super::super::SharedParameterEvent;
        use mondrian_core::ParameterId;

        let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_HELPER to mondrian executable");
        let plugin = std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_PLUGIN to Clack gain DLL");
        let render_contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 4,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let registration = probe_clap_plugin_registration(
            &helper,
            &plugin,
            "org.rust-audio.clack.gain",
            render_contract,
            None,
        )
        .expect("probe Clack gain");
        let parameter_ids = vec![ParameterId::new("clap.param.1").expect("stable parameter ID")];
        let payload = serde_json::to_vec(&ClapPayload {
            library_path: registration.library_path,
            plugin_id: registration.plugin_id,
            binary_sha256: registration.binary_sha256,
            state: None,
            parameters: registration.parameters,
            parameter_ids: vec![1],
        })
        .expect("encode worker payload");
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            payload,
            plugin_execution_contract: registration.execution_contract,
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: parameter_ids.clone(),
            render_contract,
        };
        let mut worker = ClapAudioProcessorWorkerFactory.prepare(request).expect("prepare CLAP");
        worker.enter_state(0).expect("enter CLAP state");
        let mut samples = [1.0_f32; 8];
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let events = [
            SharedParameterEvent { lane: 0, sample_offset: 0, value: 0.25 },
            SharedParameterEvent { lane: 0, sample_offset: 2, value: 0.5 },
        ];
        let block = IsolatedAudioProcessorWorkerBlock::new(
            0,
            4,
            render_contract,
            &mut samples,
            &auxiliary,
            &[],
            &parameter_ids,
            &events,
        );
        worker.process(block).expect("process exact CLAP parameter events");
        // This Clack example multiplies the entire buffer once per event batch.
        // Its result verifies delivery of both events, but is not a sample-accurate oracle.
        assert_eq!(samples, [0.125; 8]);
    }

    #[test]
    fn clap_abi_preserves_sample_accurate_parameter_offsets() {
        use super::super::SharedParameterEvent;
        use mondrian_core::ParameterId;

        let entry = PluginEntry::load_from_clack::<SinglePluginEntry<AutomatedGainPlugin>>(
            c"/test/automated-gain",
        )
        .expect("static automated CLAP fixture");
        let render_contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 4,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let parameter_ids = vec![ParameterId::new("clap.param.1").expect("parameter identity")];
        let request = IsolatedAudioProcessorWorkerPrepareRequest {
            payload: Vec::new(),
            plugin_execution_contract: contract(),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: parameter_ids.clone(),
            render_contract,
        };
        let descriptor = super::super::ClapParameterDescriptor {
            id: 1,
            name: "Gain".to_owned(),
            min_value: 0.0,
            max_value: 1.0,
            default_value: 1.0,
            current_value: 1.0,
            automatable: true,
            stepped: false,
            enumeration: false,
            read_only: false,
            hidden: false,
        };
        let mut worker = prepare_loaded(
            request,
            entry,
            c"org.mondrian.test.automated-gain",
            None,
            &[descriptor],
            &[1],
        )
        .expect("prepare automated fixture");
        worker.enter_state(0).expect("enter fixture state");
        let mut samples = [1.0_f32; 8];
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let events = [
            SharedParameterEvent { lane: 0, sample_offset: 0, value: 0.25 },
            SharedParameterEvent { lane: 0, sample_offset: 2, value: 0.5 },
        ];
        let block = IsolatedAudioProcessorWorkerBlock::new(
            0,
            4,
            render_contract,
            &mut samples,
            &auxiliary,
            &[],
            &parameter_ids,
            &events,
        );
        worker.process(block).expect("process exact parameter offsets");
        assert_eq!(samples, [0.25, 0.25, 0.25, 0.25, 0.5, 0.5, 0.5, 0.5]);
    }
}
