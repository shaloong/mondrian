//! Bounded out-of-process discovery of installed CLAP descriptors.

use super::clap_worker::{ClapHost, ClapHostShared};
use super::ClapPluginRegistration;
use crate::{
    AudioProcessorExecutionContract, AudioProcessorHostError, AudioProcessorTail,
    AudioRenderContract,
};
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfoBuffer, AudioPortType, PluginAudioPorts,
};
use clack_extensions::latency::PluginLatency;
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_extensions::state::PluginState;
use clack_extensions::tail::{PluginTail, TailLength};
use clack_host::prelude::{
    ClapId, HostInfo, PluginAudioConfiguration, PluginAudioProcessor, PluginEntry, PluginInstance,
};
use mondrian_core::automation::{
    ParameterInterpolation, ParameterInvalidValuePolicy, ParameterNumericContract, ParameterSchema,
    ParameterUnit, PropertyValue,
};
use mondrian_core::{AudioChannelLayout, ParameterId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::{CString, OsStr};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Hidden application mode used to inspect one user-installed CLAP binary.
pub const CLAP_DISCOVERY_WORKER_ARGUMENT: &str = "--internal-clap-discovery-v1";
const REQUEST_ENV: &str = "MONDRIAN_INTERNAL_CLAP_DISCOVERY_REQUEST";
const RESPONSE_ENV: &str = "MONDRIAN_INTERNAL_CLAP_DISCOVERY_RESPONSE";
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(5);
const MAX_REQUEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 512 * 1024;
const MAX_CAPTURED_STATE_BYTES: usize = 64 * 1024;
const MAX_DESCRIPTORS: usize = 4096;
const MAX_PARAMETERS: u32 = 1024;
const MAX_CLAP_BINARY_BYTES: u64 = 512 * 1024 * 1024;

pub(super) fn fingerprint_clap_binary(
    library_path: &Path,
) -> Result<[u8; 32], AudioProcessorHostError> {
    let mut file = fs::File::open(library_path)
        .map_err(|error| unavailable(format!("CLAP binary open failed: {error}")))?;
    let metadata = file
        .metadata()
        .map_err(|error| unavailable(format!("CLAP binary metadata failed: {error}")))?;
    if !metadata.is_file() || metadata.len() > MAX_CLAP_BINARY_BYTES {
        return Err(invalid("CLAP binary is not a bounded regular file"));
    }
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| unavailable(format!("CLAP binary read failed: {error}")))?;
        if read == 0 {
            break;
        }
        size = size.saturating_add(read as u64);
        if size > MAX_CLAP_BINARY_BYTES {
            return Err(invalid("CLAP binary exceeds the fingerprint limit"));
        }
        hash.update(&buffer[..read]);
    }
    if size != metadata.len() {
        return Err(unavailable("CLAP binary changed during fingerprinting"));
    }
    Ok(hash.finalize().into())
}

/// One discovered CLAP identity for display and persistent definition matching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClapPluginDescriptor {
    /// Stable CLAP plugin ID; installed paths are not persisted as identity.
    pub plugin_id: String,
    /// User-facing plugin name supplied by the installed library.
    pub name: String,
    /// Optional vendor label supplied by the installed library.
    pub vendor: Option<String>,
    /// Optional vendor version supplied by the installed library.
    pub version: Option<String>,
}

/// Stable numeric parameter facts captured from one selected CLAP definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClapParameterDescriptor {
    /// Definition-local CLAP parameter ID, independent of scan order.
    pub id: u32,
    /// Plugin-supplied display name.
    pub name: String,
    /// Lowest admitted plain value.
    pub min_value: f64,
    /// Highest admitted plain value.
    pub max_value: f64,
    /// Definition default in plain units.
    pub default_value: f64,
    /// Current plain value after the probed state has been restored.
    pub current_value: f64,
    /// Whether the parameter supports host automation.
    pub automatable: bool,
    /// Whether values represent integer steps.
    pub stepped: bool,
    /// Whether the parameter is an integer-valued enumeration.
    pub enumeration: bool,
    /// Whether the plugin marks this parameter read-only.
    pub read_only: bool,
    /// Whether the plugin hides this parameter from ordinary controls.
    pub hidden: bool,
}

impl ClapParameterDescriptor {
    /// Stable definition-local authoring identity for this CLAP parameter.
    pub fn parameter_id(&self) -> Result<ParameterId, AudioProcessorHostError> {
        ParameterId::new(format!("clap.param.{}", self.id))
            .map_err(|error| invalid(format!("invalid CLAP parameter ID: {error}")))
    }

