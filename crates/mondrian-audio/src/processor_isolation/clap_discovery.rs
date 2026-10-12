//! Bounded out-of-process discovery of installed CLAP descriptors.

use super::clap_worker::{configure_render_mode, ClapHost, ClapHostShared};
use super::protocol::{auxiliary_from_wire, auxiliary_to_wire, AuxiliaryBusWire};
use super::ClapPluginRegistration;
use crate::{
    AudioProcessingMode, AudioProcessorAuxiliaryInputBusContract,
    AudioProcessorAuxiliaryInputContract, AudioProcessorExecutionContract, AudioProcessorHostError,
    AudioProcessorTail, AudioRenderContract,
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
pub(super) const MAX_DESCRIPTORS: usize = 4096;
pub(super) const MAX_PARAMETERS: u32 = 1024;
const MAX_INPUT_PORTS: u32 = 32;
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
    NoteInputMetadata(super::clap_note_input_metadata::NoteInputMetadataRequest),
    Probe {
        plugin_id: String,
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        max_block_frames: usize,
        offline: bool,
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
    pub(super) latency_frames: u32,
    offline: bool,
    tail: ProbeTail,
    parameters: Vec<ClapParameterDescriptor>,
    pub(super) auxiliary_buses: Vec<AuxiliaryBusWire>,
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
            offline: render_contract.processing_mode == AudioProcessingMode::Offline,
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
    if probe.plugin_id != plugin_id
        || probe.offline != (render_contract.processing_mode == AudioProcessingMode::Offline)
    {
        return Err(invalid("CLAP probe response changed plugin identity"));
    }
    validate_parameters(&probe.parameters)?;
    if probe.auxiliary_buses.len() >= MAX_INPUT_PORTS as usize {
        return Err(invalid("CLAP probe returned excessive auxiliary ports"));
    }
    let auxiliary_inputs = auxiliary_from_wire(probe.auxiliary_buses);
    auxiliary_inputs.validate(render_contract.channel_layout)?;
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
        render_contract.processing_mode == AudioProcessingMode::Realtime,
        render_contract.processing_mode == AudioProcessingMode::Offline,
        0,
    )?;
    Ok((
        ClapPluginRegistration {
            plugin_id: plugin_id.to_owned(),
            library_path,
            binary_sha256: fingerprint,
            execution_contract,
            parameters: probe.parameters,
            auxiliary_inputs,
        },
        probe.captured_state,
    ))
}

fn request_discovery(
    helper_executable: &Path,
    library_path: &Path,
    operation: DiscoveryOperation,
) -> Result<(PathBuf, DiscoveryResponse), AudioProcessorHostError> {
    let (library_path, bytes, _process) =
        request_discovery_bytes(helper_executable, library_path, operation)?;
    let response = serde_json::from_slice(&bytes)
        .map_err(|error| invalid(format!("CLAP discovery response is invalid: {error}")))?;
    Ok((library_path, response))
}

/// Inspect actual inactive input-note facts without admitting note processing.
///
/// This additive versioned operation requires a current product helper. An old
/// helper rejects its unknown operation and yields Unavailable, never empty
/// metadata. It does not probe/relax audio buses, activate, or register NotePorts
/// host support. No author state or persistent registry is created.
pub fn probe_clap_note_input_metadata(
    helper_executable: &Path,
    library_path: &Path,
    plugin_id: &str,
    state: Option<&[u8]>,
) -> Result<super::clap_note_input_metadata::ClapNoteInputMetadata, AudioProcessorHostError> {
    use super::clap_note_input_metadata as metadata;
    metadata::validate_request(plugin_id, state)?;
    if !helper_executable.is_absolute() || !library_path.is_absolute() {
        return Err(invalid(
            "CLAP note metadata requires absolute helper and library paths",
        ));
    }
    let canonical = fs::canonicalize(library_path)
        .map_err(|error| unavailable(format!("CLAP library is unavailable: {error}")))?;
    let fingerprint = fingerprint_clap_binary(&canonical)?;
    let (library_path, bytes, _process) = request_discovery_bytes(
        helper_executable,
        &canonical,
        DiscoveryOperation::NoteInputMetadata(metadata::NoteInputMetadataRequest {
            schema_version: metadata::SCHEMA_VERSION,
            plugin_id: plugin_id.to_owned(),
            state: state.map(ToOwned::to_owned),
        }),
    )?;
    if fingerprint_clap_binary(&library_path)? != fingerprint {
        return Err(unavailable(
            "CLAP binary changed during note metadata inspection",
        ));
    }
    let response: metadata::ClapNoteInputMetadata = serde_json::from_slice(&bytes)
        .map_err(|error| invalid(format!("CLAP note metadata response is invalid: {error}")))?;
    metadata::validate_response(
        &response,
        plugin_id,
        fingerprint,
        metadata::state_digest(state),
    )?;
    Ok(response)
}

// Both concrete response decoders share the existing supervisor and bounds.
fn request_discovery_bytes(
    helper_executable: &Path,
    library_path: &Path,
    operation: DiscoveryOperation,
) -> Result<(PathBuf, Vec<u8>, super::supervisor::ProcessOwner), AudioProcessorHostError> {
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
    // Adopt the child and staging immediately; every post-spawn failure/unwind
    // uses the same existing OS owner as audio workers, without claiming that
    // permanent OS refusal or a successful-kill wait is kernel-time bounded.
    let mut process = super::supervisor::ProcessOwner::with_staging(
        Command::new(helper_executable)
            .arg(CLAP_DISCOVERY_WORKER_ARGUMENT)
            .env(REQUEST_ENV, &request_path)
            .env(RESPONSE_ENV, &response_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                unavailable(format!("CLAP discovery worker failed to start: {error}"))
            })?,
        temp,
    );
    let deadline = Instant::now() + DISCOVERY_DEADLINE;
    loop {
        match process.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => {
                return Err(unavailable(format!(
                    "CLAP discovery worker failed with status {status}"
                )));
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                return Err(retire_discovery_failure(
                    &mut process,
                    AudioProcessorHostError::WorkerDeadlineExceeded { operation: "CLAP discovery" },
                ));
            }
            Err(error) => {
                return Err(retire_discovery_failure(
                    &mut process,
                    unavailable(format!("CLAP discovery worker wait failed: {error}")),
                ));
            }
        }
    }
    let response = read_bounded(&response_path, "response", MAX_RESPONSE_BYTES)?;
    // The caller retains this owner through decoding (and metadata identity
    // verification), so directory lifetime covers the complete response use.
    Ok((library_path, response, process))
}