    /// Captured authoring contract for an editable parameter.
    pub fn authoring_schema(&self) -> Result<ParameterSchema, AudioProcessorHostError> {
        let default_value = if self.stepped {
            PropertyValue::Int(self.default_value as i64)
        } else {
            PropertyValue::Double(self.default_value)
        };
        let mut schema = ParameterSchema::v1(self.parameter_id()?, default_value);
        schema.is_animatable = self.automatable && !self.read_only;
        if self.stepped {
            schema.allowed_interpolations = vec![ParameterInterpolation::Hold];
        }
        let numeric = ParameterNumericContract::closed(
            self.min_value,
            self.max_value,
            self.stepped.then_some(1.0),
            ParameterInvalidValuePolicy::Reject,
        )
        .map_err(|error| invalid(format!("invalid CLAP numeric contract: {error}")))?;
        schema = schema.with_numeric_contract(ParameterUnit::Unitless, numeric);
        schema
            .validate()
            .map_err(|error| invalid(format!("invalid CLAP authoring schema: {error}")))?;
        Ok(schema)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryRequest {
    library_path: PathBuf,
    operation: DiscoveryOperation,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DiscoveryOperation {
    List,
    Probe {
        plugin_id: String,
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        max_block_frames: usize,
        state: Option<Vec<u8>>,
        capture_default_state: bool,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryResponse {
    descriptors: Vec<ClapPluginDescriptor>,
    probe: Option<ProbeResult>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ProbeResult {
    plugin_id: String,
    latency_frames: u32,
    tail: ProbeTail,
    parameters: Vec<ClapParameterDescriptor>,
    pub(super) captured_state: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
enum ProbeTail {
    None,
    Finite(u32),
    Infinite,
}

/// Discover descriptors without loading the native module in the editor process.
///
/// A child failure, hang, malformed response, or excessive metadata makes this
/// one library unavailable. The caller may continue scanning other libraries.
pub fn scan_clap_library_descriptors(
    helper_executable: &Path,
    library_path: &Path,
) -> Result<Vec<ClapPluginDescriptor>, AudioProcessorHostError> {
    let (_, response) =
        request_discovery(helper_executable, library_path, DiscoveryOperation::List)?;
    if response.probe.is_some() {
        return Err(invalid(
            "CLAP descriptor response unexpectedly contains a probe",
        ));
    }
    validate_descriptors(&response.descriptors)?;
    Ok(response.descriptors)
}

/// Probe one selected plugin's exact main-port, latency, and tail contract.
///
/// Native code executes only in the deadline-bound child. The subsequent
/// processor worker independently repeats these checks before audio admission.
pub fn probe_clap_plugin_registration(
    helper_executable: &Path,
    library_path: &Path,
    plugin_id: &str,
    render_contract: AudioRenderContract,
    state: Option<&[u8]>,
) -> Result<ClapPluginRegistration, AudioProcessorHostError> {
    probe_clap_plugin_registration_with_state(
        helper_executable,
        library_path,
        plugin_id,
        render_contract,
        state,
        false,
    )
    .map(|(registration, _)| registration)
}

pub(super) fn probe_clap_plugin_registration_with_state(
    helper_executable: &Path,
    library_path: &Path,
    plugin_id: &str,
    render_contract: AudioRenderContract,
    state: Option<&[u8]>,
    capture_default_state: bool,
) -> Result<(ClapPluginRegistration, Option<Vec<u8>>), AudioProcessorHostError> {
    if plugin_id.is_empty() || plugin_id.len() > 256 || plugin_id.contains('\0') {
        return Err(invalid("CLAP probe plugin ID is invalid"));
    }
    let canonical = fs::canonicalize(library_path)
        .map_err(|error| unavailable(format!("CLAP library is unavailable: {error}")))?;
    let fingerprint = fingerprint_clap_binary(&canonical)?;
    let (library_path, response) = request_discovery(
        helper_executable,
        library_path,
        DiscoveryOperation::Probe {
            plugin_id: plugin_id.to_owned(),
            sample_rate: render_contract.sample_rate,
            channel_layout: render_contract.channel_layout,
            max_block_frames: render_contract.max_block_frames,
            state: state.map(ToOwned::to_owned),
            capture_default_state,
        },
    )?;
    if fingerprint_clap_binary(&library_path)? != fingerprint {
        return Err(unavailable("CLAP binary changed during contract probing"));
    }
    if !response.descriptors.is_empty() {
        return Err(invalid(
            "CLAP probe response unexpectedly contains descriptors",
        ));
    }
    let probe = response.probe.ok_or_else(|| invalid("CLAP probe response is missing"))?;
    if probe.plugin_id != plugin_id {
        return Err(invalid("CLAP probe response changed plugin identity"));
    }
    validate_parameters(&probe.parameters)?;
    if probe
        .captured_state
        .as_ref()
        .is_some_and(|state| state.len() > MAX_CAPTURED_STATE_BYTES || !capture_default_state)
    {
        return Err(invalid("CLAP probe returned invalid captured state"));
    }
    let tail = match probe.tail {
        ProbeTail::None => AudioProcessorTail::None,
        ProbeTail::Finite(frames) => AudioProcessorTail::Finite(frames as usize),
        ProbeTail::Infinite => AudioProcessorTail::Infinite,
    };
    let execution_contract = AudioProcessorExecutionContract::new(
        probe.latency_frames as usize,
        tail,
        true,
        true,
        true,
        0,
    )?;
    Ok((
        ClapPluginRegistration {
            plugin_id: plugin_id.to_owned(),
            library_path,
            binary_sha256: fingerprint,
            execution_contract,
            parameters: probe.parameters,
        },
        probe.captured_state,
    ))
}

fn request_discovery(
    helper_executable: &Path,
    library_path: &Path,
    operation: DiscoveryOperation,
) -> Result<(PathBuf, DiscoveryResponse), AudioProcessorHostError> {
    if !helper_executable.is_absolute() || !library_path.is_absolute() {
        return Err(invalid(
            "CLAP discovery requires absolute helper and library paths",
        ));
    }
    let library_path = fs::canonicalize(library_path)
        .map_err(|error| unavailable(format!("CLAP library is unavailable: {error}")))?;
    if !library_path.is_file() {
        return Err(unavailable("CLAP library path is not a regular file"));
    }
    let temp = tempfile::tempdir()
        .map_err(|error| unavailable(format!("CLAP discovery staging failed: {error}")))?;
    let request_path = temp.path().join("request.json");
    let response_path = temp.path().join("response.json");
    let request =
        serde_json::to_vec(&DiscoveryRequest { library_path: library_path.clone(), operation })
            .map_err(|error| invalid(format!("CLAP discovery request failed: {error}")))?;
    if request.len() as u64 > MAX_REQUEST_BYTES {
        return Err(invalid("CLAP discovery request exceeds the metadata limit"));
    }
    fs::write(&request_path, request)
        .map_err(|error| unavailable(format!("CLAP discovery request write failed: {error}")))?;
    let mut child = Command::new(helper_executable)
        .arg(CLAP_DISCOVERY_WORKER_ARGUMENT)
        .env(REQUEST_ENV, &request_path)
        .env(RESPONSE_ENV, &response_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| unavailable(format!("CLAP discovery worker failed to start: {error}")))?;
    let deadline = Instant::now() + DISCOVERY_DEADLINE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => {
                return Err(unavailable(format!(
                    "CLAP discovery worker failed with status {status}"
                )));
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AudioProcessorHostError::WorkerDeadlineExceeded {
                    operation: "CLAP discovery",
                });
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(unavailable(format!(
                    "CLAP discovery worker wait failed: {error}"
                )));
            }
        }
    }
    let response = read_bounded(&response_path, "response", MAX_RESPONSE_BYTES)?;
    let response: DiscoveryResponse = serde_json::from_slice(&response)
        .map_err(|error| invalid(format!("CLAP discovery response is invalid: {error}")))?;
    Ok((library_path, response))
}

/// Execute the hidden discovery mode before any UI, GPU, or media initialization.
pub fn run_clap_discovery_worker() -> Result<(), AudioProcessorHostError> {
    let mut args = std::env::args_os();
    let _executable = args.next();
    if args.next().as_deref() != Some(OsStr::new(CLAP_DISCOVERY_WORKER_ARGUMENT))
        || args.next().is_some()
    {
        return Err(invalid("invalid CLAP discovery worker arguments"));
    }
    let request_path = std::env::var_os(REQUEST_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("CLAP discovery request path is missing"))?;
    let response_path = std::env::var_os(RESPONSE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("CLAP discovery response path is missing"))?;
    let request = read_bounded(&request_path, "request", MAX_REQUEST_BYTES)?;
    let request: DiscoveryRequest = serde_json::from_slice(&request)
        .map_err(|error| invalid(format!("CLAP discovery request is invalid: {error}")))?;
    if !request.library_path.is_absolute() {
        return Err(invalid("CLAP discovery library path must be absolute"));
    }
    let entry = unsafe { PluginEntry::load(request.library_path.as_os_str()) }
        .map_err(|error| unavailable(format!("CLAP library load failed: {error}")))?;
    let response = match request.operation {
        DiscoveryOperation::List => {
            DiscoveryResponse { descriptors: list_loaded(&entry)?, probe: None }
        }
        DiscoveryOperation::Probe {
            plugin_id,
            sample_rate,
            channel_layout,
            max_block_frames,
            state,
            capture_default_state,
        } => DiscoveryResponse {
            descriptors: Vec::new(),
            probe: Some(probe_loaded(
                &entry,
                &plugin_id,
                sample_rate,
                channel_layout,
                max_block_frames,
                state.as_deref(),
                capture_default_state,
            )?),
        },
    };
    let response = serde_json::to_vec(&response)
        .map_err(|error| invalid(format!("CLAP discovery response encoding failed: {error}")))?;
    if response.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(invalid(
            "CLAP discovery response exceeds the metadata limit",
        ));
    }
    fs::write(response_path, response)
        .map_err(|error| unavailable(format!("CLAP discovery response write failed: {error}")))
}

fn list_loaded(entry: &PluginEntry) -> Result<Vec<ClapPluginDescriptor>, AudioProcessorHostError> {
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| unavailable("CLAP library has no plugin factory"))?;
    let mut descriptors = Vec::new();
    for descriptor in factory.plugin_descriptors() {
        if descriptors.len() == MAX_DESCRIPTORS {
            return Err(invalid("CLAP library exceeds descriptor capacity"));
        }
        let id = descriptor
            .id()
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| invalid("CLAP descriptor has no valid UTF-8 ID"))?;
        let name = descriptor
            .name()
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| invalid("CLAP descriptor has no valid UTF-8 name"))?;
        descriptors.push(ClapPluginDescriptor {
            plugin_id: id.to_owned(),
            name: name.to_owned(),
            vendor: descriptor.vendor().and_then(|value| value.to_str().ok()).map(str::to_owned),
            version: descriptor.version().and_then(|value| value.to_str().ok()).map(str::to_owned),
        });
    }
    validate_descriptors(&descriptors)?;
    Ok(descriptors)
}