pub(super) fn retire_discovery_failure(
    process: &mut super::supervisor::ProcessOwner,
    primary: AudioProcessorHostError,
) -> AudioProcessorHostError {
    process.terminate_reported("CLAP discovery failure");
    primary
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
    if let DiscoveryOperation::NoteInputMetadata(query) = &request.operation {
        if query.schema_version != super::clap_note_input_metadata::SCHEMA_VERSION {
            return Err(invalid("unsupported CLAP note metadata schema version"));
        }
        super::clap_note_input_metadata::validate_request(
            &query.plugin_id,
            query.state.as_deref(),
        )?;
    }
    let metadata_fingerprint =
        if matches!(&request.operation, DiscoveryOperation::NoteInputMetadata(_)) {
            Some(fingerprint_clap_binary(&request.library_path)?)
        } else {
            None
        };
    let entry = unsafe { PluginEntry::load(request.library_path.as_os_str()) }
        .map_err(|error| unavailable(format!("CLAP library load failed: {error}")))?;
    let response = match request.operation {
        DiscoveryOperation::List => serde_json::to_vec(&DiscoveryResponse {
            descriptors: list_loaded(&entry)?,
            probe: None,
        }),
        DiscoveryOperation::NoteInputMetadata(query) => {
            let fingerprint = metadata_fingerprint
                .ok_or_else(|| invalid("CLAP note metadata fingerprint is missing"))?;
            let metadata = super::clap_note_input_metadata::inspect_loaded(
                &entry,
                &query.plugin_id,
                fingerprint,
                query.state.as_deref(),
            )?;
            if fingerprint_clap_binary(&request.library_path)? != fingerprint {
                return Err(unavailable(
                    "CLAP binary changed during native note metadata inspection",
                ));
            }
            serde_json::to_vec(&metadata)
        }
        DiscoveryOperation::Probe {
            plugin_id,
            sample_rate,
            channel_layout,
            max_block_frames,
            offline,
            state,
            capture_default_state,
        } => serde_json::to_vec(&DiscoveryResponse {
            descriptors: Vec::new(),
            probe: Some(probe_loaded(
                &entry,
                &plugin_id,
                AudioRenderContract {
                    sample_rate,
                    channel_layout,
                    max_block_frames,
                    processing_mode: if offline {
                        AudioProcessingMode::Offline
                    } else {
                        AudioProcessingMode::Realtime
                    },
                    processor_session_scratch_budget_bytes: usize::MAX,
                    public_output_lookahead_budget_frames: usize::MAX,
                    compensation_delay_scratch_budget_bytes: usize::MAX,
                },
                state.as_deref(),
                capture_default_state,
            )?),
        }),
    }
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
    let factory_descriptors = factory.plugin_descriptors();
    if factory_descriptors.len() > MAX_DESCRIPTORS {
        return Err(invalid("CLAP library exceeds descriptor capacity"));
    }
    let mut descriptors = Vec::new();
    for descriptor in factory_descriptors {
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
    render_contract: AudioRenderContract,
    state: Option<&[u8]>,
    capture_default_state: bool,
) -> Result<ProbeResult, AudioProcessorHostError> {
    let max_frames = u32::try_from(render_contract.max_block_frames)
        .ok()
        .filter(|frames| *frames > 0)
        .ok_or_else(|| invalid("CLAP probe block extent is invalid"))?;
    if render_contract.sample_rate == 0 {
        return Err(invalid("CLAP probe sample rate is invalid"));
    }
    let plugin_c_id =
        CString::new(plugin_id).map_err(|_| invalid("CLAP probe plugin ID contains a NUL byte"))?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| unavailable("CLAP library has no plugin factory"))?;
    let descriptors = factory.plugin_descriptors();
    if descriptors.len() > MAX_DESCRIPTORS {
        return Err(invalid("CLAP library exceeds descriptor capacity"));
    }
    if descriptors
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
    let native_shared = shared.clone().with_native_logs()?;
    let mut instance = PluginInstance::<ClapHost>::new(
        |_| native_shared,
        |shared| super::clap_main_services::ClapHostMainThread::new(shared),
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
    configure_render_mode(&instance.plugin_handle(), render_contract.processing_mode)?;
    shared.service_callbacks(&mut instance)?;
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
    let auxiliary_inputs = inspect_audio_inputs(&handle, render_contract.channel_layout)?.contract;
    let configuration = PluginAudioConfiguration {
        sample_rate: f64::from(render_contract.sample_rate),
        min_frames_count: 1,
        max_frames_count: max_frames,
    };
    let mut processor: PluginAudioProcessor<ClapHost> = instance
        .activate(|_, _| (), configuration)
        .map_err(|error| unavailable(format!("CLAP probe activation failed: {error}")))?
        .into();
    instance.access_shared_handler(|host| {
        host.services.active.store(true, std::sync::atomic::Ordering::Release);
    });
    let handle = instance.plugin_handle();
    let latency_frames = handle
        .get_extension::<PluginLatency>()
        .map(|extension| extension.get(&handle))
        .unwrap_or(0);
    let tail = {
        let _audio_role = shared.enter_audio_thread();
        let tail = processor
            .plugin_handle()
            .get_extension::<PluginTail>()
            .map(|extension| extension.get(&processor.plugin_handle()))
            .unwrap_or(TailLength::Finite(0));
        processor.ensure_processing_stopped();
        tail
    };
    drop(processor);
    instance
        .try_deactivate()
        .map_err(|error| unavailable(format!("CLAP probe deactivation failed: {error}")))?;
    instance.access_shared_handler(|host| {
        host.services.active.store(false, std::sync::atomic::Ordering::Release);
    });
    shared.service_callbacks(&mut instance)?;
    let tail = match tail {
        TailLength::Finite(0) => ProbeTail::None,
        TailLength::Finite(frames) => ProbeTail::Finite(frames),
        TailLength::Infinite => ProbeTail::Infinite,
    };
    Ok(ProbeResult {
        plugin_id: plugin_id.to_owned(),
        offline: render_contract.processing_mode == AudioProcessingMode::Offline,
        latency_frames,
        tail,
        parameters,
        captured_state,
        auxiliary_buses: auxiliary_to_wire(&auxiliary_inputs),
    })
}

#[derive(Default)]
struct BoundedStateWriter {
    bytes: Vec<u8>,
    exceeded: bool,
}

/// Read deactivated native ports in ABI order, using stable IDs for author routes.
pub(super) fn inspect_audio_inputs(
    handle: &clack_host::prelude::PluginMainThreadHandle<'_>,
    channel_layout: AudioChannelLayout,
) -> Result<super::NativeAudioInputMetadata, AudioProcessorHostError> {
    let expected_type = match channel_layout {
        AudioChannelLayout::Mono => AudioPortType::MONO,
        AudioChannelLayout::Stereo => AudioPortType::STEREO,
        _ => return Err(invalid("CLAP audio port layout is unsupported")),
    };
    let ports = handle
        .get_extension::<PluginAudioPorts>()
        .ok_or_else(|| invalid("CLAP plugin does not expose audio ports"))?;
    let mut auxiliary_inputs = AudioProcessorAuxiliaryInputContract::default();
    let mut names = Vec::new();
    for is_input in [true, false] {
        let count = ports.count(handle, is_input);
        if count == 0 || (is_input && count > MAX_INPUT_PORTS) || (!is_input && count != 1) {
            return Err(invalid(
                "CLAP requires bounded input ports and one main output",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for index in 0..count {
            let mut buffer = AudioPortInfoBuffer::new();
            let port = ports
                .get(handle, index, is_input, &mut buffer)
                .ok_or_else(|| invalid("CLAP audio port metadata is unavailable"))?;
            if !ids.insert(port.id.get())
                || port.flags.contains(AudioPortFlags::IS_MAIN) != (index == 0)
                || port.port_type != Some(expected_type)
                || usize::try_from(port.channel_count).ok() != Some(channel_layout.channel_count())
            {
                return Err(invalid(
                    "CLAP audio port identity, role, or layout is invalid",
                ));
            }
            if is_input && index != 0 {
                if let Ok(name) = std::str::from_utf8(port.name)
                    && let Some(name) = crate::AudioProcessorInputName::capture(
                        format!("clap.audio-input.{}", port.id.get()),
                        name,
                    )
                {
                    names.push(name);
                }
                auxiliary_inputs.buses.push(AudioProcessorAuxiliaryInputBusContract {
                    bus_key: format!("clap.audio-input.{}", port.id.get()),
                    channel_layout,
                });
            }
        }
    }
    auxiliary_inputs.validate(channel_layout)?;
    Ok(super::NativeAudioInputMetadata { contract: auxiliary_inputs, names })
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
    for (index, parameter) in parameters.iter().enumerate() {
        // Keep the original predicate and short-circuit order. Only a refusal
        // formats diagnostics; the descriptor is the already acquired snapshot.
        let reason = if ClapId::from_raw(parameter.id).is_none() {
            Some("reserved_id")
        } else if !seen.insert(parameter.id) {
            Some("duplicate_id")
        } else if parameter.name.is_empty() {
            Some("empty_name")
        } else if parameter.name.len() > 256 {
            Some("name_bytes_exceed_256")
        } else if parameter.name.chars().any(char::is_control) {
            Some("name_contains_control")
        } else if !parameter.min_value.is_finite() {
            Some("min_nonfinite")
        } else if !parameter.max_value.is_finite() {
            Some("max_nonfinite")
        } else if !parameter.default_value.is_finite() {
            Some("default_nonfinite")
        } else if !parameter.current_value.is_finite() {
            Some("current_nonfinite")
        } else if parameter.min_value > parameter.max_value {
            Some("reversed_range")
        } else if !(parameter.min_value..=parameter.max_value).contains(&parameter.default_value) {
            Some("default_out_of_range")
        } else if !(parameter.min_value..=parameter.max_value).contains(&parameter.current_value) {
            Some("current_out_of_range")
        } else if parameter.enumeration && !parameter.stepped {
            Some("enum_not_stepped")
        } else if parameter.stepped {
            [
                parameter.min_value,
                parameter.max_value,
                parameter.default_value,
                parameter.current_value,
            ]
            .into_iter()
            .enumerate()
            .find_map(|(field, value)| {
                // Preserve field order and fractional-before-precision checks.
                if value.fract() != 0.0 {
                    Some(
                        [
                            "stepped_min_fractional",
                            "stepped_max_fractional",
                            "stepped_default_fractional",
                            "stepped_current_fractional",
                        ][field],
                    )
                } else if value.abs() > 9_007_199_254_740_992.0 {
                    Some(
                        [
                            "stepped_min_precision_limit",
                            "stepped_max_precision_limit",
                            "stepped_default_precision_limit",
                            "stepped_current_precision_limit",
                        ][field],
                    )
                } else {
                    None
                }
            })
        } else {
            None
        };
        if let Some(reason) = reason {
            // ASCII only: never echo a plugin-provided name, reread a native
            // field, or normalize float values to construct this error.
            return Err(invalid(format!(
                "CLAP parameter metadata is invalid or ambiguous: index={index} native_id={} reason={reason} min_bits={:016X} max_bits={:016X} default_bits={:016X} current_bits={:016X}",
                parameter.id,
                parameter.min_value.to_bits(),
                parameter.max_value.to_bits(),
                parameter.default_value.to_bits(),
                parameter.current_value.to_bits(),
            )));
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

pub(super) fn invalid(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.into())
}

pub(super) fn unavailable(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::Unavailable(detail.into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn note_metadata_legacy_list_request_and_response_bytes_are_unchanged() {
        assert_eq!(
            serde_json::to_vec(&super::DiscoveryOperation::List).unwrap(),
            br#"{"kind":"list"}"#
        );
        let response = super::DiscoveryResponse { descriptors: Vec::new(), probe: None };
        assert_eq!(
            serde_json::to_vec(&response).unwrap(),
            br#"{"descriptors":[],"probe":null}"#
        );
    }

    #[test]
    fn note_metadata_legacy_probe_seven_fields_and_additive_operation_are_separate() {
        let response = super::ProbeResult {
            plugin_id: "fixture".to_owned(),
            latency_frames: 0,
            offline: false,
            tail: super::ProbeTail::None,
            parameters: Vec::new(),
            auxiliary_buses: Vec::new(),
            captured_state: None,
        };
        assert_eq!(serde_json::to_vec(&response).unwrap(), br#"{"plugin_id":"fixture","latency_frames":0,"offline":false,"tail":"None","parameters":[],"auxiliary_buses":[],"captured_state":null}"#);
        let query = super::DiscoveryOperation::NoteInputMetadata(
            super::super::clap_note_input_metadata::NoteInputMetadataRequest {
                schema_version: 1,
                plugin_id: "fixture".to_owned(),
                state: None,
            },
        );
        assert_eq!(serde_json::to_vec(&query).unwrap(), br#"{"kind":"note_input_metadata","schema_version":1,"plugin_id":"fixture","state":null}"#);
        assert!(serde_json::from_str::<super::DiscoveryOperation>(
            r#"{"kind":"note_input_metadata","plugin_id":"fixture","state":null}"#
        )
        .is_err());
        assert!(serde_json::from_str::<super::DiscoveryOperation>(r#"{"kind":"note_input_metadata","schema_version":1,"plugin_id":"fixture","state":null,"extra":true}"#).is_err());
        assert!(serde_json::from_str::<super::DiscoveryOperation>(r#"{"kind":"note_input_metadata","schema_version":1,"plugin_id":"fixture","state":null}"#).is_ok());
    }

    use super::*;
    use clack_plugin::prelude::{
        DefaultPluginFactory, HostMainThreadHandle, HostSharedHandle, Plugin, PluginDescriptor,
        PluginError, SinglePluginEntry,
    };

    #[derive(Clone, Copy)]
    enum CountingFactoryMode {
        Null,
        Duplicate,
        LastOnly,
        Unique,
    }

    #[derive(Default)]
    struct FactoryObservations {
        counts: std::sync::atomic::AtomicUsize,
        gets: std::sync::atomic::AtomicUsize,
        creates: std::sync::atomic::AtomicUsize,
        wrong_order: std::sync::atomic::AtomicBool,
    }

    struct CountingFactory {
        advertised: u32,
        mode: CountingFactoryMode,
        descriptors: Vec<PluginDescriptor>,
        observations: std::sync::Arc<FactoryObservations>,
    }

    impl clack_plugin::factory::plugin::PluginFactoryImpl for CountingFactory {
        fn plugin_count(&self) -> u32 {
            self.observations.counts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.advertised
        }

        fn plugin_descriptor(&self, index: u32) -> Option<&PluginDescriptor> {
            use std::sync::atomic::Ordering::Relaxed;
            let previous = self.observations.gets.fetch_add(1, Relaxed);
            if previous != index as usize || self.observations.counts.load(Relaxed) != 1 {
                self.observations.wrong_order.store(true, Relaxed);
            }
            match self.mode {
                CountingFactoryMode::Null => None,
                CountingFactoryMode::Duplicate => self.descriptors.first(),
                CountingFactoryMode::LastOnly => {
                    if index.checked_add(1) == Some(self.advertised) {
                        self.descriptors.first()
                    } else {
                        None
                    }
                }
                CountingFactoryMode::Unique => self.descriptors.get(index as usize),
            }
        }

        fn create_plugin<'a>(
            &'a self,
            _host: clack_plugin::host::HostInfo<'a>,
            _id: &std::ffi::CStr,
        ) -> Option<clack_plugin::plugin::PluginInstance<'a>> {
            self.observations.creates.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Instance creation is deliberately refused: this fixture qualifies
            // descriptor admission and order, never audio or note processing.
            None
        }
    }

    // Entry::new has only a path argument. Hand off this test's owned factory
    // once, on its test thread; callbacks retain owned atomic state and never
    // borrow this setup slot. No global native pointers or production registry.
    thread_local! {
        static NEXT_COUNTING_FACTORY: std::cell::RefCell<Option<CountingFactory>> =
            const { std::cell::RefCell::new(None) };
    }

    struct CountingEntry(clack_plugin::factory::plugin::PluginFactoryWrapper<CountingFactory>);

    impl clack_plugin::entry::Entry for CountingEntry {
        fn new(
            _path: Option<&std::ffi::CStr>,
        ) -> Result<Self, clack_plugin::entry::EntryLoadError> {
            let factory = NEXT_COUNTING_FACTORY
                .with(|slot| slot.borrow_mut().take())
                .ok_or(clack_plugin::entry::EntryLoadError)?;
            Ok(Self(
                clack_plugin::factory::plugin::PluginFactoryWrapper::new(factory),
            ))
        }

        fn declare_factories<'a>(&'a self, builder: &mut clack_plugin::entry::EntryFactories<'a>) {
            builder.register_factory(&self.0);
        }
    }

    fn counting_entry(
        advertised: u32,
        mode: CountingFactoryMode,
    ) -> (PluginEntry, std::sync::Arc<FactoryObservations>) {
        let observations = std::sync::Arc::new(FactoryObservations::default());
        let descriptors = if matches!(mode, CountingFactoryMode::Unique) {
            assert!(
                advertised <= 4096,
                "owned fixture allocation must remain bounded"
            );
            (0..advertised)
                .map(|index| PluginDescriptor::new(&format!("fixture.{index}"), "Fixture"))
                .collect()
        } else {
            vec![PluginDescriptor::new("fixture", "Fixture")]
        };
        NEXT_COUNTING_FACTORY.with(|slot| {
            assert!(slot.borrow().is_none(), "previous setup was not consumed");
            *slot.borrow_mut() = Some(CountingFactory {
                advertised,
                mode,
                descriptors,
                observations: observations.clone(),
            });
        });
        // Existing safe Clack entry bridge supplies the real CLAP ABI callbacks.
        // Entry initialization consumes the setup before any descriptor query.
        let loaded = PluginEntry::load_from_clack::<CountingEntry>(c"/test/counting-factory");
        assert!(NEXT_COUNTING_FACTORY.with(|slot| slot.borrow().is_none()));
        (loaded.expect("owned counting entry"), observations)
    }

    fn assert_factory_observations(
        observations: &FactoryObservations,
        gets: usize,
        creates: usize,
    ) {
        use std::sync::atomic::Ordering::Relaxed;
        assert_eq!(observations.counts.load(Relaxed), 1);
        assert_eq!(observations.gets.load(Relaxed), gets);
        assert_eq!(observations.creates.load(Relaxed), creates);
        assert!(!observations.wrong_order.load(Relaxed));
    }

    fn count_probe(entry: &PluginEntry, note: bool) -> Result<(), AudioProcessorHostError> {
        if note {
            super::super::clap_note_input_metadata::inspect_loaded(entry, "fixture", [7; 32], None)
                .map(|_| ())
        } else {
            probe_loaded(
                entry,
                "fixture",
                AudioRenderContract {
                    sample_rate: 48_000,
                    channel_layout: AudioChannelLayout::Stereo,
                    max_block_frames: 512,
                    processing_mode: AudioProcessingMode::Offline,
                    processor_session_scratch_budget_bytes: usize::MAX,
                    public_output_lookahead_budget_frames: usize::MAX,
                    compensation_delay_scratch_budget_bytes: usize::MAX,
                },
                None,
                false,
            )
            .map(|_| ())
        }
    }

    #[test]
    fn clap_factory_count_admission_rejects_oversize_before_get_in_all_queries() {
        assert_eq!(MAX_DESCRIPTORS, 4096);
        for advertised in [4097, u32::MAX] {
            for query in 0..3 {
                // Matches immediately repeat: historical uncapped selectors
                // stop after2 and List after4097, never hang this regression.
                let (entry, observations) =
                    counting_entry(advertised, CountingFactoryMode::Duplicate);
                let result = if query == 0 {
                    list_loaded(&entry).map(|_| ())
                } else {
                    count_probe(&entry, query == 2)
                };
                assert!(matches!(
                    result,
                    Err(AudioProcessorHostError::InvalidContract(_))
                ));
                assert_factory_observations(&observations, 0, 0);
            }
        }
    }

    #[test]
    fn clap_factory_list_admitted_boundary_empty_null_and_duplicate_preserve_semantics() {
        let (entry, observations) = counting_entry(4096, CountingFactoryMode::Unique);
        let descriptors = list_loaded(&entry).expect("exact admitted descriptor capacity");
        assert_eq!(descriptors.len(), 4096);
        for (index, descriptor) in descriptors.iter().enumerate() {
            assert_eq!(descriptor.plugin_id, format!("fixture.{index}"));
            assert_eq!(descriptor.name, "Fixture");
            assert_eq!(descriptor.vendor, None);
            assert_eq!(descriptor.version, None);
        }
        assert_factory_observations(&observations, 4096, 0);
        for advertised in [0, 4096] {
            let (entry, observations) = counting_entry(advertised, CountingFactoryMode::Null);
            assert!(list_loaded(&entry).unwrap().is_empty());
            assert_factory_observations(&observations, advertised as usize, 0);
        }
        let (entry, observations) = counting_entry(16, CountingFactoryMode::Duplicate);
        assert!(matches!(
            list_loaded(&entry),
            Err(AudioProcessorHostError::InvalidContract(_))
        ));
        assert_factory_observations(&observations, 16, 0);
    }

    #[test]
    fn clap_factory_probe_and_note_boundary_missing_and_duplicate_preserve_selection() {
        for note in [false, true] {
            let (entry, observations) = counting_entry(4096, CountingFactoryMode::LastOnly);
            assert!(
                matches!(count_probe(&entry, note), Err(AudioProcessorHostError::Unavailable(message)) if message.contains("instance failed"))
            );
            // Selection must reach create once after the sole match at4095;
            // deliberate factory creation refusal supplies the final error.
            assert_factory_observations(&observations, 4096, 1);
            for (advertised, mode, expected_gets) in [
                (0, CountingFactoryMode::Null, 0),
                (4096, CountingFactoryMode::Null, 4096),
                (16, CountingFactoryMode::Duplicate, 2),
            ] {
                let (entry, observations) = counting_entry(advertised, mode);
                assert!(
                    matches!(count_probe(&entry, note), Err(AudioProcessorHostError::Unavailable(message)) if message.contains("missing or duplicated"))
                );
                assert_factory_observations(&observations, expected_gets, 0);
            }
        }
    }

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
                AudioRenderContract {
                    sample_rate: 48_000,
                    channel_layout: AudioChannelLayout::Stereo,
                    max_block_frames: 512,
                    processing_mode: AudioProcessingMode::Offline,
                    processor_session_scratch_budget_bytes: usize::MAX,
                    public_output_lookahead_budget_frames: usize::MAX,
                    compensation_delay_scratch_budget_bytes: usize::MAX
                },
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

    fn diagnostic_parameter() -> ClapParameterDescriptor {
        ClapParameterDescriptor {
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
        }
    }

    fn diagnostic_detail(parameters: &[ClapParameterDescriptor]) -> String {
        match validate_parameters(parameters) {
            Err(AudioProcessorHostError::InvalidContract(detail)) => detail,
            other => panic!("expected the original typed InvalidContract refusal, got {other:?}"),
        }
    }

    fn assert_parameter_diagnostic(
        parameters: &[ClapParameterDescriptor],
        index: usize,
        reason: &str,
    ) {
        let parameter = &parameters[index];
        let detail = diagnostic_detail(parameters);
        assert_eq!(
            detail,
            format!(
                "CLAP parameter metadata is invalid or ambiguous: index={index} native_id={} reason={reason} min_bits={:016X} max_bits={:016X} default_bits={:016X} current_bits={:016X}",
                parameter.id,
                parameter.min_value.to_bits(),
                parameter.max_value.to_bits(),
                parameter.default_value.to_bits(),
                parameter.current_value.to_bits(),
            ),
            "first-reason mismatch for {reason}",
        );
        assert!(detail.is_ascii());
        assert!(detail.len() < 512, "diagnostic must remain bounded");
    }

    #[test]
    fn parameter_refusal_diagnostic_reports_ordered_reasons() {
        let parameter = diagnostic_parameter();
        let mut cases = Vec::new();
        macro_rules! case {
            ($reason:literal, $($field:ident: $value:expr),+ $(,)?) => {
                cases.push(($reason, ClapParameterDescriptor { $($field: $value,)+ ..parameter.clone() }));
            };
        }
        case!("reserved_id", id: u32::MAX);
        case!("empty_name", name: String::new());
        case!("name_bytes_exceed_256", name: "é".repeat(129));
        case!("name_contains_control", name: "plugin\nlabel".to_owned());
        case!("min_nonfinite", min_value: f64::NEG_INFINITY);
        case!("max_nonfinite", max_value: f64::INFINITY);
        case!("default_nonfinite", default_value: f64::NAN);
        case!("current_nonfinite", current_value: f64::NAN);
        case!("reversed_range", min_value: 2.0);
        case!("default_out_of_range", default_value: -0.25);
        case!("current_out_of_range", current_value: 1.25);
        case!("enum_not_stepped", enumeration: true);
        case!("stepped_min_fractional", stepped: true, min_value: 0.25);
        case!("stepped_max_fractional", stepped: true, max_value: 0.75);
        case!("stepped_default_fractional", stepped: true);
        case!("stepped_current_fractional", stepped: true, default_value: 0.0);
        case!("stepped_min_precision_limit", stepped: true, min_value: -9_007_199_254_740_994.0, default_value: 0.0, current_value: 0.0);
        case!("stepped_max_precision_limit", stepped: true, max_value: 9_007_199_254_740_994.0, default_value: 0.0, current_value: 0.0);
        for (reason, rejected) in cases {
            assert_parameter_diagnostic(&[rejected], 0, reason);
        }
        let mut second = parameter.clone();
        second.current_value = 0.75;
        assert_parameter_diagnostic(&[parameter, second], 1, "duplicate_id");
    }

    #[test]
    fn parameter_refusal_diagnostic_keeps_original_short_circuit_precedence() {
        let parameter = diagnostic_parameter();
        let cases = [
            (
                "reserved_id",
                ClapParameterDescriptor {
                    id: u32::MAX,
                    name: String::new(),
                    min_value: f64::NAN,
                    ..parameter.clone()
                },
            ),
            (
                "empty_name",
                ClapParameterDescriptor {
                    name: String::new(),
                    min_value: f64::NAN,
                    ..parameter.clone()
                },
            ),
            (
                "name_bytes_exceed_256",
                ClapParameterDescriptor {
                    name: "\n".repeat(257),
                    min_value: f64::NAN,
                    ..parameter.clone()
                },
            ),
            (
                "min_nonfinite",
                ClapParameterDescriptor {
                    min_value: f64::NAN,
                    max_value: f64::NAN,
                    default_value: f64::NAN,
                    current_value: f64::NAN,
                    ..parameter.clone()
                },
            ),
            (
                "max_nonfinite",
                ClapParameterDescriptor {
                    max_value: f64::NAN,
                    default_value: f64::NAN,
                    current_value: f64::NAN,
                    ..parameter.clone()
                },
            ),
            (
                "default_nonfinite",
                ClapParameterDescriptor {
                    default_value: f64::NAN,
                    current_value: f64::NAN,
                    ..parameter.clone()
                },
            ),
            (
                "reversed_range",
                ClapParameterDescriptor {
                    min_value: 2.0,
                    default_value: -1.0,
                    current_value: -1.0,
                    ..parameter.clone()
                },
            ),
            (
                "default_out_of_range",
                ClapParameterDescriptor {
                    default_value: -1.0,
                    current_value: 2.0,
                    enumeration: true,
                    ..parameter.clone()
                },
            ),
            (
                "current_out_of_range",
                ClapParameterDescriptor {
                    current_value: 2.0,
                    enumeration: true,
                    ..parameter.clone()
                },
            ),
            (
                "enum_not_stepped",
                ClapParameterDescriptor { enumeration: true, ..parameter.clone() },
            ),
            (
                "stepped_min_fractional",
                ClapParameterDescriptor {
                    stepped: true,
                    min_value: 0.25,
                    max_value: 0.75,
                    ..parameter.clone()
                },
            ),
            (
                "stepped_max_fractional",
                ClapParameterDescriptor {
                    stepped: true,
                    max_value: 0.75,
                    ..parameter.clone()
                },
            ),
            // Default/current precision failures cannot precede their required
            // containing bounds: the original earlier max predicate wins.
            (
                "stepped_max_precision_limit",
                ClapParameterDescriptor {
                    stepped: true,
                    max_value: 9_007_199_254_740_994.0,
                    default_value: 9_007_199_254_740_994.0,
                    current_value: 0.0,
                    ..parameter.clone()
                },
            ),
            (
                "stepped_max_precision_limit",
                ClapParameterDescriptor {
                    stepped: true,
                    max_value: 9_007_199_254_740_994.0,
                    default_value: 0.0,
                    current_value: 9_007_199_254_740_994.0,
                    ..parameter.clone()
                },
            ),
        ];
        for (reason, rejected) in cases {
            assert_parameter_diagnostic(&[rejected], 0, reason);
        }
        let second = ClapParameterDescriptor {
            name: String::new(),
            current_value: f64::NAN,
            ..parameter.clone()
        };
        assert_parameter_diagnostic(&[parameter.clone(), second], 1, "duplicate_id");
        let later = ClapParameterDescriptor { id: 8, min_value: f64::NAN, ..parameter.clone() };
        let first = ClapParameterDescriptor { default_value: 2.0, ..parameter };
        assert_parameter_diagnostic(&[first, later], 0, "default_out_of_range");
    }

    #[test]
    fn parameter_refusal_diagnostic_preserves_admission_and_count_message() {
        let parameter = diagnostic_parameter();
        validate_parameters(&[]).expect("empty parameters remain valid");
        for id in [0, 7, 0x8000_0000, u32::MAX - 1] {
            validate_parameters(&[ClapParameterDescriptor { id, ..parameter.clone() }])
                .expect("all non-reserved unsigned IDs remain admitted");
        }
        for value in [0.0, 1.0] {
            validate_parameters(&[ClapParameterDescriptor {
                default_value: value,
                current_value: value,
                ..parameter.clone()
            }])
            .expect("inclusive range endpoints remain admitted");
        }
        validate_parameters(&[ClapParameterDescriptor {
            min_value: -0.0,
            max_value: 0.0,
            default_value: 0.0,
            current_value: -0.0,
            stepped: true,
            enumeration: true,
            ..parameter.clone()
        }])
        .expect("signed zero and singleton stepped range remain admitted");
        validate_parameters(&[ClapParameterDescriptor {
            min_value: -9_007_199_254_740_992.0,
            max_value: 9_007_199_254_740_992.0,
            default_value: 0.0,
            current_value: 9_007_199_254_740_992.0,
            stepped: true,
            ..parameter.clone()
        }])
        .expect("exact 2^53 stepped boundary remains admitted");
        let admitted = (0..MAX_PARAMETERS)
            .map(|id| ClapParameterDescriptor { id, ..parameter.clone() })
            .collect::<Vec<_>>();
        validate_parameters(&admitted).expect("the existing capacity is inclusive");
        let mut last_rejected = admitted;
        last_rejected[MAX_PARAMETERS as usize - 1].id = u32::MAX;
        assert_parameter_diagnostic(&last_rejected, MAX_PARAMETERS as usize - 1, "reserved_id");
        let oversized = vec![parameter; MAX_PARAMETERS as usize + 1];
        assert_eq!(
            diagnostic_detail(&oversized),
            "CLAP parameter count exceeds the supported bound"
        );
    }

    #[test]
    fn parameter_refusal_diagnostic_preserves_nan_payload_signed_zero_and_unsigned_id() {
        let parameter = diagnostic_parameter();
        let rejected = ClapParameterDescriptor {
            id: 0x8000_0000,
            min_value: -0.0,
            current_value: f64::from_bits(0x7FF8_0000_0000_0042),
            ..parameter.clone()
        };
        let detail = diagnostic_detail(&[rejected]);
        assert_eq!(detail, "CLAP parameter metadata is invalid or ambiguous: index=0 native_id=2147483648 reason=current_nonfinite min_bits=8000000000000000 max_bits=3FF0000000000000 default_bits=3FE0000000000000 current_bits=7FF8000000000042");
        let second = ClapParameterDescriptor {
            min_value: -0.0,
            max_value: 0.0,
            default_value: 0.0,
            current_value: -0.0,
            ..parameter.clone()
        };
        assert_eq!(diagnostic_detail(&[parameter, second]), "CLAP parameter metadata is invalid or ambiguous: index=1 native_id=7 reason=duplicate_id min_bits=8000000000000000 max_bits=0000000000000000 default_bits=0000000000000000 current_bits=8000000000000000");
        let signaling_nan = ClapParameterDescriptor {
            min_value: f64::from_bits(0x7FF0_0000_0000_0042),
            ..diagnostic_parameter()
        };
        assert!(diagnostic_detail(&[signaling_nan]).contains("min_bits=7FF0000000000042"));
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
        assert_eq!(
            schema
                .numeric
                .as_ref()
                .and_then(|contracts| contracts.first())
                .expect("numeric bounds")
                .hard_range
                .max,
            1.0
        );
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

    #[cfg(windows)]
    mod probe_active_rescan_tests {
        use super::*;
        use clack_extensions::audio_ports::{
            AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
            PluginAudioPortsImpl,
        };
        use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
        use clack_extensions::params::{
            HostParams, ParamDisplayWriter, ParamInfoWriter, ParamRescanFlags,
            PluginAudioProcessorParams, PluginMainThreadParams, PluginParams,
        };
        use clack_extensions::render::{PluginRender, PluginRenderImpl, RenderMode};
        use clack_host::extensions::wrapper::HostWrapper;
        use clack_plugin::events::io::{InputEvents, OutputEvents};
        use clack_plugin::prelude::{
            Audio, ClapId, DefaultPluginFactory, Events, HostAudioProcessorHandle,
            HostMainThreadHandle, HostSharedHandle, Plugin,
            PluginAudioConfiguration as PluginConfig, PluginDescriptor, PluginError,
            PluginExtensions, PluginMainThread, PluginShared, Process, ProcessStatus,
            SinglePluginEntry,
        };
        use std::cell::RefCell;
        use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
        use std::sync::Arc;

        const ACTIVE_LATENCY_ALL: u8 = 0;
        const AFTER_DEACTIVATE_ALL: u8 = 1;
        const ACTIVATE_FALSE: u8 = 2;
        const RENDER_ERROR: u8 = 3;

        #[derive(Default)]
        struct Audit {
            activations: AtomicU32,
            deactivations: AtomicU32,
            deactivate_native_active: AtomicBool,
            deactivate_host_active: AtomicBool,
            latency_calls: AtomicU32,
            latency_native_active: AtomicBool,
            latency_host_active: AtomicBool,
            latency_incomplete: AtomicU16,
            latency_controls: AtomicU32,
            post_deactivate_calls: AtomicU32,
            post_deactivate_host_active: AtomicBool,
            post_deactivate_incomplete: AtomicU16,
            post_deactivate_controls: AtomicU32,
            destroy_calls: AtomicU32,
            destroy_native_active: AtomicBool,
            destroy_host_active: AtomicBool,
            destroy_incomplete: AtomicU16,
            destroy_controls: AtomicU32,
        }

        // Same test-thread setup convention as the existing CountingFactory fixture.
        // No global native pointer, production registry, mutex, or wrapper is added.
        thread_local! {
            static NEXT_AUDIT: RefCell<Option<Arc<Audit>>> = const { RefCell::new(None) };
        }

        struct Fixture<const CASE: u8>;
        struct FixtureShared {
            audit: Arc<Audit>,
            native_active: AtomicBool,
        }
        struct FixtureMain<'a, const CASE: u8> {
            host: HostMainThreadHandle<'a>,
            shared: &'a FixtureShared,
        }
        struct FixtureProcessor<'a, const CASE: u8>(&'a FixtureShared);

        impl PluginShared<'_> for FixtureShared {}

        impl<const CASE: u8> Plugin for Fixture<CASE> {
            type Shared<'a> = FixtureShared;
            type MainThread<'a> = FixtureMain<'a, CASE>;
            type AudioProcessor<'a> = FixtureProcessor<'a, CASE>;

            fn declare_extensions(
                builder: &mut PluginExtensions<Self>,
                _shared: Option<&FixtureShared>,
            ) {
                builder
                    .register::<PluginAudioPorts>()
                    .register::<PluginParams>()
                    .register::<PluginLatency>()
                    .register::<PluginRender>();
            }
        }

        impl<const CASE: u8> DefaultPluginFactory for Fixture<CASE> {
            fn get_descriptor() -> PluginDescriptor {
                PluginDescriptor::new(
                    "org.mondrian.test.probe-active-rescan",
                    "Probe Active Rescan Fixture",
                )
            }

            fn new_shared(_host: HostSharedHandle<'_>) -> Result<FixtureShared, PluginError> {
                let audit = NEXT_AUDIT
                    .with(|slot| slot.borrow_mut().take())
                    .ok_or(PluginError::Message("fixture audit was not supplied"))?;
                Ok(FixtureShared { audit, native_active: AtomicBool::new(false) })
            }

            fn new_main_thread<'a>(
                host: HostMainThreadHandle<'a>,
                shared: &'a FixtureShared,
            ) -> Result<FixtureMain<'a, CASE>, PluginError> {
                Ok(FixtureMain { host, shared })
            }
        }

        impl<const CASE: u8> FixtureMain<'_, CASE> {
            fn host_observation(&self) -> (bool, u16, u32) {
                // SAFETY: every fixture instance in this module is created only by
                // probe_loaded with the live, actual ClapHost wrapper. This typed
                // observer is not usable against an arbitrary installed host.
                unsafe {
                    HostWrapper::<ClapHost>::handle_observation(self.host.as_raw(), |wrapper| {
                        let host = wrapper.shared();
                        let source = host.control.parameter_source.snapshot();
                        Ok((
                            host.services.active.load(Ordering::Acquire),
                            source.incomplete,
                            source.pending_controls,
                        ))
                    })
                }
                .expect("owned fixture host observation")
            }

            fn rescan_all(&self) -> (bool, u16, u32) {
                let before_active = self.host_observation().0;
                self.host
                    .get_extension::<HostParams>()
                    .expect("Windows actual ClapHost exposes HostParams")
                    .rescan(&self.host, ParamRescanFlags::ALL);
                let (_, incomplete, controls) = self.host_observation();
                (before_active, incomplete, controls)
            }
        }

        impl<'a, const CASE: u8> PluginMainThread<'a, FixtureShared> for FixtureMain<'a, CASE> {
            fn on_main_thread(&self) {
                // Only the native deactivate callback schedules this request.
                if CASE == AFTER_DEACTIVATE_ALL {
                    let (active, incomplete, controls) = self.rescan_all();
                    self.shared.audit.post_deactivate_calls.fetch_add(1, Ordering::SeqCst);
                    self.shared.audit.post_deactivate_host_active.store(active, Ordering::SeqCst);
                    self.shared
                        .audit
                        .post_deactivate_incomplete
                        .store(incomplete, Ordering::SeqCst);
                    self.shared.audit.post_deactivate_controls.store(controls, Ordering::SeqCst);
                }
            }
        }

        impl<const CASE: u8> PluginAudioPortsImpl for FixtureMain<'_, CASE> {
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

        // Explicitly expose an empty parameter set. No values/defaults or metadata
        // are modified to manufacture the lifecycle observations.
        impl<const CASE: u8> PluginMainThreadParams for FixtureMain<'_, CASE> {
            fn count(&self) -> u32 {
                0
            }
            fn get_info(&self, _index: u32, _writer: &mut ParamInfoWriter) {}
            fn get_value(&self, _id: ClapId) -> Option<f64> {
                None
            }
            fn value_to_text(
                &self,
                _id: ClapId,
                _value: f64,
                _writer: &mut ParamDisplayWriter,
            ) -> core::fmt::Result {
                Err(core::fmt::Error)
            }
            fn text_to_value(&self, _id: ClapId, _text: &std::ffi::CStr) -> Option<f64> {
                None
            }
            fn flush(&self, _input: &InputEvents, _output: &mut OutputEvents) {}
        }

        impl<const CASE: u8> PluginAudioProcessorParams for FixtureProcessor<'_, CASE> {
            fn flush(&mut self, _input: &InputEvents, _output: &mut OutputEvents) {}
        }

        impl<const CASE: u8> PluginRenderImpl for FixtureMain<'_, CASE> {
            fn has_hard_realtime_requirement(&self) -> bool {
                false
            }
            fn set(&self, _mode: RenderMode) -> Result<(), PluginError> {
                if CASE == RENDER_ERROR {
                    Err(PluginError::Message(
                        "owned render refusal before activation",
                    ))
                } else {
                    Ok(())
                }
            }
        }

        impl<const CASE: u8> PluginLatencyImpl for FixtureMain<'_, CASE> {
            fn get(&self) -> u32 {
                self.shared.audit.latency_calls.fetch_add(1, Ordering::SeqCst);
                self.shared.audit.latency_native_active.store(
                    self.shared.native_active.load(Ordering::SeqCst),
                    Ordering::SeqCst,
                );
                let (active, incomplete, controls) = if CASE == ACTIVE_LATENCY_ALL {
                    self.rescan_all()
                } else {
                    self.host_observation()
                };
                self.shared.audit.latency_host_active.store(active, Ordering::SeqCst);
                self.shared.audit.latency_incomplete.store(incomplete, Ordering::SeqCst);
                self.shared.audit.latency_controls.store(controls, Ordering::SeqCst);
                17
            }
        }

        impl<'a, const CASE: u8>
            clack_plugin::plugin::PluginAudioProcessor<'a, FixtureShared, FixtureMain<'a, CASE>>
            for FixtureProcessor<'a, CASE>
        {
            fn activate(
                _host: HostAudioProcessorHandle<'a>,
                _main: &FixtureMain<'a, CASE>,
                shared: &'a FixtureShared,
                _config: PluginConfig,
            ) -> Result<Self, PluginError> {
                shared.audit.activations.fetch_add(1, Ordering::SeqCst);
                if CASE == ACTIVATE_FALSE {
                    // The genuine Clack plugin ABI translates this error into a
                    // false native activate result; probe receives ActivationFailed.
                    return Err(PluginError::Message("owned activation refusal"));
                }
                shared.native_active.store(true, Ordering::SeqCst);
                Ok(Self(shared))
            }

            fn process(
                &mut self,
                _process: Process,
                _audio: Audio,
                _events: Events,
            ) -> Result<ProcessStatus, PluginError> {
                Err(PluginError::Message("probe must not process PCM"))
            }

            fn deactivate(self, main: &FixtureMain<'a, CASE>) {
                self.0.audit.deactivate_native_active.store(
                    self.0.native_active.load(Ordering::SeqCst),
                    Ordering::SeqCst,
                );
                self.0
                    .audit
                    .deactivate_host_active
                    .store(main.host_observation().0, Ordering::SeqCst);
                self.0.audit.deactivations.fetch_add(1, Ordering::SeqCst);
                self.0.native_active.store(false, Ordering::SeqCst);
                if CASE == AFTER_DEACTIVATE_ALL {
                    main.host.shared().request_callback();
                }
            }
        }

        impl<const CASE: u8> Drop for FixtureMain<'_, CASE> {
            fn drop(&mut self) {
                // SDK keeps host handlers alive through native destruction, and
                // drops the plugin main handler before its plugin shared owner.
                let (active, incomplete, controls) = self.rescan_all();
                self.shared.audit.destroy_calls.fetch_add(1, Ordering::SeqCst);
                self.shared.audit.destroy_native_active.store(
                    self.shared.native_active.load(Ordering::SeqCst),
                    Ordering::SeqCst,
                );
                self.shared.audit.destroy_host_active.store(active, Ordering::SeqCst);
                self.shared.audit.destroy_incomplete.store(incomplete, Ordering::SeqCst);
                self.shared.audit.destroy_controls.store(controls, Ordering::SeqCst);
            }
        }

        fn run<const CASE: u8>() -> (Result<ProbeResult, AudioProcessorHostError>, Arc<Audit>) {
            let audit = Arc::new(Audit::default());
            NEXT_AUDIT.with(|slot| {
                assert!(
                    slot.borrow().is_none(),
                    "previous fixture setup was not consumed"
                );
                *slot.borrow_mut() = Some(Arc::clone(&audit));
            });
            let entry = PluginEntry::load_from_clack::<SinglePluginEntry<Fixture<CASE>>>(
                c"/test/probe-active-rescan",
            )
            .expect("owned ABI entry");
            let result = probe_loaded(
                &entry,
                "org.mondrian.test.probe-active-rescan",
                AudioRenderContract {
                    sample_rate: 48_000,
                    channel_layout: AudioChannelLayout::Stereo,
                    max_block_frames: 512,
                    processing_mode: AudioProcessingMode::Offline,
                    processor_session_scratch_budget_bytes: usize::MAX,
                    public_output_lookahead_budget_frames: usize::MAX,
                    compensation_delay_scratch_budget_bytes: usize::MAX,
                },
                None,
                false,
            );
            NEXT_AUDIT.with(|slot| assert!(slot.borrow().is_none(), "fixture setup unused"));
            (result, audit)
        }

        fn active_rescan_all_bit() -> u16 {
            1_u16 << (super::super::super::clap_parameter_source::ClapParameterSourceFailure::ActiveRescanAll as u8)
        }

        #[test]
        fn probe_active_latency_all_is_retained_and_classified_active() {
            let (result, audit) = run::<ACTIVE_LATENCY_ALL>();
            assert_eq!(
                result.expect("ordinary probe still returns its facts").latency_frames,
                17
            );
            assert_eq!(audit.activations.load(Ordering::SeqCst), 1);
            assert_eq!(audit.deactivations.load(Ordering::SeqCst), 1);
            assert!(audit.deactivate_native_active.load(Ordering::SeqCst));
            assert!(audit.deactivate_host_active.load(Ordering::SeqCst));
            assert_eq!(audit.latency_calls.load(Ordering::SeqCst), 1);
            assert!(audit.latency_native_active.load(Ordering::SeqCst));
            assert!(audit.latency_host_active.load(Ordering::SeqCst));
            assert_eq!(
                audit.latency_incomplete.load(Ordering::SeqCst),
                active_rescan_all_bit()
            );
            assert_eq!(audit.latency_controls.load(Ordering::SeqCst), 1);
            assert_eq!(audit.destroy_calls.load(Ordering::SeqCst), 1);
            assert!(!audit.destroy_native_active.load(Ordering::SeqCst));
            assert!(!audit.destroy_host_active.load(Ordering::SeqCst));
            // The original ActiveRescanAll bit is intentionally sticky. A later
            // deactivated callback must not erase it to make this test pass.
            assert_eq!(
                audit.destroy_incomplete.load(Ordering::SeqCst),
                active_rescan_all_bit()
            );
        }

        #[test]
        fn probe_after_successful_deactivate_all_has_no_active_false_positive() {
            let (result, audit) = run::<AFTER_DEACTIVATE_ALL>();
            assert_eq!(result.expect("ordinary probe").latency_frames, 17);
            assert_eq!(audit.deactivations.load(Ordering::SeqCst), 1);
            assert!(audit.deactivate_native_active.load(Ordering::SeqCst));
            assert!(audit.deactivate_host_active.load(Ordering::SeqCst));
            assert!(audit.latency_native_active.load(Ordering::SeqCst));
            assert!(audit.latency_host_active.load(Ordering::SeqCst));
            assert_eq!(audit.latency_incomplete.load(Ordering::SeqCst), 0);
            assert_eq!(audit.latency_controls.load(Ordering::SeqCst), 0);
            assert_eq!(audit.post_deactivate_calls.load(Ordering::SeqCst), 1);
            assert!(!audit.post_deactivate_host_active.load(Ordering::SeqCst));
            assert_eq!(audit.post_deactivate_incomplete.load(Ordering::SeqCst), 0);
            assert_eq!(audit.post_deactivate_controls.load(Ordering::SeqCst), 1);
            assert_eq!(audit.destroy_calls.load(Ordering::SeqCst), 1);
            assert!(!audit.destroy_host_active.load(Ordering::SeqCst));
            assert_eq!(audit.destroy_incomplete.load(Ordering::SeqCst), 0);
            assert_eq!(audit.destroy_controls.load(Ordering::SeqCst), 2);
        }

        #[test]
        fn probe_native_activation_false_does_not_leave_active_on_destroy() {
            let (result, audit) = run::<ACTIVATE_FALSE>();
            assert!(
                matches!(result, Err(AudioProcessorHostError::Unavailable(message))
                if message.contains("CLAP probe activation failed"))
            );
            assert_eq!(audit.activations.load(Ordering::SeqCst), 1);
            assert_eq!(audit.deactivations.load(Ordering::SeqCst), 0);
            assert_eq!(audit.latency_calls.load(Ordering::SeqCst), 0);
            assert_eq!(audit.destroy_calls.load(Ordering::SeqCst), 1);
            assert!(!audit.destroy_native_active.load(Ordering::SeqCst));
            assert!(!audit.destroy_host_active.load(Ordering::SeqCst));
            assert_eq!(audit.destroy_incomplete.load(Ordering::SeqCst), 0);
            assert_eq!(audit.destroy_controls.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn probe_render_error_before_activation_stays_inactive_through_destroy() {
            let (result, audit) = run::<RENDER_ERROR>();
            assert!(
                matches!(result, Err(AudioProcessorHostError::Unavailable(message))
                if message.contains("render mode was declined"))
            );
            assert_eq!(audit.activations.load(Ordering::SeqCst), 0);
            assert_eq!(audit.deactivations.load(Ordering::SeqCst), 0);
            assert_eq!(audit.latency_calls.load(Ordering::SeqCst), 0);
            assert_eq!(audit.destroy_calls.load(Ordering::SeqCst), 1);
            assert!(!audit.destroy_native_active.load(Ordering::SeqCst));
            assert!(!audit.destroy_host_active.load(Ordering::SeqCst));
            assert_eq!(audit.destroy_incomplete.load(Ordering::SeqCst), 0);
            assert_eq!(audit.destroy_controls.load(Ordering::SeqCst), 1);
        }
    }
}