pub(super) fn probe_loaded(
    entry: &PluginEntry,
    plugin_id: &str,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    max_block_frames: usize,
    state: Option<&[u8]>,
    capture_default_state: bool,
) -> Result<ProbeResult, AudioProcessorHostError> {
    let expected_type = match channel_layout {
        AudioChannelLayout::Mono => AudioPortType::MONO,
        AudioChannelLayout::Stereo => AudioPortType::STEREO,
        _ => return Err(invalid("CLAP probe channel layout is unsupported")),
    };
    let max_frames = u32::try_from(max_block_frames)
        .ok()
        .filter(|frames| *frames > 0)
        .ok_or_else(|| invalid("CLAP probe block extent is invalid"))?;
    if sample_rate == 0 {
        return Err(invalid("CLAP probe sample rate is invalid"));
    }
    let plugin_c_id =
        CString::new(plugin_id).map_err(|_| invalid("CLAP probe plugin ID contains a NUL byte"))?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| unavailable("CLAP library has no plugin factory"))?;
    if factory
        .plugin_descriptors()
        .filter(|descriptor| descriptor.id() == Some(plugin_c_id.as_c_str()))
        .take(2)
        .count()
        != 1
    {
        return Err(unavailable("CLAP probe plugin ID is missing or duplicated"));
    }
    let info = HostInfo::new(
        "Mondrian",
        "Mondrian",
        "https://github.com/shaloong/mondrian",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| invalid(format!("invalid CLAP host identity: {error}")))?;
    let shared = ClapHostShared::new();
    let mut instance = PluginInstance::<ClapHost>::new(
        |_| shared.clone(),
        |_| (),
        entry,
        plugin_c_id.as_c_str(),
        &info,
    )
    .map_err(|error| unavailable(format!("CLAP probe instance failed: {error}")))?;
    shared.service_callbacks(&mut instance)?;
    if let Some(state) = state {
        let handle = instance.plugin_handle();
        let extension = handle
            .get_extension::<PluginState>()
            .ok_or_else(|| unavailable("CLAP plugin does not support state restoration"))?;
        let mut reader = state;
        extension
            .load(&handle, &mut reader)
            .map_err(|error| unavailable(format!("CLAP probe state restore failed: {error}")))?;
        shared.service_callbacks(&mut instance)?;
    }
    let handle = instance.plugin_handle();
    let parameters = list_parameters(&handle)?;
    let captured_state = if capture_default_state && state.is_none() {
        if let Some(extension) = handle.get_extension::<PluginState>() {
            let mut writer = BoundedStateWriter::default();
            let saved = extension.save(&handle, &mut writer);
            if writer.exceeded {
                return Err(invalid("CLAP state exceeds the capture limit"));
            }
            saved.map_err(|error| unavailable(format!("CLAP state capture failed: {error}")))?;
            Some(writer.bytes)
        } else {
            None
        }
    } else {
        None
    };
    let ports = handle
        .get_extension::<PluginAudioPorts>()
        .ok_or_else(|| invalid("CLAP plugin does not expose audio ports"))?;
    for is_input in [true, false] {
        if ports.count(&handle, is_input) != 1 {
            return Err(invalid(
                "CLAP plugin must expose one main input and output port",
            ));
        }
        let mut buffer = AudioPortInfoBuffer::new();
        let port = ports
            .get(&handle, 0, is_input, &mut buffer)
            .ok_or_else(|| invalid("CLAP main port metadata is unavailable"))?;
        if !port.flags.contains(AudioPortFlags::IS_MAIN)
            || port.port_type != Some(expected_type)
            || usize::try_from(port.channel_count).ok() != Some(channel_layout.channel_count())
        {
            return Err(invalid("CLAP main port differs from the requested layout"));
        }
    }
    let latency_frames = handle
        .get_extension::<PluginLatency>()
        .map(|extension| extension.get(&handle))
        .unwrap_or(0);
    let configuration = PluginAudioConfiguration {
        sample_rate: f64::from(sample_rate),
        min_frames_count: 1,
        max_frames_count: max_frames,
    };
    let mut processor: PluginAudioProcessor<ClapHost> = instance
        .activate(|_, _| (), configuration)
        .map_err(|error| unavailable(format!("CLAP probe activation failed: {error}")))?
        .into();
    let tail = processor
        .plugin_handle()
        .get_extension::<PluginTail>()
        .map(|extension| extension.get(&processor.plugin_handle()))
        .unwrap_or(TailLength::Finite(0));
    processor.ensure_processing_stopped();
    drop(processor);
    instance
        .try_deactivate()
        .map_err(|error| unavailable(format!("CLAP probe deactivation failed: {error}")))?;
    shared.service_callbacks(&mut instance)?;
    let tail = match tail {
        TailLength::Finite(0) => ProbeTail::None,
        TailLength::Finite(frames) => ProbeTail::Finite(frames),
        TailLength::Infinite => ProbeTail::Infinite,
    };
    Ok(ProbeResult {
        plugin_id: plugin_id.to_owned(),
        latency_frames,
        tail,
        parameters,
        captured_state,
    })
}

#[derive(Default)]
struct BoundedStateWriter {
    bytes: Vec<u8>,
    exceeded: bool,
}

impl Write for BoundedStateWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(buf.len())
            .is_none_or(|len| len > MAX_CAPTURED_STATE_BYTES)
        {
            self.exceeded = true;
            return Err(io::Error::other("CLAP state exceeds the capture limit"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn list_parameters(
    handle: &clack_host::prelude::PluginMainThreadHandle<'_>,
) -> Result<Vec<ClapParameterDescriptor>, AudioProcessorHostError> {
    let Some(extension) = handle.get_extension::<PluginParams>() else {
        return Ok(Vec::new());
    };
    let count = extension.count(handle);
    if count > MAX_PARAMETERS {
        return Err(invalid("CLAP parameter count exceeds the supported bound"));
    }
    let mut parameters = Vec::with_capacity(count as usize);
    for index in 0..count {
        let mut buffer = ParamInfoBuffer::new();
        let info = extension
            .get_info(handle, index, &mut buffer)
            .ok_or_else(|| invalid("CLAP parameter metadata is missing"))?;
        let name = std::str::from_utf8(info.name)
            .map_err(|_| invalid("CLAP parameter name is not UTF-8"))?;
        parameters.push(ClapParameterDescriptor {
            id: info.id.get(),
            name: name.to_owned(),
            min_value: info.min_value,
            max_value: info.max_value,
            default_value: info.default_value,
            current_value: extension
                .get_value(handle, info.id)
                .ok_or_else(|| invalid("CLAP parameter current value is unavailable"))?,
            automatable: info.flags.contains(ParamInfoFlags::IS_AUTOMATABLE),
            stepped: info.flags.contains(ParamInfoFlags::IS_STEPPED),
            enumeration: info.flags.contains(ParamInfoFlags::IS_ENUM),
            read_only: info.flags.contains(ParamInfoFlags::IS_READONLY),
            hidden: info.flags.contains(ParamInfoFlags::IS_HIDDEN),
        });
    }
    validate_parameters(&parameters)?;
    Ok(parameters)
}

pub(super) fn validate_parameters(
    parameters: &[ClapParameterDescriptor],
) -> Result<(), AudioProcessorHostError> {
    if parameters.len() > MAX_PARAMETERS as usize {
        return Err(invalid("CLAP parameter count exceeds the supported bound"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for parameter in parameters {
        if ClapId::from_raw(parameter.id).is_none()
            || !seen.insert(parameter.id)
            || parameter.name.is_empty()
            || parameter.name.len() > 256
            || parameter.name.chars().any(char::is_control)
            || !parameter.min_value.is_finite()
            || !parameter.max_value.is_finite()
            || !parameter.default_value.is_finite()
            || !parameter.current_value.is_finite()
            || parameter.min_value > parameter.max_value
            || !(parameter.min_value..=parameter.max_value).contains(&parameter.default_value)
            || !(parameter.min_value..=parameter.max_value).contains(&parameter.current_value)
            || (parameter.enumeration && !parameter.stepped)
            || (parameter.stepped
                && [
                    parameter.min_value,
                    parameter.max_value,
                    parameter.default_value,
                    parameter.current_value,
                ]
                .into_iter()
                .any(|value| value.fract() != 0.0 || value.abs() > 9_007_199_254_740_992.0))
        {
            return Err(invalid("CLAP parameter metadata is invalid or ambiguous"));
        }
    }
    Ok(())
}

fn validate_descriptors(
    descriptors: &[ClapPluginDescriptor],
) -> Result<(), AudioProcessorHostError> {
    if descriptors.len() > MAX_DESCRIPTORS {
        return Err(invalid("CLAP library exceeds descriptor capacity"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for descriptor in descriptors {
        if descriptor.plugin_id.is_empty()
            || descriptor.plugin_id.len() > 256
            || descriptor.name.is_empty()
            || descriptor.name.len() > 256
            || descriptor.vendor.as_ref().is_some_and(|value| value.len() > 256)
            || descriptor.version.as_ref().is_some_and(|value| value.len() > 256)
            || !seen.insert(&descriptor.plugin_id)
        {
            return Err(invalid("CLAP descriptor identity or metadata is invalid"));
        }
    }
    Ok(())
}

fn read_bounded(path: &Path, name: &str, maximum: u64) -> Result<Vec<u8>, AudioProcessorHostError> {
    let file = fs::File::open(path)
        .map_err(|error| unavailable(format!("CLAP discovery {name} open failed: {error}")))?;
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| unavailable(format!("CLAP discovery {name} read failed: {error}")))?;
    if bytes.len() as u64 > maximum {
        return Err(invalid(format!(
            "CLAP discovery {name} exceeds the metadata limit"
        )));
    }
    Ok(bytes)
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
    use clack_plugin::prelude::{
        DefaultPluginFactory, HostMainThreadHandle, HostSharedHandle, Plugin, PluginDescriptor,
        PluginError, SinglePluginEntry,
    };

    struct DiscoveryFixture;

    impl Plugin for DiscoveryFixture {
        type AudioProcessor<'a> = ();
        type Shared<'a> = ();
        type MainThread<'a> = ();
    }

    impl DefaultPluginFactory for DiscoveryFixture {
        fn get_descriptor() -> PluginDescriptor {
            PluginDescriptor::new("org.mondrian.discovery-fixture", "Discovery Fixture")
                .with_vendor("Mondrian")
                .with_version("1.2.3")
        }

        fn new_shared(_host: HostSharedHandle<'_>) -> Result<(), PluginError> {
            Ok(())
        }

        fn new_main_thread<'a>(
            _host: HostMainThreadHandle<'a>,
            _shared: &'a (),
        ) -> Result<(), PluginError> {
            Ok(())
        }
    }

    #[test]
    fn discovers_clap_descriptor_from_abi_factory() {
        let entry =
            PluginEntry::load_from_clack::<SinglePluginEntry<DiscoveryFixture>>(c"/test/discovery")
                .expect("static discovery entry");
        let descriptors = list_loaded(&entry).expect("discover static CLAP entry");
        assert_eq!(
            descriptors,
            vec![ClapPluginDescriptor {
                plugin_id: "org.mondrian.discovery-fixture".to_owned(),
                name: "Discovery Fixture".to_owned(),
                vendor: Some("Mondrian".to_owned()),
                version: Some("1.2.3".to_owned()),
            }]
        );
    }

    #[test]
    fn probe_rejects_plugin_without_explicit_main_audio_ports() {
        let entry =
            PluginEntry::load_from_clack::<SinglePluginEntry<DiscoveryFixture>>(c"/test/discovery")
                .expect("static discovery entry");
        assert!(matches!(
            probe_loaded(
                &entry,
                "org.mondrian.discovery-fixture",
                48_000,
                AudioChannelLayout::Stereo,
                512,
                None,
                false,
            ),
            Err(AudioProcessorHostError::InvalidContract(_))
        ));
    }

    #[test]
    fn rejects_duplicate_or_oversized_discovery_metadata() {
        let descriptor = ClapPluginDescriptor {
            plugin_id: "org.example.gain".to_owned(),
            name: "Gain".to_owned(),
            vendor: None,
            version: None,
        };
        assert!(validate_descriptors(&[descriptor.clone(), descriptor.clone()]).is_err());
        assert!(validate_descriptors(&[ClapPluginDescriptor {
            name: "x".repeat(257),
            ..descriptor
        }])
        .is_err());
    }

    #[test]
    fn parameter_metadata_has_stable_ids_and_exact_numeric_contracts() {
        let parameter = ClapParameterDescriptor {
            id: 7,
            name: "Mix".to_owned(),
            min_value: 0.0,
            max_value: 1.0,
            default_value: 0.5,
            current_value: 0.5,
            automatable: true,
            stepped: false,
            enumeration: false,
            read_only: false,
            hidden: false,
        };
        validate_parameters(std::slice::from_ref(&parameter)).expect("valid CLAP parameter");
        let schema = parameter.authoring_schema().expect("authoring schema");
        assert_eq!(schema.parameter_id.as_str(), "clap.param.7");
        assert_eq!(schema.default_value, PropertyValue::Double(0.5));
        assert!(schema.is_animatable);
        assert_eq!(schema.numeric.expect("numeric bounds").hard_range.max, 1.0);
        assert!(validate_parameters(&[parameter.clone(), parameter.clone()]).is_err());
        assert!(validate_parameters(&[ClapParameterDescriptor {
            default_value: 2.0,
            ..parameter.clone()
        }])
        .is_err());
        assert!(validate_parameters(&[ClapParameterDescriptor {
            id: u32::MAX,
            ..parameter.clone()
        }])
        .is_err());
        assert!(validate_parameters(&[ClapParameterDescriptor {
            name: "unsafe\nlabel".to_owned(),
            ..parameter.clone()
        }])
        .is_err());
        assert!(
            validate_parameters(&[ClapParameterDescriptor { stepped: true, ..parameter }]).is_err()
        );
    }

    #[test]
    fn bounded_reader_rejects_oversized_worker_response() {
        let temp = tempfile::tempdir().expect("temporary discovery directory");
        let response_path = temp.path().join("response.json");
        fs::write(&response_path, vec![b'x'; MAX_RESPONSE_BYTES as usize + 1])
            .expect("write oversized response");
        assert!(matches!(
            read_bounded(&response_path, "response", MAX_RESPONSE_BYTES),
            Err(AudioProcessorHostError::InvalidContract(_))
        ));
    }

    #[test]
    fn captured_state_writer_enforces_limit_across_multiple_writes() {
        let mut writer = BoundedStateWriter::default();
        writer.write_all(&vec![7; MAX_CAPTURED_STATE_BYTES - 1]).expect("initial state");
        writer.write_all(&[8]).expect("exact limit");
        assert!(writer.write_all(&[9]).is_err());
        assert!(writer.exceeded);
        assert_eq!(writer.bytes.len(), MAX_CAPTURED_STATE_BYTES);
    }

    #[test]
    fn binary_fingerprint_changes_with_file_and_rejects_oversized_file() {
        let temp = tempfile::tempdir().expect("temporary plugin directory");
        let library = temp.path().join("gain.clap");
        fs::write(&library, b"plugin revision one").expect("first revision");
        let first = fingerprint_clap_binary(&library).expect("first fingerprint");
        fs::write(&library, b"plugin revision two").expect("second revision");
        assert_ne!(
            first,
            fingerprint_clap_binary(&library).expect("second fingerprint")
        );
        let file = fs::OpenOptions::new().write(true).open(&library).expect("open plugin");
        file.set_len(MAX_CLAP_BINARY_BYTES + 1).expect("grow sparse file");
        assert!(matches!(
            fingerprint_clap_binary(&library),
            Err(AudioProcessorHostError::InvalidContract(_))
        ));
    }

    #[test]
    fn scanner_reports_child_failure_without_editor_crash() {
        let test_executable = std::env::current_exe().expect("test executable path");
        assert!(matches!(
            scan_clap_library_descriptors(&test_executable, &test_executable),
            Err(AudioProcessorHostError::Unavailable(_))
        ));
    }

    #[test]
    #[ignore = "requires a built Mondrian executable and the Clack gain reference DLL"]
    fn installed_clap_reference_is_discovered_across_process_boundary() {
        let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_HELPER to mondrian-app executable");
        let plugin = std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_PLUGIN to Clack gain DLL");
        let descriptors = scan_clap_library_descriptors(&helper, &plugin)
            .expect("discover installed CLAP reference plugin");
        assert!(descriptors.iter().any(|descriptor| {
            descriptor.plugin_id == "org.rust-audio.clack.gain"
                && descriptor.name == "Clack Gain Example"
        }));
    }

    #[test]
    #[ignore = "requires a built Mondrian executable and the Clack gain reference DLL"]
    fn installed_clap_reference_default_state_is_captured_in_child() {
        let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_HELPER to mondrian executable");
        let plugin = std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_PLUGIN to Clack gain DLL");
        let render_contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 512,
            processing_mode: crate::AudioProcessingMode::Realtime,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let (_, state) = probe_clap_plugin_registration_with_state(
            &helper,
            &plugin,
            "org.rust-audio.clack.gain",
            render_contract,
            None,
            true,
        )
        .expect("capture gain factory state");
        assert_eq!(state, Some(1.0_f32.to_le_bytes().to_vec()));
    }

    #[test]
    #[ignore = "requires a built Mondrian executable and the Clack gain reference DLL"]
    fn installed_clap_reference_contract_is_probed_in_child() {
        let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_HELPER to mondrian executable");
        let plugin = std::env::var_os("MONDRIAN_CLAP_TEST_PLUGIN")
            .map(PathBuf::from)
            .expect("set MONDRIAN_CLAP_TEST_PLUGIN to Clack gain DLL");
        let render_contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 512,
            processing_mode: crate::AudioProcessingMode::Realtime,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let registration = probe_clap_plugin_registration(
            &helper,
            &plugin,
            "org.rust-audio.clack.gain",
            render_contract,
            Some(&0.5_f32.to_le_bytes()),
        )
        .expect("probe installed gain plugin");
        assert_eq!(registration.plugin_id, "org.rust-audio.clack.gain");
        assert_eq!(
            registration.binary_sha256,
            fingerprint_clap_binary(&plugin).expect("reference fingerprint")
        );
        assert_eq!(
            registration.execution_contract.algorithmic_latency_frames(),
            0
        );
        assert_eq!(
            registration.execution_contract.tail(),
            AudioProcessorTail::None
        );
        assert!(registration.execution_contract.requires_state_entry());
        assert_eq!(registration.parameters.len(), 1);
        let volume = &registration.parameters[0];
        assert_eq!(
            volume.parameter_id().expect("volume identity").as_str(),
            "clap.param.1"
        );
        assert_eq!(volume.name, "Volume");
        assert_eq!((volume.min_value, volume.max_value), (0.0, 1.0));
        assert!(volume.automatable);
    }
}
