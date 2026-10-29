//! Bounded out-of-process discovery of installed CLAP descriptors.

#[cfg(test)]
#[path = "clap_note_processing_offer_wire_tests.rs"]
mod note_processing_wire_tests;

use super::clap_worker::{configure_render_mode, ClapHost, ClapHostShared};
use super::protocol::{auxiliary_from_wire, auxiliary_to_wire, AuxiliaryBusWire};
use super::{ClapMainAudioInput, ClapPluginRegistration};
use crate::{
    AudioProcessingMode, AudioProcessorAuxiliaryInputBusContract,
    AudioProcessorAuxiliaryInputContract, AudioProcessorExecutionContract, AudioProcessorHostError,
    AudioProcessorTail, AudioRenderContract,
};
use clack_extensions::audio_ports::{AudioPortFlags, AudioPortType};
#[cfg(all(test, windows))]
use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
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
pub(super) const MAX_INPUT_PORTS: u32 = 32;
pub(super) const MAX_OUTPUT_PORTS: u32 = 32;
const MAX_CLAP_BINARY_BYTES: u64 = 512 * 1024 * 1024;

// Private optional discovery-error transport; never parameter or playback authority.
const ERROR_ENV: &str = "MONDRIAN_INTERNAL_CLAP_DISCOVERY_ERROR";
const MAX_DISCOVERY_ERROR_TEXT_BYTES: usize = 1024;
const MAX_DISCOVERY_ERROR_PACKET_BYTES: u64 = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DiscoveryErrorStage {
    Arguments,
    RequestRead,
    RequestDecode,
    RequestValidation,
    BinaryFingerprint,
    LibraryLoad,
    List,
    NoteInputMetadata,
    NoteInputContract,
    Probe,
    ResponseEncoding,
    ResponseWrite,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryErrorPacket {
    schema_version: u32,
    stage: DiscoveryErrorStage,
    error_debug: String,
    truncated: bool,
}

struct DiscoveryErrorText {
    text: String,
    truncated: bool,
}

impl std::fmt::Write for DiscoveryErrorText {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let available = MAX_DISCOVERY_ERROR_TEXT_BYTES.saturating_sub(self.text.len());
        let mut count = available.min(text.len());
        while !text.is_char_boundary(count) {
            count -= 1;
        }
        self.text.push_str(&text[..count]);
        if count < text.len() {
            self.truncated = true;
            Err(std::fmt::Error)
        } else {
            Ok(())
        }
    }
}

fn discovery_error_packet(
    stage: DiscoveryErrorStage,
    error: &AudioProcessorHostError,
) -> DiscoveryErrorPacket {
    let mut writer = DiscoveryErrorText {
        text: String::with_capacity(MAX_DISCOVERY_ERROR_TEXT_BYTES),
        truncated: false,
    };
    // Rust main's Result termination prints Error: followed by the error's Debug.
    // This records that host-returned error, not arbitrary native stderr output.
    if std::fmt::write(&mut writer, format_args!("Error: {error:?}")).is_err() {
        writer.truncated = true;
    }
    DiscoveryErrorPacket {
        schema_version: 1,
        stage,
        error_debug: writer.text,
        truncated: writer.truncated,
    }
}

fn write_discovery_error(
    path: &Path,
    stage: DiscoveryErrorStage,
    error: &AudioProcessorHostError,
) -> io::Result<()> {
    let packet = discovery_error_packet(stage, error);
    // A 1024-byte text can escape to at most 6144 JSON bytes; fixed metadata fits
    // inside the 8192-byte packet cap. This formatting is outside native callbacks.
    let bytes = serde_json::to_vec(&packet).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_DISCOVERY_ERROR_PACKET_BYTES {
        return Err(io::Error::other(
            "CLAP discovery error packet exceeds its bound",
        ));
    }
    // Parent created this ordinary file before spawning. Never create a new sink.
    let mut file = fs::OpenOptions::new().write(true).open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(
            "CLAP discovery error sink is not an ordinary file",
        ));
    }
    file.set_len(0)?;
    file.write_all(&bytes)
}

fn run_discovery_with_error_report(
    path: Option<&Path>,
    run: impl FnOnce(&mut DiscoveryErrorStage) -> Result<(), AudioProcessorHostError>,
) -> Result<(), AudioProcessorHostError> {
    let mut stage = DiscoveryErrorStage::Arguments;
    let result = run(&mut stage);
    if let (Err(error), Some(path)) = (&result, path) {
        // Capture failure is secondary: preserve the exact original business Err
        // and its existing main/worker exit behavior. Never report on success.
        let _ = write_discovery_error(path, stage, error);
    }
    result
}

fn read_discovery_error(file: &mut fs::File) -> Option<DiscoveryErrorPacket> {
    use std::io::{Seek, SeekFrom};
    // Read the parent's held ordinary descriptor, never re-open the child-writable
    // path. Replacement of that path cannot redirect the parent to a FIFO/pipe.
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_DISCOVERY_ERROR_PACKET_BYTES {
        return None;
    }
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut bytes = Vec::with_capacity(MAX_DISCOVERY_ERROR_PACKET_BYTES as usize + 1);
    file.take(MAX_DISCOVERY_ERROR_PACKET_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_DISCOVERY_ERROR_PACKET_BYTES {
        return None;
    }
    let packet: DiscoveryErrorPacket = serde_json::from_slice(&bytes).ok()?;
    if packet.schema_version != 1
        || packet.error_debug.is_empty()
        || packet.error_debug.len() > MAX_DISCOVERY_ERROR_TEXT_BYTES
    {
        return None;
    }
    Some(packet)
}

fn discovery_failed_status(
    status: std::process::ExitStatus,
    diagnostic: Option<&mut fs::File>,
) -> AudioProcessorHostError {
    let original = format!("CLAP discovery worker failed with status {status}");
    let Some(packet) = diagnostic.and_then(read_discovery_error) else {
        return unavailable(original);
    };
    unavailable(format!(
        "{original}; worker stage={:?}: {}{}",
        packet.stage,
        packet.error_debug,
        if packet.truncated { " [truncated]" } else { "" }
    ))
}

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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DiscoveryOperation {
    List,
    NoteInputMetadata(super::clap_note_input_metadata::NoteInputMetadataRequest),
    ProbeNotes {
        schema_version: u32,
        plugin_id: String,
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        max_block_frames: usize,
        offline: bool,
        state: Option<Vec<u8>>,
    },
    ProbeProcessing {
        schema_version: u32,
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
    probe: Option<NormalProbeResultV4>,
}

// Ordinary processing discovery owns this envelope only. The same-instance
// Note offer retains its original Processing3 wire and independent lifecycle.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NormalProbeResultV4 {
    schema_version: u32,
    processing: ProbeResult,
    input_names: Vec<crate::AudioProcessorInputName>,
}

pub(super) struct ClapProbe {
    pub(super) registration: ClapPluginRegistration,
    pub(super) captured_state: Option<Vec<u8>>,
    pub(super) input_names: Vec<crate::AudioProcessorInputName>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProbeResult {
    schema_version: u32,
    pub(super) main_audio_input: ClapMainAudioInput,
    pub(super) main_audio_output_id: u32,
    pub(super) auxiliary_outputs: Vec<AuxiliaryBusWire>,
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
    let (_, response) = request_discovery(
        helper_executable,
        library_path,
        DiscoveryOperation::List,
        None,
    )?;
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
    probe_clap_plugin_registration_with_names(
        helper_executable,
        library_path,
        plugin_id,
        render_contract,
        state,
        capture_default_state,
        None,
    )
    .map(|probe| (probe.registration, probe.captured_state))
}

pub(super) fn probe_clap_plugin_registration_with_names(
    helper_executable: &Path,
    library_path: &Path,
    plugin_id: &str,
    render_contract: AudioRenderContract,
    state: Option<&[u8]>,
    capture_default_state: bool,
    native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
) -> Result<ClapProbe, AudioProcessorHostError> {
    if plugin_id.is_empty() || plugin_id.len() > 256 || plugin_id.contains('\0') {
        return Err(invalid("CLAP probe plugin ID is invalid"));
    }
    let canonical = fs::canonicalize(library_path)
        .map_err(|error| unavailable(format!("CLAP library is unavailable: {error}")))?;
    let fingerprint = fingerprint_clap_binary(&canonical)?;
    let (library_path, response) = request_discovery(
        helper_executable,
        library_path,
        DiscoveryOperation::ProbeProcessing {
            schema_version: 4,
            plugin_id: plugin_id.to_owned(),
            sample_rate: render_contract.sample_rate,
            channel_layout: render_contract.channel_layout,
            max_block_frames: render_contract.max_block_frames,
            offline: render_contract.processing_mode == AudioProcessingMode::Offline,
            state: state.map(ToOwned::to_owned),
            capture_default_state,
        },
        native_child_settlement,
    )?;
    if fingerprint_clap_binary(&library_path)? != fingerprint {
        return Err(unavailable("CLAP binary changed during contract probing"));
    }
    realize_normal_probe(
        response,
        library_path,
        fingerprint,
        plugin_id,
        render_contract,
        capture_default_state,
    )
}

fn realize_normal_probe(
    response: DiscoveryResponse,
    library_path: PathBuf,
    fingerprint: [u8; 32],
    plugin_id: &str,
    render_contract: AudioRenderContract,
    capture_default_state: bool,
) -> Result<ClapProbe, AudioProcessorHostError> {
    if !response.descriptors.is_empty() {
        return Err(invalid(
            "CLAP probe response unexpectedly contains descriptors",
        ));
    }
    let normal = response.probe.ok_or_else(|| invalid("CLAP probe response is missing"))?;
    if normal.schema_version != 4 {
        return Err(invalid(
            "unsupported CLAP normal processing probe schema version",
        ));
    }
    let NormalProbeResultV4 { processing: probe, input_names, .. } = normal;
    if probe.schema_version != 3
        || probe.plugin_id != plugin_id
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
    crate::AudioProcessorInputName::validate_all(&input_names, &auxiliary_inputs)?;
    probe.main_audio_input.validate(&auxiliary_inputs)?;
    validate_audio_outputs(
        probe.main_audio_output_id,
        &super::protocol::outputs_from_wire(probe.auxiliary_outputs.clone()),
        render_contract.channel_layout,
    )?;
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
    Ok(ClapProbe {
        registration: ClapPluginRegistration {
            plugin_id: plugin_id.to_owned(),
            library_path,
            binary_sha256: fingerprint,
            execution_contract,
            parameters: probe.parameters,
            main_audio_input: probe.main_audio_input,
            main_audio_output_id: probe.main_audio_output_id,
            auxiliary_outputs: super::protocol::outputs_from_wire(probe.auxiliary_outputs),
            auxiliary_inputs,
        },
        captured_state: probe.captured_state,
        input_names,
    })
}

fn request_discovery(
    helper_executable: &Path,
    library_path: &Path,
    operation: DiscoveryOperation,
    native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
) -> Result<(PathBuf, DiscoveryResponse), AudioProcessorHostError> {
    let (library_path, bytes, _process) = request_discovery_bytes(
        helper_executable,
        library_path,
        operation,
        native_child_settlement,
    )?;
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
        None,
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

/// Explicit restored-instance native Note port/storage offer. This operation
/// registers CLAP-only host Notes; the original facts-only metadata never does.
/// The concrete processing instance must independently revalidate this offer.
pub fn probe_clap_note_input_contract(
    helper_executable: &Path,
    library_path: &Path,
    plugin_id: &str,
    render: AudioRenderContract,
    state: Option<&[u8]>,
) -> Result<super::clap_note_input_contract::ClapNoteInputContractProbe, AudioProcessorHostError> {
    super::clap_note_input_metadata::validate_request(plugin_id, state)?;
    if !helper_executable.is_absolute()
        || !library_path.is_absolute()
        || render.sample_rate == 0
        || u32::try_from(render.max_block_frames).ok().is_none_or(|frames| frames == 0)
    {
        return Err(invalid(
            "CLAP Note contract probe paths/render extent are invalid",
        ));
    }
    let canonical = fs::canonicalize(library_path)
        .map_err(|error| unavailable(format!("CLAP library is unavailable: {error}")))?;
    let fingerprint = fingerprint_clap_binary(&canonical)?;
    let (library_path, bytes, _process) = request_discovery_bytes(
        helper_executable,
        &canonical,
        DiscoveryOperation::ProbeNotes {
            schema_version: 2,
            plugin_id: plugin_id.to_owned(),
            sample_rate: render.sample_rate,
            channel_layout: render.channel_layout,
            max_block_frames: render.max_block_frames,
            offline: render.processing_mode == AudioProcessingMode::Offline,
            state: state.map(ToOwned::to_owned),
        },
        None,
    )?;
    if fingerprint_clap_binary(&library_path)? != fingerprint {
        return Err(unavailable(
            "CLAP binary changed during Note contract negotiation",
        ));
    }
    let result: super::clap_note_input_contract::ClapNoteInputContractProbe =
        serde_json::from_slice(&bytes)
            .map_err(|error| invalid(format!("CLAP Note contract response is invalid: {error}")))?;
    if result.schema_version != 2
        || result.plugin_id != plugin_id
        || result.binary_sha256 != fingerprint
        || result.state_sha256 != super::clap_note_input_metadata::state_digest(state)
        || result.sample_rate != render.sample_rate
        || result.channel_layout != render.channel_layout
        || result.max_block_frames != render.max_block_frames
        || result.offline != (render.processing_mode == AudioProcessingMode::Offline)
        || result.contract.ports.is_empty()
        || result.contract.limits != super::clap_note_input_contract::offered_limits()
    {
        return Err(invalid(
            "CLAP Note contract response changed identity/render/finite offer",
        ));
    }
    super::clap_note_input_contract::validate_clap_contract(&result.contract)?;
    for port in &result.contract.ports {
        if port.port_key != format!("clap.note-input.{}", port.native_id) {
            return Err(invalid(
                "CLAP Note contract response changed its stable native key",
            ));
        }
    }
    Ok(result)
}

pub(super) fn processing_result_for_notes(
    plugin_id: &str,
    render: AudioRenderContract,
    parameters: Vec<ClapParameterDescriptor>,
    audio: &ClapAudioPortMetadata,
    latency_frames: u32,
    tail: TailLength,
) -> Result<ProbeResult, AudioProcessorHostError> {
    validate_parameters(&parameters)?;
    audio.contract.validate(render.channel_layout)?;
    audio.main_audio_input.validate(&audio.contract)?;
    validate_audio_outputs(
        audio.main_audio_output_id,
        &audio.outputs,
        render.channel_layout,
    )?;
    Ok(ProbeResult {
        schema_version: 3,
        main_audio_input: audio.main_audio_input,
        main_audio_output_id: audio.main_audio_output_id,
        auxiliary_outputs: super::protocol::outputs_to_wire(&audio.outputs),
        plugin_id: plugin_id.to_owned(),
        offline: render.processing_mode == AudioProcessingMode::Offline,
        latency_frames,
        tail: match tail {
            TailLength::Finite(0) => ProbeTail::None,
            TailLength::Finite(frames) => ProbeTail::Finite(frames),
            TailLength::Infinite => ProbeTail::Infinite,
        },
        parameters,
        auxiliary_buses: auxiliary_to_wire(&audio.contract),
        captured_state: None,
    })
}

fn validate_processing_result_for_notes(
    processing: &ProbeResult,
    plugin_id: &str,
    render: AudioRenderContract,
) -> Result<(), AudioProcessorHostError> {
    if processing.schema_version != 3
        || processing.plugin_id != plugin_id
        || processing.offline != (render.processing_mode == AudioProcessingMode::Offline)
        || processing.captured_state.is_some()
        || processing.auxiliary_buses.len() >= MAX_INPUT_PORTS as usize
    {
        return Err(invalid(
            "CLAP Note processing response changed its contract identity",
        ));
    }
    validate_parameters(&processing.parameters)?;
    let auxiliary_inputs = auxiliary_from_wire(processing.auxiliary_buses.clone());
    auxiliary_inputs.validate(render.channel_layout)?;
    processing.main_audio_input.validate(&auxiliary_inputs)?;
    validate_audio_outputs(
        processing.main_audio_output_id,
        &super::protocol::outputs_from_wire(processing.auxiliary_outputs.clone()),
        render.channel_layout,
    )?;
    let tail = match &processing.tail {
        ProbeTail::None => AudioProcessorTail::None,
        ProbeTail::Finite(frames) => AudioProcessorTail::Finite(*frames as usize),
        ProbeTail::Infinite => AudioProcessorTail::Infinite,
    };
    AudioProcessorExecutionContract::new(
        processing.latency_frames as usize,
        tail,
        true,
        render.processing_mode == AudioProcessingMode::Realtime,
        render.processing_mode == AudioProcessingMode::Offline,
        0,
    )?;
    Ok(())
}

pub(super) fn note_processing_parameters(processing: &ProbeResult) -> &[ClapParameterDescriptor] {
    &processing.parameters
}

pub(super) fn note_processing_auxiliary_inputs(
    processing: &ProbeResult,
) -> impl ExactSizeIterator<Item = (&str, AudioChannelLayout)> + '_ {
    processing
        .auxiliary_buses
        .iter()
        .map(|bus| (bus.bus_key.as_str(), bus.channel_layout))
}

pub(super) fn note_processing_tail(processing: &ProbeResult) -> AudioProcessorTail {
    match &processing.tail {
        ProbeTail::None => AudioProcessorTail::None,
        ProbeTail::Finite(frames) => AudioProcessorTail::Finite(*frames as usize),
        ProbeTail::Infinite => AudioProcessorTail::Infinite,
    }
}

pub(super) fn note_processing_mode(processing: &ProbeResult) -> AudioProcessingMode {
    if processing.offline {
        AudioProcessingMode::Offline
    } else {
        AudioProcessingMode::Realtime
    }
}

pub(super) fn revalidate_processing_metadata_for_notes(
    processing: &ProbeResult,
    parameters: &[ClapParameterDescriptor],
    audio: &ClapAudioPortMetadata,
) -> Result<(), AudioProcessorHostError> {
    if processing.main_audio_input != audio.main_audio_input
        || processing.parameters != parameters
        || auxiliary_from_wire(processing.auxiliary_buses.clone()) != audio.contract
        || processing.main_audio_output_id != audio.main_audio_output_id
        || super::protocol::outputs_from_wire(processing.auxiliary_outputs.clone()) != audio.outputs
    {
        return Err(unavailable(
            "CLAP Note processing metadata changed across activation/deactivation",
        ));
    }
    Ok(())
}

// Only the v6 full-processing branch uses this bounded pure serializer.
// Legacy read-only Note metadata keeps its original wire/paths. Both passes serialize owned DTOs after native destruction.
fn encode_processing_offer_v6(
    value: &super::clap_note_input_contract::ClapNoteProcessingOfferV6,
) -> Result<Vec<u8>, AudioProcessorHostError> {
    struct Count(usize);
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|size| *size as u64 <= MAX_RESPONSE_BYTES)
                .ok_or_else(|| {
                    io::Error::other("CLAP Note processing response exceeds metadata limit")
                })?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, value)
        .map_err(|error| invalid(format!("CLAP Note response measurement failed: {error}")))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(count.0)
        .map_err(|_| invalid("CLAP Note response allocation failed"))?;
    if bytes.capacity() != count.0 {
        return Err(invalid("CLAP Note response allocation exceeded admission"));
    }
    struct Exact<'a> {
        bytes: &'a mut Vec<u8>,
        limit: usize,
    }
    impl Write for Exact<'_> {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            if self.bytes.len().checked_add(input.len()).is_none_or(|size| size > self.limit) {
                return Err(io::Error::other(
                    "CLAP Note response changed during encoding",
                ));
            }
            self.bytes.extend_from_slice(input);
            Ok(input.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Exact { bytes: &mut bytes, limit: count.0 }, value)
        .map_err(|error| invalid(format!("CLAP Note response encoding failed: {error}")))?;
    if bytes.len() != count.0 {
        return Err(invalid("CLAP Note response encoding extent changed"));
    }
    Ok(bytes)
}

/// Query one Notes-enabled instance's complete processing facts in the bounded child.
///
/// Activation and metadata collection do not prove DSP, real-time behavior or
/// installed ownership. Execution is admitted separately by the native resolver.
pub fn probe_clap_note_processing_offer(
    helper_executable: &Path,
    library_path: &Path,
    plugin_id: &str,
    render: AudioRenderContract,
    state: Option<&[u8]>,
) -> Result<super::clap_note_input_contract::ClapNoteProcessingOfferV6, AudioProcessorHostError> {
    probe_clap_note_processing_offer_with_native_child_settlement(
        helper_executable,
        library_path,
        plugin_id,
        render,
        state,
        None,
    )
}

pub(super) fn probe_clap_note_processing_offer_with_native_child_settlement(
    helper_executable: &Path,
    library_path: &Path,
    plugin_id: &str,
    render: AudioRenderContract,
    state: Option<&[u8]>,
    native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
) -> Result<super::clap_note_input_contract::ClapNoteProcessingOfferV6, AudioProcessorHostError> {
    super::clap_note_input_metadata::validate_request(plugin_id, state)?;
    if !helper_executable.is_absolute()
        || !library_path.is_absolute()
        || render.sample_rate == 0
        || u32::try_from(render.max_block_frames).ok().is_none_or(|frames| frames == 0)
    {
        return Err(invalid(
            "CLAP Note processing probe paths/render extent are invalid",
        ));
    }
    let canonical = fs::canonicalize(library_path)
        .map_err(|error| unavailable(format!("CLAP library is unavailable: {error}")))?;
    let fingerprint = fingerprint_clap_binary(&canonical)?;
    let (library_path, bytes, _process) = request_discovery_bytes(
        helper_executable,
        &canonical,
        DiscoveryOperation::ProbeNotes {
            schema_version: 6,
            plugin_id: plugin_id.to_owned(),
            sample_rate: render.sample_rate,
            channel_layout: render.channel_layout,
            max_block_frames: render.max_block_frames,
            offline: render.processing_mode == AudioProcessingMode::Offline,
            state: state.map(ToOwned::to_owned),
        },
        native_child_settlement,
    )?;
    if fingerprint_clap_binary(&library_path)? != fingerprint {
        return Err(unavailable(
            "CLAP binary changed during Note processing negotiation",
        ));
    }
    decode_processing_offer_v6(&bytes, plugin_id, fingerprint, state, render)
}

fn decode_processing_offer_v6(
    bytes: &[u8],
    plugin_id: &str,
    fingerprint: [u8; 32],
    state: Option<&[u8]>,
    render: AudioRenderContract,
) -> Result<super::clap_note_input_contract::ClapNoteProcessingOfferV6, AudioProcessorHostError> {
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(invalid(
            "CLAP Note processing response exceeds metadata limit",
        ));
    }
    let result: super::clap_note_input_contract::ClapNoteProcessingOfferV6 =
        serde_json::from_slice(bytes).map_err(|error| {
            invalid(format!("CLAP Note processing response is invalid: {error}"))
        })?;
    let notes = &result.notes;
    if result.schema_version != 6
        || notes.schema_version != 2
        || notes.plugin_id != plugin_id
        || notes.binary_sha256 != fingerprint
        || notes.state_sha256 != super::clap_note_input_metadata::state_digest(state)
        || notes.sample_rate != render.sample_rate
        || notes.channel_layout != render.channel_layout
        || notes.max_block_frames != render.max_block_frames
        || notes.offline != (render.processing_mode == AudioProcessingMode::Offline)
        || notes.contract.ports.is_empty()
        || notes.contract.limits != super::clap_note_input_contract::offered_limits()
    {
        return Err(invalid(
            "CLAP Note processing response changed requested identity/state/render/offer",
        ));
    }
    super::clap_note_input_contract::validate_clap_contract(&notes.contract)?;
    for port in &notes.contract.ports {
        if port.port_key != format!("clap.note-input.{}", port.native_id) {
            return Err(invalid("CLAP Note processing response changed native key"));
        }
    }
    validate_processing_result_for_notes(&result.processing, plugin_id, render)?;
    crate::AudioProcessorInputName::validate_all(
        &result.input_names,
        &auxiliary_from_wire(result.processing.auxiliary_buses.clone()),
    )?;
    Ok(result)
}

// Concrete response decoders share the existing supervisor and bounds.
fn request_discovery_bytes(
    helper_executable: &Path,
    library_path: &Path,
    operation: DiscoveryOperation,
    native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
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
    let error_path = temp.path().join("error.json");
    let request =
        serde_json::to_vec(&DiscoveryRequest { library_path: library_path.clone(), operation })
            .map_err(|error| invalid(format!("CLAP discovery request failed: {error}")))?;
    if request.len() as u64 > MAX_REQUEST_BYTES {
        return Err(invalid("CLAP discovery request exceeds the metadata limit"));
    }
    fs::write(&request_path, request)
        .map_err(|error| unavailable(format!("CLAP discovery request write failed: {error}")))?;
    // Diagnostics are optional. Retain the create_new ordinary descriptor so the
    // parent never reopens a path that native code in the child could replace.
    let mut diagnostic = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&error_path)
        .ok();
    let mut command = Command::new(helper_executable);
    command
        .arg(CLAP_DISCOVERY_WORKER_ARGUMENT)
        .env(REQUEST_ENV, &request_path)
        .env(RESPONSE_ENV, &response_path)
        .env_remove(ERROR_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if diagnostic.is_some() {
        command.env(ERROR_ENV, &error_path);
    }
    #[cfg(windows)]
    super::supervisor::configure_background_worker_start(&mut command);
    // Adopt the child and staging immediately; every post-spawn failure/unwind
    // uses the same existing OS owner as audio workers, without claiming that
    // permanent OS refusal or a successful-kill wait is kernel-time bounded.
    let mut process = super::supervisor::ProcessOwner::with_staging(
        command.spawn().map_err(|error| {
            unavailable(format!("CLAP discovery worker failed to start: {error}"))
        })?,
        temp,
    );
    if let Some(registrar) = native_child_settlement {
        // Retain observation on the same original Child/staging owner. Rejected
        // evidence must not replace the original discovery result or cleanup.
        let _ = process.bind_native_child_settlement(registrar);
    }
    let outcome = wait_for_discovery_worker(&mut process, diagnostic.as_mut());
    drop(diagnostic);
    outcome?;
    let response = read_bounded(&response_path, "response", MAX_RESPONSE_BYTES)?;
    // The caller retains this owner through decoding (and metadata identity
    // verification), so directory lifetime covers the complete response use.
    Ok((library_path, response, process))
}

// The existing fixed wait policy has one production consumer; owned child tests
// use this same loop, not a second deadline or process-ownership implementation.
fn wait_for_discovery_worker(
    process: &mut super::supervisor::ProcessOwner,
    diagnostic: Option<&mut fs::File>,
) -> Result<(), AudioProcessorHostError> {
    let deadline = Instant::now() + DISCOVERY_DEADLINE;
    loop {
        match process.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(discovery_failed_status(status, diagnostic));
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                return Err(retire_discovery_failure(
                    process,
                    AudioProcessorHostError::WorkerDeadlineExceeded { operation: "CLAP discovery" },
                ));
            }
            Err(error) => {
                return Err(retire_discovery_failure(
                    process,
                    unavailable(format!("CLAP discovery worker wait failed: {error}")),
                ));
            }
        }
    }
}

pub(super) fn retire_discovery_failure(
    process: &mut super::supervisor::ProcessOwner,
    primary: AudioProcessorHostError,
) -> AudioProcessorHostError {
    process.terminate_reported("CLAP discovery failure");
    primary
}

fn validate_discovery_schema(
    operation: &DiscoveryOperation,
) -> Result<(), AudioProcessorHostError> {
    match operation {
        DiscoveryOperation::ProbeProcessing { schema_version, .. } if *schema_version != 4 => {
            Err(invalid("unsupported CLAP processing probe schema version"))
        }
        DiscoveryOperation::ProbeNotes { schema_version, .. }
            if !matches!(*schema_version, 2 | 6) =>
        {
            Err(invalid("unsupported CLAP Note probe schema version"))
        }
        _ => Ok(()),
    }
}

/// Execute the hidden discovery mode before any UI, GPU, or media initialization.
pub fn run_clap_discovery_worker() -> Result<(), AudioProcessorHostError> {
    let error_path = std::env::var_os(ERROR_ENV).map(PathBuf::from);
    run_discovery_with_error_report(error_path.as_deref(), run_clap_discovery_worker_inner)
}

fn run_clap_discovery_worker_inner(
    stage: &mut DiscoveryErrorStage,
) -> Result<(), AudioProcessorHostError> {
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
    *stage = DiscoveryErrorStage::RequestRead;
    let request = read_bounded(&request_path, "request", MAX_REQUEST_BYTES)?;
    *stage = DiscoveryErrorStage::RequestDecode;
    let request: DiscoveryRequest = serde_json::from_slice(&request)
        .map_err(|error| invalid(format!("CLAP discovery request is invalid: {error}")))?;
    *stage = DiscoveryErrorStage::RequestValidation;
    if !request.library_path.is_absolute() {
        return Err(invalid("CLAP discovery library path must be absolute"));
    }
    validate_discovery_schema(&request.operation)?;
    if let DiscoveryOperation::NoteInputMetadata(query) = &request.operation {
        if query.schema_version != super::clap_note_input_metadata::SCHEMA_VERSION {
            return Err(invalid("unsupported CLAP note metadata schema version"));
        }
        super::clap_note_input_metadata::validate_request(
            &query.plugin_id,
            query.state.as_deref(),
        )?;
    }
    if let DiscoveryOperation::ProbeNotes {
        plugin_id, state, sample_rate, max_block_frames, ..
    } = &request.operation
    {
        super::clap_note_input_metadata::validate_request(plugin_id, state.as_deref())?;
        if *sample_rate == 0
            || u32::try_from(*max_block_frames).ok().is_none_or(|frames| frames == 0)
        {
            return Err(invalid("CLAP Note probe render extent is invalid"));
        }
    }
    let metadata_fingerprint = if matches!(
        &request.operation,
        DiscoveryOperation::NoteInputMetadata(_) | DiscoveryOperation::ProbeNotes { .. }
    ) {
        *stage = DiscoveryErrorStage::BinaryFingerprint;
        Some(fingerprint_clap_binary(&request.library_path)?)
    } else {
        None
    };

    *stage = DiscoveryErrorStage::LibraryLoad;
    let entry = unsafe { PluginEntry::load(request.library_path.as_os_str()) }
        .map_err(|error| unavailable(format!("CLAP library load failed: {error}")))?;
    let response = match request.operation {
        DiscoveryOperation::List => {
            *stage = DiscoveryErrorStage::List;
            let descriptors = list_loaded(&entry)?;
            *stage = DiscoveryErrorStage::ResponseEncoding;
            serde_json::to_vec(&DiscoveryResponse { descriptors, probe: None })
        }
        DiscoveryOperation::NoteInputMetadata(query) => {
            *stage = DiscoveryErrorStage::NoteInputMetadata;
            let fingerprint = metadata_fingerprint
                .ok_or_else(|| invalid("CLAP note metadata fingerprint is missing"))?;
            let metadata = super::clap_note_input_metadata::inspect_loaded(
                &entry,
                &query.plugin_id,
                fingerprint,
                query.state.as_deref(),
            )?;
            *stage = DiscoveryErrorStage::BinaryFingerprint;
            if fingerprint_clap_binary(&request.library_path)? != fingerprint {
                return Err(unavailable(
                    "CLAP binary changed during native note metadata inspection",
                ));
            }
            *stage = DiscoveryErrorStage::ResponseEncoding;
            serde_json::to_vec(&metadata)
        }
        DiscoveryOperation::ProbeProcessing {
            schema_version: _,
            plugin_id,
            sample_rate,
            channel_layout,
            max_block_frames,
            offline,
            state,
            capture_default_state,
        } => {
            *stage = DiscoveryErrorStage::Probe;
            let probe = probe_loaded_with_names(
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
            )?;
            *stage = DiscoveryErrorStage::ResponseEncoding;
            serde_json::to_vec(&DiscoveryResponse { descriptors: Vec::new(), probe: Some(probe) })
        }
        DiscoveryOperation::ProbeNotes {
            schema_version,
            plugin_id,
            sample_rate,
            channel_layout,
            max_block_frames,
            offline,
            state,
        } => {
            *stage = DiscoveryErrorStage::NoteInputContract;
            let fingerprint = metadata_fingerprint
                .ok_or_else(|| invalid("CLAP Note probe fingerprint is missing"))?;
            let render = AudioRenderContract {
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
            };
            if schema_version == 2 {
                // Standalone explicit Note contract, separate from full processing.
                let result = super::clap_note_input_contract::probe_loaded(
                    &entry,
                    &plugin_id,
                    fingerprint,
                    state.as_deref(),
                    render,
                )?;
                *stage = DiscoveryErrorStage::BinaryFingerprint;
                if fingerprint_clap_binary(&request.library_path)? != fingerprint {
                    return Err(unavailable(
                        "CLAP binary changed during native Note contract negotiation",
                    ));
                }
                *stage = DiscoveryErrorStage::ResponseEncoding;
                serde_json::to_vec(&result)
            } else {
                // The complete offer is captured from one actual Notes-enabled
                // restored/render-configured/activated instance, then destroyed.
                let result = super::clap_note_input_contract::probe_loaded_processing_v6(
                    &entry,
                    &plugin_id,
                    fingerprint,
                    state.as_deref(),
                    render,
                )?;
                *stage = DiscoveryErrorStage::BinaryFingerprint;
                if fingerprint_clap_binary(&request.library_path)? != fingerprint {
                    return Err(unavailable(
                        "CLAP binary changed during native Note processing negotiation",
                    ));
                }
                *stage = DiscoveryErrorStage::ResponseEncoding;
                Ok(encode_processing_offer_v6(&result)?)
            }
        }
    }
    .map_err(|error| invalid(format!("CLAP discovery response encoding failed: {error}")))?;
    if response.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(invalid(
            "CLAP discovery response exceeds the metadata limit",
        ));
    }
    *stage = DiscoveryErrorStage::ResponseWrite;
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

// Existing ABI controls observe the unchanged Processing3 projection of the
// ordinary probe, through the same implementation used by the real child.
#[cfg(test)]
pub(super) fn probe_loaded(
    entry: &PluginEntry,
    plugin_id: &str,
    render_contract: AudioRenderContract,
    state: Option<&[u8]>,
    capture_default_state: bool,
) -> Result<ProbeResult, AudioProcessorHostError> {
    probe_loaded_with_names(
        entry,
        plugin_id,
        render_contract,
        state,
        capture_default_state,
    )
    .map(|normal| normal.processing)
}

fn probe_loaded_with_names(
    entry: &PluginEntry,
    plugin_id: &str,
    render_contract: AudioRenderContract,
    state: Option<&[u8]>,
    capture_default_state: bool,
) -> Result<NormalProbeResultV4, AudioProcessorHostError> {
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
    let inactive = instance
        .inactive_plugin_handle()
        .ok_or_else(|| invalid("CLAP cold probe is active"))?;
    if super::clap_audio_config::negotiate_initial(&inactive, render_contract.channel_layout)? {
        shared.service_callbacks(&mut instance)?;
    }
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
    let parameters = {
        let mut capture = ParameterQueryCapture::from_environment();
        list_parameters_observed(&handle, capture.as_mut())?
    };
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
    let audio = inspect_audio_ports(&handle, render_contract.channel_layout)?;
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
    let current_audio =
        inspect_audio_ports(&instance.plugin_handle(), render_contract.channel_layout)?;
    if current_audio.main_audio_input != audio.main_audio_input
        || current_audio.contract != audio.contract
        || current_audio.main_audio_output_id != audio.main_audio_output_id
        || current_audio.outputs != audio.outputs
    {
        return Err(unavailable(
            "CLAP audio input shape changed across activation/deactivation",
        ));
    }
    Ok(NormalProbeResultV4 {
        schema_version: 4,
        processing: ProbeResult {
            schema_version: 3,
            main_audio_input: audio.main_audio_input,
            main_audio_output_id: audio.main_audio_output_id,
            auxiliary_outputs: super::protocol::outputs_to_wire(&audio.outputs),
            plugin_id: plugin_id.to_owned(),
            offline: render_contract.processing_mode == AudioProcessingMode::Offline,
            latency_frames,
            tail,
            parameters,
            captured_state,
            auxiliary_buses: auxiliary_to_wire(&audio.contract),
        },
        // This is the restored/render-configured inactive snapshot consumed by
        // the real Worker before activation, not the later deactivation scan.
        input_names: audio.names,
    })
}

#[derive(Default)]
struct BoundedStateWriter {
    bytes: Vec<u8>,
    exceeded: bool,
}

pub(super) struct ClapAudioPortMetadata {
    pub(super) main_audio_input: ClapMainAudioInput,
    pub(super) contract: AudioProcessorAuxiliaryInputContract,
    pub(super) names: Vec<crate::AudioProcessorInputName>,
    pub(super) main_audio_output_id: u32,
    pub(super) outputs: crate::AudioProcessorAuxiliaryOutputContract,
}

// Only the cold refusal path formats owned facts from this same successful get.
// Clack has already removed unknown flag bits and normalized NULL/empty types.
pub(super) fn audio_port_refusal(
    reason: &'static str,
    is_input: bool,
    index: u32,
    count: u32,
    port: &clack_extensions::audio_ports::AudioPortInfo<'_>,
    expected_channels: usize,
) -> AudioProcessorHostError {
    let direction = if is_input { "input" } else { "output" };
    let port_type = match port.port_type {
        None => "unspecified",
        Some(value) if value == AudioPortType::MONO => "mono",
        Some(value) if value == AudioPortType::STEREO => "stereo",
        Some(_) => "other",
    };
    let expected_type = if expected_channels == 1 {
        "mono"
    } else {
        "stereo"
    };
    invalid(format!(
        "CLAP audio port identity, role, or layout is invalid; reason={reason} direction={direction} index={index} count={count} id={} known_flags_bits={:08X} main={} channels={} type={port_type} expected_channels={expected_channels} expected_type={expected_type}",
        port.id.get(), port.flags.bits(), port.flags.contains(AudioPortFlags::IS_MAIN), port.channel_count,
    ))
}

/// Read native ports in ABI order, using stable IDs for author routes.
/// The plugin must be deactivated: CLAP forbids audio-port scans while active.
pub(super) fn validate_audio_outputs(
    main_id: u32,
    outputs: &crate::AudioProcessorAuxiliaryOutputContract,
    layout: AudioChannelLayout,
) -> Result<(), AudioProcessorHostError> {
    outputs.validate(layout)?;
    if main_id == u32::MAX || outputs.buses.len() >= MAX_OUTPUT_PORTS as usize {
        return Err(invalid("CLAP output identity or port count is invalid"));
    }
    let mut ids = std::collections::BTreeSet::from([main_id]);
    for bus in &outputs.buses {
        let id = bus
            .bus_key
            .strip_prefix("clap.audio-output.")
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|id| *id != u32::MAX && bus.bus_key == format!("clap.audio-output.{id}"))
            .ok_or_else(|| invalid("CLAP output key is not a canonical native port identity"))?;
        if !ids.insert(id) {
            return Err(invalid(
                "CLAP output native IDs must be unique within their direction",
            ));
        }
    }
    Ok(())
}

pub(super) fn inspect_audio_ports(
    handle: &clack_host::prelude::PluginMainThreadHandle<'_>,
    channel_layout: AudioChannelLayout,
) -> Result<ClapAudioPortMetadata, AudioProcessorHostError> {
    let mut auxiliary_inputs = AudioProcessorAuxiliaryInputContract::default();
    let mut names = Vec::new();
    let mut outputs = crate::AudioProcessorAuxiliaryOutputContract::default();
    let mut scratch = super::clap_metadata::MetadataScanScratch::new();
    let (main_audio_input, main_audio_output_id, _, _) = super::clap_metadata::scan_audio_ports(
        handle,
        channel_layout,
        &mut scratch,
        |is_input, index, port, port_layout| {
            if !is_input && index != 0 {
                outputs.buses.push(crate::AudioProcessorAuxiliaryOutputBusContract {
                    bus_key: format!("clap.audio-output.{}", port.id.get()),
                    channel_layout,
                });
            } else if is_input && !port.flags.contains(AudioPortFlags::IS_MAIN) {
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
                    channel_layout: port_layout,
                });
            }
        },
    )?;
    auxiliary_inputs.validate(channel_layout)?;
    main_audio_input.validate(&auxiliary_inputs)?;
    Ok(ClapAudioPortMetadata {
        main_audio_input,
        contract: auxiliary_inputs,
        names,
        main_audio_output_id,
        outputs,
    })
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

// Private startup diagnostics only. None remains the default query path.
const PARAMETER_SAME_CALL_ENV: &str = "MONDRIAN_INTERNAL_CLAP_PARAMETER_SAME_CALL";
const MAX_PARAMETER_OBSERVATION_SUFFIX_BYTES: usize = 640;

#[derive(Clone, Copy)]
struct ParameterQueryRecord {
    index: u32,
    info: Option<clack_extensions::params::ParamInfoQueryObservation>,
    value: Option<clack_extensions::params::ParamValueQueryObservation>,
    #[cfg(all(test, windows))]
    copied: Option<clap_deactivated_parameter_diagnostic::CopiedParameterObservation>,
}

#[derive(Default)]
pub(super) struct ParameterQueryCapture {
    reported_count: Option<u32>,
    rows: Vec<ParameterQueryRecord>,
    allocation_or_capacity_loss: bool,
}

impl ParameterQueryCapture {
    pub(super) fn from_environment() -> Option<Self> {
        if std::env::var_os(PARAMETER_SAME_CALL_ENV).as_deref() != Some(OsStr::new("1")) {
            return None;
        }
        Some(Self::default())
    }

    fn reserve(&mut self, count: u32) {
        self.reported_count = Some(count);
        if count > MAX_PARAMETERS || self.rows.try_reserve_exact(count as usize).is_err() {
            self.allocation_or_capacity_loss = true;
        }
    }

    pub(super) fn begin_row(&mut self, index: u32) {
        if self.rows.len() >= MAX_PARAMETERS as usize || self.rows.len() >= self.rows.capacity() {
            self.allocation_or_capacity_loss = true;
            return;
        }
        // Called immediately after this query's ParamInfoBuffer::new. This is a
        // construction fact, not a reused-buffer read or native write coverage.
        self.rows.push(ParameterQueryRecord {
            index,
            info: None,
            value: None,
            #[cfg(all(test, windows))]
            copied: None,
        });
    }

    pub(super) fn record_info(
        &mut self,
        index: u32,
        info: clack_extensions::params::ParamInfoQueryObservation,
    ) {
        if let Some(row) = self.rows.last_mut().filter(|row| row.index == index) {
            row.info = Some(info);
        } else {
            self.allocation_or_capacity_loss = true;
        }
    }

    pub(super) fn record_value(
        &mut self,
        index: u32,
        value: clack_extensions::params::ParamValueQueryObservation,
    ) {
        if let Some(row) = self.rows.last_mut().filter(|row| row.index == index) {
            row.value = Some(value);
        } else {
            self.allocation_or_capacity_loss = true;
        }
    }

    pub(super) fn suffix(
        &self,
        index: Option<usize>,
        copied: Option<&ClapParameterDescriptor>,
    ) -> String {
        use std::fmt::Write as _;
        const MARKER: &str = " observation_suffix_truncated=0";
        let mut text = ParameterObservationText {
            text: String::with_capacity(MAX_PARAMETER_OBSERVATION_SUFFIX_BYTES),
            truncated: false,
        };
        let row = index.and_then(|index| self.rows.iter().find(|row| row.index as usize == index));
        let incomplete = self.allocation_or_capacity_loss
            || self.reported_count.is_none_or(|count| self.rows.len() != count as usize)
            || self.rows.iter().any(|row| row.info.is_none() || row.value.is_none());
        let _ = write!(
            text,
            "; same_call count={:?} rows={} incomplete={} selected_index={index:?}",
            self.reported_count,
            self.rows.len(),
            incomplete as u8
        );
        if let Some(row) = row {
            let _ = write!(
                text,
                " before_origin=FreshZeroedConstructor before_numeric_fields=zero"
            );
            if let Some(info) = row.info {
                let _ = write!(
                    text,
                    " info_return={:?} raw={} converted={}",
                    info.native_return,
                    ParameterInfoText(info.raw_after),
                    ParameterInfoText(info.converted)
                );
            } else {
                let _ = write!(text, " info_observation=unavailable");
            }
            if let Some(value) = row.value {
                let _ = write!(
                    text,
                    " value_return={:?} value_before={:016X} value_raw={} value_converted={}",
                    value.native_return,
                    value.initialized_before_bits,
                    ParameterBitsText(value.raw_after_bits),
                    ParameterBitsText(value.converted_bits)
                );
            } else {
                let _ = write!(text, " value_observation=NotCalled");
            }
        } else {
            let _ = write!(text, " selected_observation=unavailable");
        }
        if let Some(p) = copied {
            let mask = p.automatable as u8
                | ((p.stepped as u8) << 1)
                | ((p.enumeration as u8) << 2)
                | ((p.read_only as u8) << 3)
                | ((p.hidden as u8) << 4);
            let _ = write!(
                text,
                " copied={:08X}/{mask:02X}/{:016X}/{:016X}/{:016X}/{:016X}",
                p.id,
                p.min_value.to_bits(),
                p.max_value.to_bits(),
                p.default_value.to_bits(),
                p.current_value.to_bits()
            );
        } else {
            let _ = write!(text, " copied=unavailable");
        }
        text.text.push_str(if text.truncated {
            " observation_suffix_truncated=1"
        } else {
            MARKER
        });
        text.text
    }
}

struct ParameterObservationText {
    text: String,
    truncated: bool,
}
impl std::fmt::Write for ParameterObservationText {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        if self.truncated {
            return Err(std::fmt::Error);
        }
        const RESERVED: usize = " observation_suffix_truncated=0".len();
        let available =
            (MAX_PARAMETER_OBSERVATION_SUFFIX_BYTES - RESERVED).saturating_sub(self.text.len());
        let mut count = available.min(value.len());
        while !value.is_char_boundary(count) {
            count -= 1;
        }
        self.text.push_str(&value[..count]);
        if count < value.len() {
            self.truncated = true;
            Err(std::fmt::Error)
        } else {
            Ok(())
        }
    }
}
struct ParameterInfoText(Option<clack_extensions::params::ParamInfoScalars>);
impl std::fmt::Display for ParameterInfoText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(p) => write!(
                f,
                "{:08X}/{:08X}/{:016X}/{:016X}/{:016X}",
                p.id, p.flags, p.min_bits, p.max_bits, p.default_bits
            ),
            None => f.write_str("unavailable"),
        }
    }
}
struct ParameterBitsText(Option<u64>);
impl std::fmt::Display for ParameterBitsText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(bits) => write!(f, "{bits:016X}"),
            None => f.write_str("unavailable"),
        }
    }
}
pub(super) fn parameter_error_with_capture(
    error: AudioProcessorHostError,
    capture: Option<&ParameterQueryCapture>,
    index: Option<usize>,
    copied: Option<&ClapParameterDescriptor>,
) -> AudioProcessorHostError {
    match (error, capture) {
        (AudioProcessorHostError::InvalidContract(mut detail), Some(capture)) => {
            detail.push_str(&capture.suffix(index, copied));
            invalid(detail)
        }
        (error, _) => error,
    }
}

#[cfg(test)]
pub(super) fn list_parameters(
    handle: &clack_host::prelude::PluginMainThreadHandle<'_>,
) -> Result<Vec<ClapParameterDescriptor>, AudioProcessorHostError> {
    list_parameters_observed(handle, None)
}

// Private opt-in startup consumer. Existing callers/default tests use None.
pub(super) fn list_parameters_observed(
    handle: &clack_host::prelude::PluginMainThreadHandle<'_>,
    mut capture: Option<&mut ParameterQueryCapture>,
) -> Result<Vec<ClapParameterDescriptor>, AudioProcessorHostError> {
    let Some(extension) = handle.get_extension::<PluginParams>() else {
        return Ok(Vec::new());
    };
    let count = extension.count(handle);
    if count > MAX_PARAMETERS {
        if let Some(capture) = capture.as_deref_mut() {
            capture.reserve(count);
        }
        return Err(parameter_error_with_capture(
            invalid("CLAP parameter count exceeds the supported bound"),
            capture.as_deref(),
            None,
            None,
        ));
    }
    let mut parameters = Vec::with_capacity(count as usize);
    if let Some(capture) = capture.as_deref_mut() {
        capture.reserve(count);
    }
    for index in 0..count {
        let mut buffer = ParamInfoBuffer::new();
        let (info, name) = super::clap_metadata::parameter_info_at(
            handle,
            &extension,
            index,
            &mut buffer,
            capture.as_deref_mut(),
        )?;
        parameters.push(ClapParameterDescriptor {
            id: info.id.get(),
            // Keep the original allocation/evaluation order before get_value.
            name: name.to_owned(),
            min_value: info.min_value,
            max_value: info.max_value,
            default_value: info.default_value,
            current_value: super::clap_metadata::parameter_value_at(
                handle,
                &extension,
                info.id,
                index,
                capture.as_deref_mut(),
            )?,
            automatable: info.flags.contains(ParamInfoFlags::IS_AUTOMATABLE),
            stepped: info.flags.contains(ParamInfoFlags::IS_STEPPED),
            enumeration: info.flags.contains(ParamInfoFlags::IS_ENUM),
            read_only: info.flags.contains(ParamInfoFlags::IS_READONLY),
            hidden: info.flags.contains(ParamInfoFlags::IS_HIDDEN),
        });
        #[cfg(all(test, windows))]
        if let (Some(capture), Some(descriptor)) = (capture.as_deref_mut(), parameters.last()) {
            capture.record_copied(index, descriptor);
        }
    }
    validate_parameters_observed(&parameters, capture.as_deref())?;
    Ok(parameters)
}

pub(super) fn validate_parameters(
    parameters: &[ClapParameterDescriptor],
) -> Result<(), AudioProcessorHostError> {
    validate_parameters_observed(parameters, None)
}

fn validate_parameters_observed(
    parameters: &[ClapParameterDescriptor],
    capture: Option<&ParameterQueryCapture>,
) -> Result<(), AudioProcessorHostError> {
    if parameters.len() > MAX_PARAMETERS as usize {
        return Err(parameter_error_with_capture(
            invalid("CLAP parameter count exceeds the supported bound"),
            capture,
            None,
            None,
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for (index, parameter) in parameters.iter().enumerate() {
        // Keep the original predicate and short-circuit order. Only a refusal
        // formats diagnostics; the descriptor is the already acquired snapshot.
        let row = super::clap_metadata::ParameterRow::from_descriptor(parameter);
        let duplicate = ClapId::from_raw(parameter.id).is_some() && !seen.insert(parameter.id);
        let reason = super::clap_metadata::parameter_invalid_reason(row, duplicate);
        if let Some(reason) = reason {
            // ASCII only: never echo a plugin-provided name, reread a native
            // field, or normalize float values to construct this error.
            return Err(parameter_error_with_capture(
                super::clap_metadata::ParameterFailure::new(index, row, reason).error(),
                capture,
                Some(index),
                Some(parameter),
            ));
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
    mod discovery_error_tests {
        use super::*;
        use std::io::{Seek, SeekFrom};

        const CHILD_MODE_ENV: &str = "MONDRIAN_CLAP_DISCOVERY_ERROR_TEST_MODE";
        const CHILD_READY_ENV: &str = "MONDRIAN_CLAP_DISCOVERY_ERROR_TEST_READY";

        fn held_file(path: &Path) -> fs::File {
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(path)
                .expect("create parent-owned ordinary diagnostic")
        }

        fn rejected_status() -> std::process::ExitStatus {
            #[cfg(windows)]
            {
                use std::os::windows::process::ExitStatusExt;
                std::process::ExitStatus::from_raw(17)
            }
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                std::process::ExitStatus::from_raw(17 << 8)
            }
        }

        fn original_status_error() -> AudioProcessorHostError {
            unavailable(format!(
                "CLAP discovery worker failed with status {}",
                rejected_status()
            ))
        }

        #[test]
        fn returned_error_text_and_coarse_stage_are_preserved() {
            let error = invalid("CLAP parameter metadata is invalid or ambiguous: index=0 native_id=7 reason=default_out_of_range");
            let packet = discovery_error_packet(DiscoveryErrorStage::Probe, &error);
            assert_eq!(packet.schema_version, 1);
            assert_eq!(packet.stage, DiscoveryErrorStage::Probe);
            assert_eq!(packet.error_debug, format!("Error: {error:?}"));
            assert!(!packet.truncated);
        }

        #[test]
        fn bounded_writer_preserves_utf8_and_exact_byte_boundary() {
            use std::fmt::Write as _;
            let mut exact = DiscoveryErrorText { text: String::new(), truncated: false };
            exact.write_str(&"x".repeat(MAX_DISCOVERY_ERROR_TEXT_BYTES)).unwrap();
            assert_eq!(exact.text.len(), MAX_DISCOVERY_ERROR_TEXT_BYTES);
            assert!(!exact.truncated);
            assert!(exact.write_str("y").is_err());
            assert!(exact.truncated);
            let mut unicode = DiscoveryErrorText { text: String::new(), truncated: false };
            unicode.write_str(&"x".repeat(MAX_DISCOVERY_ERROR_TEXT_BYTES - 2)).unwrap();
            assert!(unicode.write_str("中").is_err());
            assert_eq!(unicode.text.len(), MAX_DISCOVERY_ERROR_TEXT_BYTES - 2);
            assert!(unicode.truncated);
            assert!(std::str::from_utf8(unicode.text.as_bytes()).is_ok());
        }

        #[test]
        fn formatted_error_and_escaped_wire_are_both_bounded() {
            for text in ["中".repeat(4096), "\u{0001}\"\\\n".repeat(4096)] {
                let packet =
                    discovery_error_packet(DiscoveryErrorStage::LibraryLoad, &unavailable(text));
                assert!(packet.truncated);
                assert!(packet.error_debug.len() <= MAX_DISCOVERY_ERROR_TEXT_BYTES);
                let bytes = serde_json::to_vec(&packet).unwrap();
                assert!(bytes.len() as u64 <= MAX_DISCOVERY_ERROR_PACKET_BYTES);
                let decoded: DiscoveryErrorPacket = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(decoded.error_debug, packet.error_debug);
                assert_eq!(decoded.stage, DiscoveryErrorStage::LibraryLoad);
            }
        }

        #[test]
        fn missing_empty_truncated_invalid_or_oversized_diagnostic_keeps_status() {
            assert_eq!(
                discovery_failed_status(rejected_status(), None),
                original_status_error()
            );
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("error.json");
            let mut file = held_file(&path);
            let valid = serde_json::to_vec(&discovery_error_packet(
                DiscoveryErrorStage::Probe,
                &invalid("original"),
            ))
            .unwrap();
            let mut bad_version = serde_json::from_slice::<serde_json::Value>(&valid).unwrap();
            bad_version["schema_version"] = 2.into();
            let mut unknown_field = serde_json::from_slice::<serde_json::Value>(&valid).unwrap();
            unknown_field["surprise"] = true.into();
            let mut oversized_text = serde_json::from_slice::<serde_json::Value>(&valid).unwrap();
            oversized_text["error_debug"] = "x".repeat(MAX_DISCOVERY_ERROR_TEXT_BYTES + 1).into();
            let mut empty_text = serde_json::from_slice::<serde_json::Value>(&valid).unwrap();
            empty_text["error_debug"] = "".into();
            let mut unknown_stage = serde_json::from_slice::<serde_json::Value>(&valid).unwrap();
            unknown_stage["stage"] = "invented_stage".into();
            for bytes in [
                Vec::new(),
                valid[..valid.len() / 2].to_vec(),
                b"not json".to_vec(),
                vec![0xff, 0xfe],
                vec![b'x'; MAX_DISCOVERY_ERROR_PACKET_BYTES as usize + 1],
                serde_json::to_vec(&bad_version).unwrap(),
                serde_json::to_vec(&unknown_field).unwrap(),
                serde_json::to_vec(&oversized_text).unwrap(),
                serde_json::to_vec(&empty_text).unwrap(),
                serde_json::to_vec(&unknown_stage).unwrap(),
            ] {
                fs::write(&path, bytes).unwrap();
                assert_eq!(
                    discovery_failed_status(rejected_status(), Some(&mut file)),
                    original_status_error()
                );
            }
        }

        #[test]
        fn held_descriptor_ignores_replacement_of_child_writable_path() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("error.json");
            let mut file = held_file(&path);
            let original = invalid("original inode diagnostic");
            write_discovery_error(&path, DiscoveryErrorStage::Probe, &original).unwrap();
            fs::rename(&path, temp.path().join("detached-error.json")).unwrap();
            let _replacement = held_file(&path);
            write_discovery_error(
                &path,
                DiscoveryErrorStage::LibraryLoad,
                &invalid("replacement diagnostic"),
            )
            .unwrap();
            let packet = read_discovery_error(&mut file).expect("held original descriptor");
            assert_eq!(packet.stage, DiscoveryErrorStage::Probe);
            assert_eq!(packet.error_debug, format!("Error: {original:?}"));
        }

        #[test]
        fn diagnostic_write_failure_preserves_exact_business_error() {
            let temp = tempfile::tempdir().unwrap();
            let error = invalid("business error is authoritative");
            for path in [temp.path().join("missing.json"), temp.path().to_path_buf()] {
                let result = run_discovery_with_error_report(Some(&path), |stage| {
                    *stage = DiscoveryErrorStage::RequestDecode;
                    Err(error.clone())
                });
                assert_eq!(result, Err(error.clone()));
            }
            assert_eq!(
                run_discovery_with_error_report(None, |_| Err(error.clone())),
                Err(error)
            );
        }

        #[test]
        fn successful_worker_does_not_write_or_clear_diagnostic() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("error.json");
            fs::write(&path, b"preexisting child data").unwrap();
            run_discovery_with_error_report(Some(&path), |_| Ok(())).unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"preexisting child data");
            run_discovery_with_error_report(None, |_| Ok(())).unwrap();
        }

        #[cfg(windows)]
        mod child_start_diagnostic {
            use super::*;
            use std::os::windows::io::AsRawHandle;

            const ENABLE_ENV: &str = "MONDRIAN_CLAP_CHILD_START_DIAGNOSTIC";
            const DIRECTORY_ENV: &str = "MONDRIAN_CLAP_CHILD_START_DIAGNOSTIC_DIRECTORY";
            const MARKER_ENV: &str = "MONDRIAN_CLAP_CHILD_START_FIXTURE_MARKER";
            const ENTRY_BYTES: &[u8] = b"fixture-entry-v1\n";
            const TEXT_BYTES: usize = 1024;
            const REPORT_BYTES: usize = 8 * 1024;

            #[derive(Serialize)]
            struct BoundedText {
                value: String,
                truncated: bool,
            }

            fn text(value: &str) -> BoundedText {
                let mut length = value.len().min(TEXT_BYTES);
                while !value.is_char_boundary(length) {
                    length -= 1;
                }
                BoundedText {
                    value: value[..length].to_owned(),
                    truncated: length != value.len(),
                }
            }

            fn enabled() -> bool {
                std::env::var_os(ENABLE_ENV).as_deref() == Some(OsStr::new("1"))
            }

            fn epoch_millis() -> Option<u128> {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|duration| duration.as_millis())
            }

            // A fixture-test entry witness, not a PE/Rust startup or native loader witness.
            // Missing sink/write failure is secondary and changes no original fixture result.
            pub(super) fn fixture_entry() {
                if !enabled() {
                    return;
                }
                let Some(path) = std::env::var_os(MARKER_ENV) else {
                    return;
                };
                if let Ok(mut file) = fs::OpenOptions::new().write(true).open(path) {
                    let _ = file.write_all(ENTRY_BYTES);
                }
            }

            pub(super) struct Capture {
                directory: PathBuf,
                program: BoundedText,
                arguments: Vec<BoundedText>,
                parent_pid: u32,
                original_sidecar_handle: String,
                requested_explicit_creation_flags: u32,
                marker: Option<fs::File>,
                marker_created: bool,
                before_spawn_epoch_millis: Option<u128>,
            }

            impl Capture {
                pub(super) fn begin(
                    command: &mut Command,
                    sidecar: &fs::File,
                    original_path: &Path,
                    requested_explicit_creation_flags: u32,
                ) -> Option<Self> {
                    if !enabled() {
                        return None;
                    }
                    let directory = PathBuf::from(std::env::var_os(DIRECTORY_ENV)?);
                    // Root supplies an existing local, isolated evidence directory.
                    // No diagnostic directory fallback, assertion or business error is introduced.
                    if !directory.metadata().ok()?.is_dir() {
                        return None;
                    }
                    let marker_path = original_path.with_extension("fixture-entry");
                    let marker = fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create_new(true)
                        .open(&marker_path)
                        .ok();
                    command.env_remove(MARKER_ENV);
                    if marker.is_some() {
                        command.env(MARKER_ENV, &marker_path);
                    }
                    Some(Self {
                        directory,
                        program: text(&command.get_program().to_string_lossy()),
                        arguments: command
                            .get_args()
                            .map(|argument| text(&argument.to_string_lossy()))
                            .collect(),
                        parent_pid: std::process::id(),
                        original_sidecar_handle: format!("{:X}", sidecar.as_raw_handle() as usize),
                        requested_explicit_creation_flags,
                        marker_created: marker.is_some(),
                        marker,
                        before_spawn_epoch_millis: epoch_millis(),
                    })
                }

                // No second try_wait/query/owner. This borrows the same original wait Result
                // before its unchanged unwrap and observes only a parent-held ordinary marker FD.
                pub(super) fn finish(
                    mut self,
                    child_pid: u32,
                    result: &Result<(), AudioProcessorHostError>,
                    sidecar: &fs::File,
                ) {
                    #[derive(Serialize)]
                    struct Report<'a> {
                        schema_version: u32,
                        scope: &'static str,
                        parent_pid: u32,
                        child_pid: u32,
                        before_spawn_epoch_millis: Option<u128>,
                        after_original_wait_epoch_millis: Option<u128>,
                        program: &'a BoundedText,
                        arguments: &'a [BoundedText],
                        original_wait_result: BoundedText,
                        status_source: &'static str,
                        original_sidecar_held: bool,
                        original_sidecar_handle_before: &'a str,
                        original_sidecar_handle_after: String,
                        actual_inheritance_flags: &'static str,
                        creation_flags_changed: bool,
                        requested_explicit_creation_flags: u32,
                        effective_os_creation_flags: &'static str,
                        marker_created: bool,
                        fixture_entry_observed: Option<bool>,
                        marker_read_bytes: Option<usize>,
                        marker_read_error: Option<BoundedText>,
                    }
                    let after_wait = epoch_millis();
                    let (fixture_entry_observed, marker_read_bytes, marker_read_error) =
                        if let Some(marker) = self.marker.as_mut() {
                            let mut bytes = [0_u8; 17];
                            match marker.read(&mut bytes) {
                                Ok(count) => {
                                    (Some(&bytes[..count] == ENTRY_BYTES), Some(count), None)
                                }
                                Err(error) => (None, None, Some(text(&error.to_string()))),
                            }
                        } else {
                            (None, None, None)
                        };
                    let report = Report {
                        schema_version: 1,
                        scope: "cfg_test_original_success_child_wait_only",
                        parent_pid: self.parent_pid,
                        child_pid,
                        before_spawn_epoch_millis: self.before_spawn_epoch_millis,
                        after_original_wait_epoch_millis: after_wait,
                        program: &self.program,
                        arguments: &self.arguments,
                        original_wait_result: text(&format!("{result:?}")),
                        status_source: "original_wait_result_only_no_additional_process_poll",
                        original_sidecar_held: true,
                        original_sidecar_handle_before: &self.original_sidecar_handle,
                        original_sidecar_handle_after: format!(
                            "{:X}",
                            sidecar.as_raw_handle() as usize
                        ),
                        actual_inheritance_flags: "unqueried",
                        creation_flags_changed: self.requested_explicit_creation_flags != 0,
                        requested_explicit_creation_flags: self.requested_explicit_creation_flags,
                        effective_os_creation_flags: "unqueried",
                        marker_created: self.marker_created,
                        fixture_entry_observed,
                        marker_read_bytes,
                        marker_read_error,
                    };
                    let Ok(bytes) = serde_json::to_vec(&report) else {
                        return;
                    };
                    if bytes.len() > REPORT_BYTES {
                        return;
                    }
                    let path = self
                        .directory
                        .join(format!("child-start-{}-{child_pid}.json", self.parent_pid));
                    if let Ok(mut output) =
                        fs::OpenOptions::new().write(true).create_new(true).open(path)
                    {
                        let _ = output.write_all(&bytes);
                    }
                }
            }
        }

        fn child_command(mode: &str, path: &Path, ready: &Path) -> (Command, u32) {
            let mut command = Command::new(std::env::current_exe().unwrap());
            let module = module_path!().split_once("::").expect("crate-qualified module path").1;
            command
                .args([
                    "--ignored",
                    "--exact",
                    &format!("{module}::owned_child_fixture"),
                    "--test-threads=1",
                ])
                .env(CHILD_MODE_ENV, mode)
                .env(ERROR_ENV, path)
                .env(CHILD_READY_ENV, ready)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            let requested_explicit_creation_flags =
                super::super::super::supervisor::configure_background_worker_start(&mut command);
            #[cfg(not(windows))]
            let requested_explicit_creation_flags = 0;
            (command, requested_explicit_creation_flags)
        }

        #[test]
        #[ignore = "private subprocess entry; ordinary parent tests select exact fixture"]
        fn owned_child_fixture() {
            let Some(mode) = std::env::var_os(CHILD_MODE_ENV) else {
                return;
            };
            let path = PathBuf::from(std::env::var_os(ERROR_ENV).expect("parent sidecar"));
            let ready =
                PathBuf::from(std::env::var_os(CHILD_READY_ENV).expect("parent ready path"));
            #[cfg(windows)]
            child_start_diagnostic::fixture_entry();
            let error = invalid("owned child returned business error");
            match mode.to_str().unwrap() {
                "error" => {
                    assert_eq!(
                        run_discovery_with_error_report(Some(&path), |stage| {
                            *stage = DiscoveryErrorStage::RequestValidation;
                            Err(error.clone())
                        }),
                        Err(error)
                    );
                    std::process::exit(17);
                }
                "legacy" => std::process::exit(17),
                "success" => {
                    write_discovery_error(&path, DiscoveryErrorStage::Probe, &error).unwrap();
                }
                "timeout" => {
                    write_discovery_error(&path, DiscoveryErrorStage::Probe, &error).unwrap();
                    fs::write(ready, b"written before hang").unwrap();
                    thread::sleep(Duration::from_secs(60));
                    panic!("parent did not terminate timed-out child");
                }
                other => panic!("unknown child fixture {other}"),
            }
        }

        #[test]
        fn owned_nonzero_child_preserves_actual_status_and_returned_error() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("error.json");
            let ready = temp.path().join("ready");
            let mut file = held_file(&path);
            let child = child_command("error", &path, &ready).0.spawn().unwrap();
            let mut process =
                super::super::super::supervisor::ProcessOwner::with_staging(child, temp);
            let error = wait_for_discovery_worker(&mut process, Some(&mut file))
                .expect_err("native nonzero exit");
            assert_eq!(process.try_wait().unwrap().unwrap(), rejected_status());
            assert_eq!(error, unavailable(format!("CLAP discovery worker failed with status {}; worker stage=RequestValidation: Error: InvalidContract(\"owned child returned business error\")", rejected_status())));
        }

        #[test]
        fn owned_legacy_helper_empty_sidecar_preserves_original_status() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("error.json");
            let ready = temp.path().join("ready");
            let mut file = held_file(&path);
            let child = child_command("legacy", &path, &ready).0.spawn().unwrap();
            let mut process =
                super::super::super::supervisor::ProcessOwner::with_staging(child, temp);
            assert_eq!(
                wait_for_discovery_worker(&mut process, Some(&mut file)),
                Err(original_status_error())
            );
            assert_eq!(process.try_wait().unwrap().unwrap(), rejected_status());
        }

        #[test]
        fn owned_successful_child_ignores_diagnostic_even_when_present() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("error.json");
            let ready = temp.path().join("ready");
            let mut file = held_file(&path);
            let (mut command, requested_explicit_creation_flags) =
                child_command("success", &path, &ready);
            #[cfg(not(windows))]
            let _ = requested_explicit_creation_flags;
            #[cfg(windows)]
            let diagnostic = child_start_diagnostic::Capture::begin(
                &mut command,
                &file,
                &path,
                requested_explicit_creation_flags,
            );
            let child = command.spawn().unwrap();
            #[cfg(windows)]
            let child_pid = child.id();
            let mut process =
                super::super::super::supervisor::ProcessOwner::with_staging(child, temp);
            let result = wait_for_discovery_worker(&mut process, Some(&mut file));
            #[cfg(windows)]
            if let Some(diagnostic) = diagnostic {
                diagnostic.finish(child_pid, &result, &file);
            }
            result.unwrap();
            assert!(process.try_wait().unwrap().unwrap().success());
            assert_eq!(
                file.stream_position().unwrap(),
                0,
                "success did not read sidecar"
            );
            assert!(
                file.metadata().unwrap().len() > 0,
                "child actually wrote diagnostic"
            );
        }

        #[test]
        fn owned_timeout_uses_fixed_deadline_and_ignores_written_unsettled_diagnostic() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("error.json");
            let ready = temp.path().join("ready");
            let mut file = held_file(&path);
            let child = child_command("timeout", &path, &ready).0.spawn().unwrap();
            let mut process =
                super::super::super::supervisor::ProcessOwner::with_staging(child, temp);
            let started = Instant::now();
            assert_eq!(
                wait_for_discovery_worker(&mut process, Some(&mut file)),
                Err(AudioProcessorHostError::WorkerDeadlineExceeded {
                    operation: "CLAP discovery"
                })
            );
            assert!(started.elapsed() >= DISCOVERY_DEADLINE);
            assert_eq!(
                fs::read(ready).unwrap(),
                b"written before hang",
                "diagnostic was present before timeout"
            );
            assert_eq!(
                file.stream_position().unwrap(),
                0,
                "timeout did not read sidecar"
            );
            assert!(
                process.try_wait().unwrap().is_some(),
                "original owner reaped child"
            );
            file.seek(SeekFrom::Start(0)).unwrap();
            assert!(
                read_discovery_error(&mut file).is_some(),
                "valid packet was deliberately ignored"
            );
        }

        #[test]
        #[ignore = "requires freshly built normal Mondrian helper with diagnostic sidecar support"]
        fn installed_worker_library_load_failure_reports_phase_and_original_error() {
            let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
                .map(PathBuf::from)
                .expect("freshly built normal Mondrian helper");
            let temp = tempfile::tempdir().unwrap();
            let library = temp.path().join("not-a-native-plugin.clap");
            fs::write(&library, b"ordinary file, deliberately invalid library").unwrap();
            let error = scan_clap_library_descriptors(&helper, &library)
                .expect_err("native library load must fail");
            assert!(
                matches!(error, AudioProcessorHostError::Unavailable(ref detail)
                if detail.starts_with("CLAP discovery worker failed with status ")
                    && detail.contains("worker stage=LibraryLoad: Error: Unavailable(")
                    && detail.contains("CLAP library load failed:")),
                "actual discovery error: {error:?}"
            );
        }
    }

    #[test]
    fn processing_probe_requires_new_kind_version_and_explicit_main_input_shape() {
        let request = serde_json::json!({"kind":"probe_processing","schema_version":4,"plugin_id":"fixture","sample_rate":48000,"channel_layout":AudioChannelLayout::Stereo,"max_block_frames":64,"offline":true,"state":null,"capture_default_state":false});
        let operation: DiscoveryOperation = serde_json::from_value(request.clone()).unwrap();
        validate_discovery_schema(&operation).unwrap();
        for legacy in ["probe", "future_probe"] {
            let mut bad = request.clone();
            bad["kind"] = serde_json::json!(legacy);
            assert!(serde_json::from_value::<DiscoveryOperation>(bad).is_err());
        }
        let mut missing = request.clone();
        missing.as_object_mut().unwrap().remove("schema_version");
        assert!(serde_json::from_value::<DiscoveryOperation>(missing).is_err());
        let mut extra = request.clone();
        extra["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<DiscoveryOperation>(extra).is_err());
        for version in [1, 2, 3, 5] {
            let mut wrong = request.clone();
            wrong["schema_version"] = serde_json::json!(version);
            let operation: DiscoveryOperation = serde_json::from_value(wrong).unwrap();
            assert!(validate_discovery_schema(&operation).is_err());
        }
        let legacy_notes = serde_json::json!({"kind":"probe_notes","schema_version":1,"plugin_id":"fixture","sample_rate":48000,"channel_layout":AudioChannelLayout::Stereo,"max_block_frames":64,"offline":true,"state":null});
        assert!(validate_discovery_schema(&serde_json::from_value(legacy_notes).unwrap()).is_err());
        for shape in [ClapMainAudioInput::Absent, ClapMainAudioInput::Present] {
            let value = processing_result_for_notes(
                "fixture",
                test_processing_render(),
                Vec::new(),
                &ClapAudioPortMetadata {
                    main_audio_input: shape,
                    contract: AudioProcessorAuxiliaryInputContract::default(),
                    names: Vec::new(),
                    main_audio_output_id: 0,
                    outputs: crate::AudioProcessorAuxiliaryOutputContract::default(),
                },
                0,
                TailLength::Finite(0),
            )
            .unwrap();
            let value = serde_json::to_value(value).unwrap();
            let parsed: ProbeResult = serde_json::from_value(value.clone()).unwrap();
            validate_processing_result_for_notes(&parsed, "fixture", test_processing_render())
                .unwrap();
            assert_eq!(parsed.main_audio_input, shape);
            for key in ["schema_version", "main_audio_input"] {
                let mut missing = value.clone();
                missing.as_object_mut().unwrap().remove(key);
                assert!(serde_json::from_value::<ProbeResult>(missing).is_err());
            }
            let mut extra = value.clone();
            extra["unknown"] = serde_json::json!(0);
            assert!(serde_json::from_value::<ProbeResult>(extra).is_err());
            let mut wrong = value.clone();
            wrong["main_audio_input"] = serde_json::json!(false);
            assert!(serde_json::from_value::<ProbeResult>(wrong).is_err());
            let mut wrong = value;
            wrong["schema_version"] = serde_json::json!(1);
            let wrong: ProbeResult = serde_json::from_value(wrong).unwrap();
            assert!(validate_processing_result_for_notes(
                &wrong,
                "fixture",
                test_processing_render()
            )
            .is_err());
        }
    }

    fn normal_processing_response(
        auxiliary_count: u32,
        input_names: Vec<crate::AudioProcessorInputName>,
    ) -> DiscoveryResponse {
        DiscoveryResponse {
            descriptors: Vec::new(),
            probe: Some(NormalProbeResultV4 {
                schema_version: 4,
                processing: ProbeResult {
                    schema_version: 3,
                    main_audio_input: ClapMainAudioInput::Present,
                    main_audio_output_id: 0,
                    auxiliary_outputs: Vec::new(),
                    plugin_id: "fixture".to_owned(),
                    latency_frames: 0,
                    offline: true,
                    tail: ProbeTail::None,
                    parameters: Vec::new(),
                    auxiliary_buses: (1..=auxiliary_count)
                        .map(|id| AuxiliaryBusWire {
                            bus_key: format!("clap.audio-input.{id}"),
                            channel_layout: AudioChannelLayout::Stereo,
                        })
                        .collect(),
                    captured_state: None,
                },
                input_names,
            }),
        }
    }

    fn realize_normal_processing_fixture(
        response: DiscoveryResponse,
    ) -> Result<ClapProbe, AudioProcessorHostError> {
        realize_normal_probe(
            response,
            std::env::temp_dir().join("normal-probe.clap"),
            [7; 32],
            "fixture",
            test_processing_render(),
            false,
        )
    }

    #[test]
    fn normal_processing_probe_v4_requires_names_and_keeps_processing_v3_strict() {
        let value = serde_json::to_value(normal_processing_response(0, Vec::new())).unwrap();
        let decoded = serde_json::from_value::<DiscoveryResponse>(value.clone()).unwrap();
        let snapshot = realize_normal_processing_fixture(decoded).expect("normal Probe4 snapshot");
        assert!(snapshot.input_names.is_empty());
        assert!(snapshot.captured_state.is_none());
        assert!(snapshot.registration.auxiliary_inputs.buses.is_empty());
        for field in ["schema_version", "processing", "input_names"] {
            let mut missing = value.clone();
            missing["probe"].as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<DiscoveryResponse>(missing).is_err());
        }
        for field in ["schema_version", "main_audio_input", "auxiliary_buses"] {
            let mut missing = value.clone();
            missing["probe"]["processing"].as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<DiscoveryResponse>(missing).is_err());
        }
        let mut null_names = value.clone();
        null_names["probe"]["input_names"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<DiscoveryResponse>(null_names).is_err());
        for depth in 0..3 {
            let mut unknown = value.clone();
            match depth {
                0 => unknown["unexpected"] = true.into(),
                1 => unknown["probe"]["unexpected"] = true.into(),
                _ => unknown["probe"]["processing"]["unexpected"] = true.into(),
            }
            assert!(serde_json::from_value::<DiscoveryResponse>(unknown).is_err());
        }
        let mut unknown_name = value.clone();
        unknown_name["probe"]["input_names"] = serde_json::json!([{
            "bus_key":"clap.audio-input.1", "name":"Sidechain", "unexpected":true
        }]);
        assert!(serde_json::from_value::<DiscoveryResponse>(unknown_name).is_err());
        for version in [0, 1, 2, 3, 5] {
            let mut wrong = value.clone();
            wrong["probe"]["schema_version"] = version.into();
            let decoded = serde_json::from_value::<DiscoveryResponse>(wrong).unwrap();
            assert!(matches!(
                realize_normal_processing_fixture(decoded),
                Err(AudioProcessorHostError::InvalidContract(_))
            ));
        }
        for version in [0, 1, 2, 4, 5] {
            let mut wrong = value.clone();
            wrong["probe"]["processing"]["schema_version"] = version.into();
            let decoded = serde_json::from_value::<DiscoveryResponse>(wrong).unwrap();
            assert!(matches!(
                realize_normal_processing_fixture(decoded),
                Err(AudioProcessorHostError::InvalidContract(_))
            ));
        }
        let mut legacy = value.clone();
        legacy["probe"] = value["probe"]["processing"].clone();
        assert!(serde_json::from_value::<DiscoveryResponse>(legacy).is_err());
    }

    #[test]
    fn normal_processing_probe_names_preserve_reordered_key_sets_and_31_auxiliary_ports() {
        let names = vec![
            crate::AudioProcessorInputName {
                bus_key: "clap.audio-input.31".to_owned(),
                name: "Last input".to_owned(),
            },
            crate::AudioProcessorInputName {
                bus_key: "clap.audio-input.1".to_owned(),
                name: "First input".to_owned(),
            },
        ];
        let expected = names.clone();
        let value = serde_json::to_value(normal_processing_response(31, names)).unwrap();
        let decoded = serde_json::from_value::<DiscoveryResponse>(value).unwrap();
        let snapshot = realize_normal_processing_fixture(decoded).expect("31 auxiliary ports");
        assert_eq!(snapshot.input_names, expected);
        assert_eq!(snapshot.registration.auxiliary_inputs.buses.len(), 31);
        assert_eq!(
            snapshot.registration.auxiliary_inputs.buses[0].bus_key,
            "clap.audio-input.1"
        );
        let error = realize_normal_processing_fixture(normal_processing_response(32, Vec::new()));
        assert!(
            matches!(error, Err(AudioProcessorHostError::InvalidContract(detail))
            if detail == "CLAP probe returned excessive auxiliary ports")
        );
    }

    #[test]
    fn normal_processing_probe_names_refuse_duplicate_unknown_keys_and_invalid_text() {
        let name = |key: &str, text: &str| crate::AudioProcessorInputName {
            bus_key: key.to_owned(),
            name: text.to_owned(),
        };
        for names in [
            vec![
                name("clap.audio-input.1", "A"),
                name("clap.audio-input.1", "B"),
            ],
            vec![name("clap.audio-input.3", "Unknown")],
            vec![name("clap.audio-input.1", "")],
            vec![name("clap.audio-input.1", "line\nbreak")],
            vec![name("clap.audio-input.1", &"x".repeat(1025))],
        ] {
            assert!(matches!(
                realize_normal_processing_fixture(normal_processing_response(2, names)),
                Err(AudioProcessorHostError::InvalidContract(_))
            ));
        }
    }

    fn test_processing_render() -> AudioRenderContract {
        AudioRenderContract {
            sample_rate: 48000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 64,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: usize::MAX,
            public_output_lookahead_budget_frames: usize::MAX,
            compensation_delay_scratch_budget_bytes: usize::MAX,
        }
    }

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
            auxiliary_outputs: Vec::new(),
            main_audio_output_id: 0,
            schema_version: 3,
            main_audio_input: ClapMainAudioInput::Present,
            plugin_id: "fixture".to_owned(),
            latency_frames: 0,
            offline: false,
            tail: super::ProbeTail::None,
            parameters: Vec::new(),
            auxiliary_buses: Vec::new(),
            captured_state: None,
        };
        // Explicit protocol migration: the old seven-field response cannot grant processing.
        let legacy = br#"{"plugin_id":"fixture","latency_frames":0,"offline":false,"tail":"None","parameters":[],"auxiliary_buses":[],"captured_state":null}"#;
        assert!(serde_json::from_slice::<super::ProbeResult>(legacy).is_err());
        assert_eq!(serde_json::to_vec(&response).unwrap(), br#"{"schema_version":3,"main_audio_input":"present","main_audio_output_id":0,"auxiliary_outputs":[],"plugin_id":"fixture","latency_frames":0,"offline":false,"tail":"None","parameters":[],"auxiliary_buses":[],"captured_state":null}"#);
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
            HostParams, ParamDisplayWriter, ParamInfo, ParamInfoQueryObservation, ParamInfoScalars,
            ParamInfoWriter, ParamQueryReturn, ParamRescanFlags, ParamValueQueryObservation,
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
        const PARAM_QUERY_OBSERVATION: u8 = 4;
        const PORT_DUPLICATE_ID: u8 = 5;
        const PORT_MAIN_ROLE: u8 = 6;
        const PORT_TYPE_MONO: u8 = 7;
        const PORT_CHANNEL_COUNT: u8 = 8;
        const PORT_TYPE_NONE: u8 = 9;
        const PORT_TYPE_EMPTY: u8 = 10;
        const PORT_TYPE_OTHER: u8 = 11;
        const PORT_ARBITRARY_WIDE: u8 = 12;
        const PORT_ARBITRARY_ZERO: u8 = 13;
        const PORT_ARBITRARY_MAX: u8 = 14;
        const PORT_ARBITRARY_AUX_WIDE: u8 = 15;
        static PORT_CUSTOM_TYPE: [u8; 4097] = {
            let mut bytes = [b'Z'; 4097];
            bytes[4096] = 0;
            bytes
        };
        const PARAM_QUERY_DEFAULT_BITS: u64 = 0x378F_FE40_0000_0000;
        const PARAM_QUERY_VALUE_BITS: u64 = 0x8000_0000_0000_0000;

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
            parameter_count_calls: AtomicU32,
            parameter_info_calls: AtomicU32,
            parameter_value_calls: AtomicU32,
            parameter_query_order: AtomicU32,
            port_count_calls: [AtomicU32; 2],
            port_get_calls: [AtomicU32; 2],
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
                // SAFETY: every fixture instance in this module is created with
                // the live, actual ClapHost wrapper. This typed
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
            fn count(&self, is_input: bool) -> u32 {
                self.shared.audit.port_count_calls[usize::from(!is_input)]
                    .fetch_add(1, Ordering::SeqCst);
                if is_input && matches!(CASE, PORT_DUPLICATE_ID | PORT_MAIN_ROLE) {
                    3
                } else if is_input && CASE == PORT_ARBITRARY_AUX_WIDE {
                    2
                } else {
                    1
                }
            }

            fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
                self.shared.audit.port_get_calls[usize::from(!is_input)]
                    .fetch_add(1, Ordering::SeqCst);
                let extra_input = is_input
                    && matches!(
                        CASE,
                        PORT_DUPLICATE_ID | PORT_MAIN_ROLE | PORT_ARBITRARY_AUX_WIDE
                    );
                if index != 0 && !(extra_input && index < 3) {
                    return;
                }
                let early_conflict = extra_input && index == 1 && CASE != PORT_ARBITRARY_AUX_WIDE;
                let output_conflict =
                    !is_input && matches!(CASE, PORT_TYPE_MONO | PORT_CHANNEL_COUNT);
                let port_type = if early_conflict
                    || (is_input
                        && matches!(
                            CASE,
                            PORT_TYPE_NONE
                                | PORT_ARBITRARY_WIDE
                                | PORT_ARBITRARY_ZERO
                                | PORT_ARBITRARY_MAX
                                | PORT_ARBITRARY_AUX_WIDE
                        )) {
                    None
                } else if is_input && CASE == PORT_TYPE_EMPTY {
                    Some(AudioPortType(c""))
                } else if is_input && CASE == PORT_TYPE_OTHER {
                    Some(AudioPortType(
                        std::ffi::CStr::from_bytes_with_nul(&PORT_CUSTOM_TYPE).unwrap(),
                    ))
                } else if output_conflict && CASE == PORT_TYPE_MONO {
                    Some(AudioPortType::MONO)
                } else {
                    Some(AudioPortType::STEREO)
                };
                writer.set(&AudioPortInfo {
                    id: ClapId::new(if CASE == PORT_DUPLICATE_ID { 0 } else { index }),
                    name: if CASE >= PORT_DUPLICATE_ID {
                        b"NAME_MUST_NOT_LEAK"
                    } else {
                        b"main"
                    },
                    channel_count: if is_input && CASE == PORT_ARBITRARY_ZERO {
                        0
                    } else if is_input && CASE == PORT_ARBITRARY_MAX {
                        u32::MAX
                    } else if early_conflict
                        || (is_input
                            && (CASE == PORT_ARBITRARY_WIDE
                                || (CASE == PORT_ARBITRARY_AUX_WIDE && index == 1)))
                    {
                        3
                    } else if output_conflict {
                        1
                    } else {
                        2
                    },
                    flags: if CASE == PORT_TYPE_OTHER {
                        AudioPortFlags::from_bits_retain(0x8000_0001)
                    } else if index == 0 || early_conflict {
                        AudioPortFlags::IS_MAIN
                    } else {
                        AudioPortFlags::empty()
                    },
                    port_type,
                    in_place_pair: None,
                });
            }
        }

        // Original lifecycle cases retain their empty parameter set. Only the
        // distinct owned-query case supplies fixed metadata/current-value controls.
        impl<const CASE: u8> FixtureMain<'_, CASE> {
            fn record_parameter_query(&self, code: u32) {
                let _ = self.shared.audit.parameter_query_order.fetch_update(
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                    |order| Some((order << 4) | code),
                );
            }
        }

        impl<const CASE: u8> PluginMainThreadParams for FixtureMain<'_, CASE> {
            fn count(&self) -> u32 {
                if CASE == PARAM_QUERY_OBSERVATION {
                    self.shared.audit.parameter_count_calls.fetch_add(1, Ordering::SeqCst);
                    self.record_parameter_query(5);
                    return 1;
                }
                0
            }
            fn get_info(&self, index: u32, writer: &mut ParamInfoWriter) {
                if CASE == PARAM_QUERY_OBSERVATION {
                    self.shared.audit.parameter_info_calls.fetch_add(1, Ordering::SeqCst);
                    self.record_parameter_query(match index {
                        0 => 1,
                        1 => 2,
                        _ => 9,
                    });
                    if index == 0 {
                        writer.set(&ParamInfo {
                            id: ClapId::new(7),
                            flags: ParamInfoFlags::from_bits_retain(0x8000_0000),
                            cookie: Default::default(),
                            name: b"Observed",
                            module: b"Query",
                            min_value: f64::from_bits(0x8000_0000_0000_0000),
                            max_value: 1.0,
                            default_value: f64::from_bits(PARAM_QUERY_DEFAULT_BITS),
                        });
                    }
                }
            }
            fn get_value(&self, id: ClapId) -> Option<f64> {
                if CASE == PARAM_QUERY_OBSERVATION {
                    self.shared.audit.parameter_value_calls.fetch_add(1, Ordering::SeqCst);
                    self.record_parameter_query(match id.get() {
                        7 => 3,
                        8 => 4,
                        _ => 10,
                    });
                    return (id.get() == 7).then(|| f64::from_bits(PARAM_QUERY_VALUE_BITS));
                }
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

        fn assert_port_refusal<const CASE: u8>(
            reason: &str,
            facts: &str,
            counts: [u32; 2],
            gets: [u32; 2],
        ) {
            let (result, audit) = run::<CASE>();
            let Err(AudioProcessorHostError::InvalidContract(detail)) = result else {
                panic!("original port refusal required");
            };
            assert_eq!(detail, format!(
                "CLAP audio port identity, role, or layout is invalid; reason={reason} {facts} expected_channels=2 expected_type=stereo"
            ));
            assert!(!detail.contains("NAME_MUST_NOT_LEAK"));
            assert!(!detail.contains("ZZZ"));
            for direction in 0..2 {
                assert_eq!(
                    audit.port_count_calls[direction].load(Ordering::SeqCst),
                    counts[direction]
                );
                assert_eq!(
                    audit.port_get_calls[direction].load(Ordering::SeqCst),
                    gets[direction]
                );
            }
            assert_eq!(audit.activations.load(Ordering::SeqCst), 0);
            assert_eq!(audit.deactivations.load(Ordering::SeqCst), 0);
            assert_eq!(audit.latency_calls.load(Ordering::SeqCst), 0);
            assert_eq!(audit.destroy_calls.load(Ordering::SeqCst), 1);
            assert!(!audit.destroy_native_active.load(Ordering::SeqCst));
            assert!(!audit.destroy_host_active.load(Ordering::SeqCst));
            assert_eq!(audit.destroy_incomplete.load(Ordering::SeqCst), 0);
            assert_eq!(audit.destroy_controls.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn probe_audio_port_refusals_preserve_first_reason_direction_and_query_stop() {
            // The second input has multiple faults: precedence must not change.
            assert_port_refusal::<PORT_DUPLICATE_ID>("duplicate_id",
                "direction=input index=1 count=3 id=0 known_flags_bits=00000001 main=true channels=3 type=unspecified", [1, 0], [2, 0]);
            assert_port_refusal::<PORT_MAIN_ROLE>("main_role",
                "direction=input index=1 count=3 id=1 known_flags_bits=00000001 main=true channels=3 type=unspecified", [1, 0], [2, 0]);
            assert_port_refusal::<PORT_TYPE_MONO>("port_type",
                "direction=output index=0 count=1 id=0 known_flags_bits=00000001 main=true channels=1 type=mono", [1, 1], [1, 1]);
            assert_port_refusal::<PORT_CHANNEL_COUNT>("channel_count",
                "direction=output index=0 count=1 id=0 known_flags_bits=00000001 main=true channels=1 type=stereo", [1, 1], [1, 1]);
            let (result, audit) = run::<AFTER_DEACTIVATE_ALL>();
            assert_eq!(
                result.expect("unchanged successful probe").latency_frames,
                17
            );
            for direction in 0..2 {
                assert_eq!(audit.port_count_calls[direction].load(Ordering::SeqCst), 2);
                assert_eq!(audit.port_get_calls[direction].load(Ordering::SeqCst), 2);
            }
            assert_eq!(audit.activations.load(Ordering::SeqCst), 1);
            assert_eq!(audit.deactivations.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn probe_audio_port_type_classification_never_copies_custom_bytes() {
            // The raw fixture sets an unknown flag: Clack's known-bit projection is all we report.
            assert_port_refusal::<PORT_TYPE_OTHER>("port_type",
                "direction=input index=0 count=1 id=0 known_flags_bits=00000001 main=true channels=2 type=other", [1, 0], [1, 0]);
        }

        #[test]
        fn probe_arbitrary_main_lanes_accept_null_and_empty_under_host_stereo_contract() {
            for (result, audit) in [run::<PORT_TYPE_NONE>(), run::<PORT_TYPE_EMPTY>()] {
                let result =
                    result.expect("host Stereo ordinal mapping accepts two arbitrary lanes");
                assert_eq!(result.main_audio_input, ClapMainAudioInput::Present);
                assert_eq!(result.main_audio_output_id, 0);
                assert!(result.auxiliary_buses.is_empty());
                assert!(result.auxiliary_outputs.is_empty());
                for direction in 0..2 {
                    assert_eq!(audit.port_count_calls[direction].load(Ordering::SeqCst), 2);
                    assert_eq!(audit.port_get_calls[direction].load(Ordering::SeqCst), 2);
                }
                assert_eq!(audit.activations.load(Ordering::SeqCst), 1);
                assert_eq!(audit.deactivations.load(Ordering::SeqCst), 1);
                assert_eq!(audit.destroy_calls.load(Ordering::SeqCst), 1);
                assert!(!audit.destroy_native_active.load(Ordering::SeqCst));
                assert!(!audit.destroy_host_active.load(Ordering::SeqCst));
            }
        }

        #[test]
        fn arbitrary_audio_lanes_refuse_unsupported_widths_before_activation() {
            assert_port_refusal::<PORT_ARBITRARY_WIDE>("channel_count",
                "direction=input index=0 count=1 id=0 known_flags_bits=00000001 main=true channels=3 type=unspecified", [1, 0], [1, 0]);
            assert_port_refusal::<PORT_ARBITRARY_ZERO>("channel_count",
                "direction=input index=0 count=1 id=0 known_flags_bits=00000001 main=true channels=0 type=unspecified", [1, 0], [1, 0]);
            assert_port_refusal::<PORT_ARBITRARY_MAX>("channel_count",
                "direction=input index=0 count=1 id=0 known_flags_bits=00000001 main=true channels=4294967295 type=unspecified", [1, 0], [1, 0]);
            assert_port_refusal::<PORT_ARBITRARY_AUX_WIDE>("channel_count",
                "direction=input index=1 count=2 id=1 known_flags_bits=00000000 main=false channels=3 type=unspecified", [1, 0], [2, 0]);
        }

        #[test]
        fn probe_audio_port_error_facts_fit_existing_discovery_transport() {
            let port = AudioPortInfo {
                id: ClapId::new(u32::MAX - 1),
                name: b"NAME_MUST_NOT_LEAK",
                channel_count: u32::MAX,
                flags: AudioPortFlags::from_bits_truncate(u32::MAX),
                port_type: Some(AudioPortType(
                    std::ffi::CStr::from_bytes_with_nul(&PORT_CUSTOM_TYPE).unwrap(),
                )),
                in_place_pair: None,
            };
            for reason in ["duplicate_id", "main_role", "port_type", "channel_count"] {
                let error = audio_port_refusal(reason, false, u32::MAX, u32::MAX, &port, 2);
                let packet = discovery_error_packet(DiscoveryErrorStage::Probe, &error);
                assert!(!packet.truncated);
                assert!(packet.error_debug.is_ascii());
                assert!(packet.error_debug.starts_with("Error: InvalidContract(\"CLAP audio port identity, role, or layout is invalid;"));
                assert!(packet.error_debug.contains("index=4294967295 count=4294967295 id=4294967294 known_flags_bits=0000000F main=true channels=4294967295 type=other"));
                assert!(!packet.error_debug.contains("NAME_MUST_NOT_LEAK"));
                assert!(!packet.error_debug.contains("ZZZ"));
                let parent_context = format!(
                    "CLAP discovery worker failed with status exit code: 1; worker stage=Probe: {}",
                    packet.error_debug
                );
                assert!(parent_context.len() < MAX_DISCOVERY_ERROR_TEXT_BYTES);
                assert!(
                    serde_json::to_vec(&packet).unwrap().len() as u64
                        <= MAX_DISCOVERY_ERROR_PACKET_BYTES
                );
            }
        }

        fn active_rescan_all_bit() -> u16 {
            1_u16 << (super::super::super::clap_parameter_source::ClapParameterSourceFailure::ActiveRescanAll as u8)
        }

        #[test]
        fn parameter_observer_public_methods_preserve_owned_typed_handle_queries() {
            let audit = Arc::new(Audit::default());
            NEXT_AUDIT.with(|slot| {
                assert!(
                    slot.borrow().is_none(),
                    "previous fixture setup was not consumed"
                );
                *slot.borrow_mut() = Some(Arc::clone(&audit));
            });
            let entry = PluginEntry::load_from_clack::<
                SinglePluginEntry<Fixture<PARAM_QUERY_OBSERVATION>>,
            >(c"/test/param-query-observation")
            .expect("owned Clack entry");
            let info = HostInfo::new("Mondrian test", "Mondrian", "https://example.invalid", "0")
                .expect("host identity");
            let shared = ClapHostShared::new();
            let native_shared =
                shared.clone().with_native_logs().expect("existing native host owner");
            let mut instance = PluginInstance::<ClapHost>::new(
                |_| native_shared,
                |shared| super::super::super::clap_main_services::ClapHostMainThread::new(shared),
                &entry,
                c"org.mondrian.test.probe-active-rescan",
                &info,
            )
            .expect("genuine owned PluginInstance");
            NEXT_AUDIT.with(|slot| assert!(slot.borrow().is_none(), "fixture setup unused"));
            let expected = ParamInfoScalars {
                id: 7,
                flags: 0x8000_0000,
                min_bits: 0x8000_0000_0000_0000,
                max_bits: 0x3FF0_0000_0000_0000,
                default_bits: PARAM_QUERY_DEFAULT_BITS,
            };
            let scalar_view = |info: &ParamInfo<'_>| ParamInfoScalars {
                id: info.id.get(),
                flags: info.flags.bits(),
                min_bits: info.min_value.to_bits(),
                max_bits: info.max_value.to_bits(),
                default_bits: info.default_value.to_bits(),
            };
            {
                let handle = instance.plugin_handle();
                let params = handle.get_extension::<PluginParams>().expect("matching params table");
                let mut plain_buffer = ParamInfoBuffer::new();
                let plain = params.get_info(&handle, 0, &mut plain_buffer).expect("plain metadata");
                assert_eq!(scalar_view(&plain), expected);
                assert_eq!(plain.name, b"Observed");
                assert_eq!(plain.module, b"Query");
                assert!(plain.cookie.as_raw().is_null());
                assert_eq!(audit.parameter_info_calls.load(Ordering::SeqCst), 1);
                let mut observed_buffer = ParamInfoBuffer::new();
                let mut seen = None;
                let mut emissions = 0;
                {
                    let observed = params
                        .get_info_observed(&handle, 0, &mut observed_buffer, |event| {
                            emissions += 1;
                            seen = Some(event);
                        })
                        .expect("observed metadata");
                    assert_eq!(scalar_view(&observed), expected);
                    assert_eq!(observed.name, b"Observed");
                    assert_eq!(observed.module, b"Query");
                    assert!(observed.cookie.as_raw().is_null());
                }
                assert_eq!(audit.parameter_info_calls.load(Ordering::SeqCst), 2);
                assert_eq!(emissions, 1);
                assert_eq!(
                    seen,
                    Some(ParamInfoQueryObservation {
                        native_return: ParamQueryReturn::NativeTrue,
                        raw_after: Some(expected),
                        converted: Some(expected),
                    })
                );
                // Defensive out-of-range control through the genuine wrapper.
                assert!(params.get_info(&handle, 1, &mut plain_buffer).is_none());
                assert_eq!(audit.parameter_info_calls.load(Ordering::SeqCst), 3);
                seen = None;
                assert!(params
                    .get_info_observed(&handle, 1, &mut observed_buffer, |event| {
                        emissions += 1;
                        seen = Some(event);
                    })
                    .is_none());
                assert_eq!(audit.parameter_info_calls.load(Ordering::SeqCst), 4);
                assert_eq!(emissions, 2);
                assert_eq!(
                    seen,
                    Some(ParamInfoQueryObservation {
                        native_return: ParamQueryReturn::NativeFalse,
                        raw_after: None,
                        converted: None,
                    })
                );
                assert_eq!(
                    params.get_value(&handle, ClapId::new(7)).map(f64::to_bits),
                    Some(PARAM_QUERY_VALUE_BITS)
                );
                assert_eq!(audit.parameter_value_calls.load(Ordering::SeqCst), 1);
                let mut value_seen = None;
                let mut value_emissions = 0;
                assert_eq!(
                    params
                        .get_value_observed(&handle, ClapId::new(7), |event| {
                            value_emissions += 1;
                            value_seen = Some(event);
                        })
                        .map(f64::to_bits),
                    Some(PARAM_QUERY_VALUE_BITS)
                );
                assert_eq!(audit.parameter_value_calls.load(Ordering::SeqCst), 2);
                assert_eq!(value_emissions, 1);
                assert_eq!(
                    value_seen,
                    Some(ParamValueQueryObservation {
                        native_return: ParamQueryReturn::NativeTrue,
                        initialized_before_bits: 0,
                        raw_after_bits: Some(PARAM_QUERY_VALUE_BITS),
                        converted_bits: Some(PARAM_QUERY_VALUE_BITS),
                    })
                );
                assert_eq!(params.get_value(&handle, ClapId::new(8)), None);
                assert_eq!(audit.parameter_value_calls.load(Ordering::SeqCst), 3);
                value_seen = None;
                assert_eq!(
                    params.get_value_observed(&handle, ClapId::new(8), |event| {
                        value_emissions += 1;
                        value_seen = Some(event);
                    }),
                    None
                );
                assert_eq!(audit.parameter_value_calls.load(Ordering::SeqCst), 4);
                assert_eq!(value_emissions, 2);
                assert_eq!(
                    value_seen,
                    Some(ParamValueQueryObservation {
                        native_return: ParamQueryReturn::NativeFalse,
                        initialized_before_bits: 0,
                        raw_after_bits: None,
                        converted_bits: None,
                    })
                );
            }
            assert_eq!(
                audit.parameter_query_order.load(Ordering::SeqCst),
                0x1122_3344
            );
            // The public-method 0x11223344 assertions above stay intact. These
            // two independent collector cycles each reuse this same owned instance.
            {
                let handle = instance.plugin_handle();
                let reset = || {
                    audit.parameter_count_calls.store(0, Ordering::SeqCst);
                    audit.parameter_info_calls.store(0, Ordering::SeqCst);
                    audit.parameter_value_calls.store(0, Ordering::SeqCst);
                    audit.parameter_query_order.store(0, Ordering::SeqCst);
                };
                let assert_cycle = || {
                    assert_eq!(audit.parameter_count_calls.load(Ordering::SeqCst), 1);
                    assert_eq!(audit.parameter_info_calls.load(Ordering::SeqCst), 1);
                    assert_eq!(audit.parameter_value_calls.load(Ordering::SeqCst), 1);
                    assert_eq!(audit.parameter_query_order.load(Ordering::SeqCst), 0x513);
                };
                let expected_descriptor = ClapParameterDescriptor {
                    id: 7,
                    name: "Observed".to_owned(),
                    min_value: f64::from_bits(0x8000_0000_0000_0000),
                    max_value: f64::from_bits(0x3FF0_0000_0000_0000),
                    default_value: f64::from_bits(PARAM_QUERY_DEFAULT_BITS),
                    current_value: f64::from_bits(PARAM_QUERY_VALUE_BITS),
                    automatable: false,
                    stepped: false,
                    enumeration: false,
                    read_only: false,
                    hidden: false,
                };
                let assert_copied_bits = |parameters: &[ClapParameterDescriptor]| {
                    assert_eq!(parameters, std::slice::from_ref(&expected_descriptor));
                    // PartialEq alone would not distinguish signed zero.
                    assert_eq!(parameters[0].min_value.to_bits(), 0x8000_0000_0000_0000);
                    assert_eq!(parameters[0].max_value.to_bits(), 0x3FF0_0000_0000_0000);
                    assert_eq!(
                        parameters[0].default_value.to_bits(),
                        PARAM_QUERY_DEFAULT_BITS
                    );
                    assert_eq!(
                        parameters[0].current_value.to_bits(),
                        PARAM_QUERY_VALUE_BITS
                    );
                };
                reset();
                let plain = list_parameters(&handle).expect("plain genuine parameter collector");
                assert_cycle();
                assert_copied_bits(&plain);
                reset();
                let mut capture = ParameterQueryCapture::default();
                let observed = list_parameters_observed(&handle, Some(&mut capture))
                    .expect("observed genuine parameter collector");
                assert_cycle();
                assert_copied_bits(&observed);
                assert_eq!(observed, plain);
                assert_eq!(capture.reported_count, Some(1));
                assert_eq!(capture.rows.len(), 1);
                assert!(!capture.allocation_or_capacity_loss);
                let row = &capture.rows[0];
                assert_eq!(row.index, 0);
                assert_eq!(
                    row.info,
                    Some(ParamInfoQueryObservation {
                        native_return: ParamQueryReturn::NativeTrue,
                        raw_after: Some(expected),
                        converted: Some(expected),
                    })
                );
                assert_eq!(
                    row.value,
                    Some(ParamValueQueryObservation {
                        native_return: ParamQueryReturn::NativeTrue,
                        initialized_before_bits: 0,
                        raw_after_bits: Some(PARAM_QUERY_VALUE_BITS),
                        converted_bits: Some(PARAM_QUERY_VALUE_BITS),
                    })
                );
                let suffix = capture.suffix(Some(0), Some(&observed[0]));
                assert!(suffix.contains("incomplete=0 selected_index=Some(0)"));
                assert!(suffix.contains(
                    "raw=00000007/80000000/8000000000000000/3FF0000000000000/378FFE4000000000"
                ));
                assert!(suffix.contains("converted=00000007/80000000/8000000000000000/3FF0000000000000/378FFE4000000000"));
                assert!(
                    suffix.contains("value_raw=8000000000000000 value_converted=8000000000000000")
                );
                assert!(suffix.contains("copied=00000007/00/8000000000000000/3FF0000000000000/378FFE4000000000/8000000000000000"));
            }

            assert_eq!(audit.activations.load(Ordering::SeqCst), 0);
            assert_eq!(audit.deactivations.load(Ordering::SeqCst), 0);
            assert_eq!(audit.latency_calls.load(Ordering::SeqCst), 0);
            drop(instance);
            assert_eq!(audit.destroy_calls.load(Ordering::SeqCst), 1);
            assert!(!audit.destroy_native_active.load(Ordering::SeqCst));
            assert!(!audit.destroy_host_active.load(Ordering::SeqCst));
            assert_eq!(audit.destroy_incomplete.load(Ordering::SeqCst), 0);
            assert_eq!(audit.destroy_controls.load(Ordering::SeqCst), 1);
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
        fn probe_render_error_diagnostic_preserves_owned_abi_failure_and_destruction() {
            let temp = tempfile::tempdir().expect("diagnostic staging");
            let path = temp.path().join("error.json");
            let mut file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("parent sidecar");
            let mut original = None;
            let result = run_discovery_with_error_report(Some(&path), |stage| {
                *stage = DiscoveryErrorStage::Probe;
                let (result, audit) = run::<RENDER_ERROR>();
                let error = result.err().expect("actual owned ABI render refusal");
                assert!(
                    matches!(&error, AudioProcessorHostError::Unavailable(message)
                    if message.contains("render mode was declined"))
                );
                assert_eq!(audit.activations.load(Ordering::SeqCst), 0);
                assert_eq!(audit.deactivations.load(Ordering::SeqCst), 0);
                assert_eq!(audit.destroy_calls.load(Ordering::SeqCst), 1);
                assert!(!audit.destroy_native_active.load(Ordering::SeqCst));
                assert!(!audit.destroy_host_active.load(Ordering::SeqCst));
                assert_eq!(audit.destroy_incomplete.load(Ordering::SeqCst), 0);
                assert_eq!(audit.destroy_controls.load(Ordering::SeqCst), 1);
                original = Some(error.clone());
                Err(error)
            });
            let original = original.expect("original owned ABI error");
            assert_eq!(result, Err(original.clone()));
            let packet = read_discovery_error(&mut file).expect("bounded diagnostic");
            assert_eq!(packet.stage, DiscoveryErrorStage::Probe);
            assert_eq!(packet.error_debug, format!("Error: {original:?}"));
            assert!(!packet.truncated);
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

#[cfg(test)]
mod same_call_consumer_contracts {
    use super::*;
    use clack_extensions::params::{
        ParamInfoQueryObservation, ParamInfoScalars, ParamQueryReturn, ParamValueQueryObservation,
    };

    fn descriptor(id: u32, default_bits: u64) -> ClapParameterDescriptor {
        ClapParameterDescriptor {
            id,
            name: "Mix".to_owned(),
            min_value: -0.0,
            max_value: 1.0,
            default_value: f64::from_bits(default_bits),
            current_value: 0.5,
            automatable: true,
            stepped: false,
            enumeration: false,
            read_only: false,
            hidden: false,
        }
    }
    fn empty_capture() -> ParameterQueryCapture {
        ParameterQueryCapture {
            reported_count: None,
            rows: Vec::new(),
            allocation_or_capacity_loss: false,
        }
    }
    fn record(capture: &mut ParameterQueryCapture, index: u32, p: &ClapParameterDescriptor) {
        capture.begin_row(index);
        let scalars = ParamInfoScalars {
            id: p.id,
            flags: 0x8000_0000,
            min_bits: p.min_value.to_bits(),
            max_bits: p.max_value.to_bits(),
            default_bits: p.default_value.to_bits(),
        };
        capture.record_info(
            index,
            ParamInfoQueryObservation {
                native_return: ParamQueryReturn::NativeTrue,
                raw_after: Some(scalars),
                converted: Some(scalars),
            },
        );
        capture.record_value(
            index,
            ParamValueQueryObservation {
                native_return: ParamQueryReturn::NativeTrue,
                initialized_before_bits: 0,
                raw_after_bits: Some(p.current_value.to_bits()),
                converted_bits: Some(p.current_value.to_bits()),
            },
        );
    }
    fn detail(error: AudioProcessorHostError) -> String {
        match error {
            AudioProcessorHostError::InvalidContract(detail) => detail,
            other => panic!("original InvalidContract required: {other:?}"),
        }
    }

    #[test]
    fn same_call_first_invalid_index_has_exact_raw_converted_copied_bits() {
        let valid = descriptor(7, 0.5_f64.to_bits());
        let bad = descriptor(9, 0x7FF8_0000_0000_0042);
        let later = descriptor(11, 0x7FF8_0000_0000_0099);
        let parameters = [valid, bad.clone(), later];
        let mut capture = empty_capture();
        capture.reserve(3);
        for (index, p) in parameters.iter().enumerate() {
            record(&mut capture, index as u32, p);
        }
        let original = detail(validate_parameters(&parameters).unwrap_err());
        let observed =
            detail(validate_parameters_observed(&parameters, Some(&capture)).unwrap_err());
        assert!(original.contains("index=1 native_id=9 reason=default_nonfinite"));
        assert_eq!(observed, original + &capture.suffix(Some(1), Some(&bad)));
        assert!(observed.contains("selected_index=Some(1) before_origin=FreshZeroedConstructor"));
        assert!(observed.contains("raw=00000009/80000000/8000000000000000/3FF0000000000000/7FF8000000000042 converted=00000009/80000000/8000000000000000/3FF0000000000000/7FF8000000000042"));
        assert!(observed.contains("copied=00000009/01/8000000000000000/3FF0000000000000/7FF8000000000042/3FE0000000000000"));
        assert!(!observed.contains("7FF8000000000099"));
        assert!(observed.ends_with("observation_suffix_truncated=0"));
        let packet = discovery_error_packet(DiscoveryErrorStage::Probe, &invalid(observed.clone()));
        assert_eq!(
            packet.error_debug,
            format!("Error: {:?}", invalid(observed))
        );
        assert!(!packet.truncated);
    }

    #[test]
    fn same_call_missing_false_conversion_failure_and_not_called_are_distinct() {
        for outcome in [
            ParamQueryReturn::MissingCallback,
            ParamQueryReturn::NativeFalse,
            ParamQueryReturn::NativeTrue,
        ] {
            let mut capture = empty_capture();
            capture.reserve(1);
            capture.begin_row(0);
            let raw = (outcome == ParamQueryReturn::NativeTrue).then_some(ParamInfoScalars {
                id: u32::MAX,
                flags: u32::MAX,
                min_bits: 0,
                max_bits: 0,
                default_bits: 0,
            });
            capture.record_info(
                0,
                ParamInfoQueryObservation {
                    native_return: outcome,
                    raw_after: raw,
                    converted: None,
                },
            );
            let suffix = capture.suffix(Some(0), None);
            assert!(suffix.contains(&format!("info_return={outcome:?}")));
            assert!(suffix.contains("converted=unavailable"));
            assert!(suffix.contains("value_observation=NotCalled"));
            assert!(suffix.contains("incomplete=1"));
            if outcome == ParamQueryReturn::NativeTrue {
                assert!(suffix.contains("raw=FFFFFFFF/FFFFFFFF/"));
            } else {
                assert!(suffix.contains("raw=unavailable"));
            }
        }
    }

    #[test]
    fn same_call_missing_value_and_signed_zero_stay_separate_from_default() {
        let p = descriptor(7, 0x378F_FE40_0000_0000);
        let mut capture = empty_capture();
        capture.reserve(1);
        record(&mut capture, 0, &p);
        capture.record_value(
            0,
            ParamValueQueryObservation {
                native_return: ParamQueryReturn::NativeFalse,
                initialized_before_bits: 0,
                raw_after_bits: None,
                converted_bits: None,
            },
        );
        let suffix = capture.suffix(Some(0), Some(&p));
        assert!(suffix.contains("378FFE4000000000"));
        assert!(suffix.contains("8000000000000000"));
        assert!(suffix.contains("value_return=NativeFalse value_before=0000000000000000 value_raw=unavailable value_converted=unavailable"));
        assert!(!suffix.contains("value_observation=NotCalled"));
        validate_parameters_observed(&[p], Some(&capture))
            .expect("diagnostic value record does not become admission authority");
    }

    #[test]
    fn same_call_capacity_loss_and_unknown_index_keep_primary_error_and_admission() {
        let p = descriptor(7, 0.5_f64.to_bits());
        let mut capture = empty_capture();
        capture.reported_count = Some(1);
        capture.begin_row(0); // No reservation simulates a loss without forcing OOM.
        assert!(capture.rows.is_empty());
        let suffix = capture.suffix(Some(0), Some(&p));
        assert!(
            suffix.contains("incomplete=1 selected_index=Some(0) selected_observation=unavailable")
        );
        validate_parameters_observed(std::slice::from_ref(&p), Some(&capture))
            .expect("capture loss preserves success");
        assert_eq!(
            detail(parameter_error_with_capture(
                invalid("primary"),
                Some(&capture),
                Some(0),
                Some(&p)
            )),
            "primary".to_owned() + &suffix
        );
        let unavailable = AudioProcessorHostError::Unavailable("original".to_owned());
        let debug = format!("{unavailable:?}");
        assert_eq!(
            format!(
                "{:?}",
                parameter_error_with_capture(unavailable, Some(&capture), None, None)
            ),
            debug
        );
        let mut oversized = empty_capture();
        oversized.reserve(MAX_PARAMETERS + 1);
        assert!(oversized.rows.is_empty());
        assert!(oversized
            .suffix(None, None)
            .contains("selected_index=None selected_observation=unavailable"));
    }

    #[test]
    fn same_call_suffix_has_explicit_bounded_ascii_truncation_marker() {
        use std::fmt::Write as _;
        let mut text = ParameterObservationText {
            text: String::with_capacity(MAX_PARAMETER_OBSERVATION_SUFFIX_BYTES),
            truncated: false,
        };
        assert!(write!(
            text,
            "{}",
            "x".repeat(MAX_PARAMETER_OBSERVATION_SUFFIX_BYTES)
        )
        .is_err());
        let prefix = text.text.clone();
        assert!(write!(text, "must-not-append-after-loss").is_err());
        assert_eq!(text.text, prefix);
        text.text.push_str(" observation_suffix_truncated=1");
        assert_eq!(text.text.len(), MAX_PARAMETER_OBSERVATION_SUFFIX_BYTES);
        assert!(text.text.is_ascii());
        assert!(text.text.ends_with("observation_suffix_truncated=1"));
        let p = descriptor(u32::MAX - 1, u64::MAX);
        let mut capture = empty_capture();
        capture.reserve(1);
        record(&mut capture, 0, &p);
        let suffix = capture.suffix(Some(0), Some(&p));
        assert!(suffix.is_ascii());
        assert!(suffix.len() <= MAX_PARAMETER_OBSERVATION_SUFFIX_BYTES);
        assert!(suffix.ends_with("observation_suffix_truncated=0"));
        let error = parameter_error_with_capture(
            invalid("x".repeat(MAX_DISCOVERY_ERROR_TEXT_BYTES)),
            Some(&capture),
            Some(0),
            Some(&p),
        );
        let packet = discovery_error_packet(DiscoveryErrorStage::Probe, &error);
        assert!(
            packet.truncated,
            "outer 1024-byte packet must honestly report lost suffix"
        );
        assert_eq!(packet.error_debug.len(), MAX_DISCOVERY_ERROR_TEXT_BYTES);
        assert!(packet.error_debug.starts_with("Error: InvalidContract(\"xxxx"));
    }
}

#[cfg(all(test, windows))]
#[path = "clap_deactivated_parameter_diagnostic.rs"]
mod clap_deactivated_parameter_diagnostic;

#[cfg(all(test, windows))]
#[path = "clap_nondefault_state_diagnostic.rs"]
mod clap_nondefault_state_diagnostic;

#[cfg(all(test, windows))]
#[path = "clap_gain_reference_diagnostic.rs"]
mod clap_gain_reference_diagnostic;

// Test-only direct-native observation. This is not processing admission and has
// no internal native-call deadline: the actual caller's frozen outer Job owns it.
#[cfg(all(test, windows))]
pub(super) fn deactivated_audio_port_census_for_test(
    library: &Path,
    plugin_id: &str,
) -> Result<(serde_json::Value, Option<AudioProcessorHostError>), AudioProcessorHostError> {
    use clack_extensions::audio_ports_config::{
        AudioPortsConfigBuffer, MainPortInfo, PluginAudioPortsConfig,
    };
    use clack_host::extensions::prelude::{Extension, RawExtension};
    use clap_sys::ext::audio_ports_config::{
        clap_plugin_audio_ports_config, CLAP_EXT_AUDIO_PORTS_CONFIG,
    };

    // SAFETY: the caller retains a hash-bound ordinary library-file lease. The
    // entry owns the loaded module and outlives the one native instance below.
    let entry = unsafe { PluginEntry::load(library.as_os_str()) }
        .map_err(|error| unavailable(format!("CLAP census library load failed: {error}")))?;
    let id = CString::new(plugin_id).map_err(|_| invalid("CLAP census ID contains NUL"))?;
    let info = HostInfo::new(
        "Mondrian",
        "Mondrian",
        "https://github.com/shaloong/mondrian",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| invalid(format!("invalid CLAP census host identity: {error}")))?;
    let shared = ClapHostShared::new();
    let native_shared = shared.clone().with_native_logs()?;
    let mut instance = PluginInstance::<ClapHost>::new(
        |_| native_shared,
        |shared| super::clap_main_services::ClapHostMainThread::new(shared),
        &entry,
        id.as_c_str(),
        &info,
    )
    .map_err(|error| unavailable(format!("CLAP census instance failed: {error}")))?;
    let mut input_count = None;
    let mut output_count = None;
    let mut rows = Vec::new();
    let mut default_topology_complete = false;
    let mut config_query_callback = None;
    let mut config_available = None;
    let mut config_count_callback = None;
    let mut config_get_callback = None;
    let mut config_count = None;
    let mut configs_complete = None;
    let mut config_rows = Vec::new();
    let observed = (|| {
        shared.service_callbacks(&mut instance)?;
        let handle = instance.plugin_handle();
        let ports = handle
            .get_extension::<PluginAudioPorts>()
            .ok_or_else(|| invalid("CLAP census has no audio-ports extension"))?;
        let inputs = ports.count(&handle, true);
        let outputs = ports.count(&handle, false);
        input_count = Some(inputs);
        output_count = Some(outputs);
        if inputs > 32 || outputs > 32 {
            return Err(invalid("CLAP census exceeds 32 ports per direction"));
        }
        for (direction, is_input, count) in [("input", true, inputs), ("output", false, outputs)] {
            for index in 0..count {
                let mut buffer = AudioPortInfoBuffer::new();
                let Some(port) = ports.get(&handle, index, is_input, &mut buffer) else {
                    rows.push(serde_json::json!({"direction":direction, "index":index,
                        "count":count, "clack_get_returned_some":false}));
                    return Err(invalid("CLAP census audio-port metadata is unavailable"));
                };
                let port_type = match port.port_type {
                    None => "unspecified",
                    Some(value) if value == AudioPortType::MONO => "mono",
                    Some(value) if value == AudioPortType::STEREO => "stereo",
                    Some(_) => "other",
                };
                rows.push(serde_json::json!({
                    "direction":direction, "index":index, "count":count,
                    "clack_get_returned_some":true, "id":port.id.get(),
                    "known_flags_bits":port.flags.bits(),
                    "main":port.flags.contains(AudioPortFlags::IS_MAIN),
                    "channels":port.channel_count, "type_class":port_type,
                    "in_place_pair":port.in_place_pair.map(|value| value.get()),
                }));
            }
        }
        default_topology_complete = true;
        // Query the same initialized, never-activated instance only after the
        // original default topology scan. No config selection or Host rescan
        // capability is declared, and no config is assumed to match default.
        let Some(query) = handle.as_raw().get_extension else {
            config_query_callback = Some(false);
            return Err(invalid("CLAP census plugin extension query is unavailable"));
        };
        config_query_callback = Some(true);
        // SAFETY: main-thread handle is the live instance above. The pinned
        // official ID requests exactly the audio-ports-config ABI table.
        let raw = unsafe { query(handle.as_raw_ptr(), CLAP_EXT_AUDIO_PORTS_CONFIG.as_ptr()) };
        let Some(pointer) = std::ptr::NonNull::new(raw.cast_mut()) else {
            config_available = Some(false);
            return Ok(());
        };
        config_available = Some(true);
        configs_complete = Some(false);
        // SAFETY: this table came from the official ID on this same instance;
        // CLAP keeps it alive until instance destruction. RawExtension retains
        // that identity, and use_extension checks it before inspecting callbacks.
        // The typed wrapper is used only while this main-thread handle is live.
        let (configs, count_callback, get_callback) = unsafe {
            let raw = RawExtension::from_raw_plugin_extension(
                pointer.cast::<()>(),
                std::ptr::NonNull::from(handle.as_raw()),
            );
            let table = handle.use_extension(&raw.cast::<clap_plugin_audio_ports_config>());
            (
                PluginAudioPortsConfig::from_raw(raw),
                table.count.is_some(),
                table.get.is_some(),
            )
        };
        config_count_callback = Some(count_callback);
        config_get_callback = Some(get_callback);
        if !count_callback {
            return Err(invalid(
                "CLAP census configuration count callback is unavailable",
            ));
        }
        let count = configs.count(&handle);
        config_count = Some(count);
        if count > 32 {
            return Err(invalid("CLAP census exceeds 32 configurations"));
        }
        if !get_callback {
            return Err(invalid(
                "CLAP census configuration get callback is unavailable",
            ));
        }
        let main_port = |port: Option<MainPortInfo<'_>>| {
            port.map(|port| {
                let type_class = match port.port_type {
                    None => "unspecified",
                    Some(value) if value == AudioPortType::MONO => "mono",
                    Some(value) if value == AudioPortType::STEREO => "stereo",
                    Some(_) => "other",
                };
                serde_json::json!({"channels":port.channel_count, "type_class":type_class})
            })
        };
        for index in 0..count {
            let mut buffer = AudioPortsConfigBuffer::new();
            let Some(config) = configs.get(&handle, index, &mut buffer) else {
                config_rows.push(serde_json::json!({"index":index, "count":count,
                    "clack_get_returned_some":false}));
                return Err(invalid(
                    "CLAP census configuration metadata is invalid or unavailable",
                ));
            };
            config_rows.push(serde_json::json!({
                "index":index, "count":count, "clack_get_returned_some":true,
                "id":config.id.get(), "input_port_count":config.input_port_count,
                "output_port_count":config.output_port_count,
                "has_main_input":config.main_input.is_some(),
                "has_main_output":config.main_output.is_some(),
                "main_input":main_port(config.main_input),
                "main_output":main_port(config.main_output),
            }));
        }
        configs_complete = Some(true);
        Ok(())
    })();
    // Neither failure nor success may publish completion before destroy/deinit
    // have returned on this owner thread. A hang/crash produces no returned carrier.
    drop(instance);
    drop(entry);
    Ok((
        serde_json::json!({
            "schema":1, "plugin_id":plugin_id, "process_id":std::process::id(),
            "phase":"default_deactivated", "reader":"clack_audio_ports",
            "input_count":input_count, "output_count":output_count, "rows":rows,
            "audio_ports_config":{
                "extension_query_callback_available":config_query_callback,
                "available_under_current_host":config_available,
                "count_callback_available":config_count_callback,
                "get_callback_available":config_get_callback,
                "count":config_count, "complete":configs_complete, "rows":config_rows,
                "availability_is_not_all_hosts_or_dll_builds":true,
                "config_info_queried":false, "selection_requested":false,
                "default_layout_comparison_performed":false,
            },
            "complete":default_topology_complete, "census_observation_complete":observed.is_ok(),
            "native_instance_drop_returned":true,
            "native_library_drop_returned":true, "known_flags_only":true,
            "null_and_empty_types_normalized_together":true,
            "diagnostic_activation_requested":false, "diagnostic_process_requested":false,
            "diagnostic_parameter_queries_requested":false, "diagnostic_state_load_requested":false,
        }),
        observed.err(),
    ))
}

#[cfg(all(test, windows))]
pub(super) mod cold_settlement_probe_controls {
    use super::*;
    use mondrian_core::{
        NativeChildSettlement, NativeChildSettlementError, NativeChildSettlementRegistrar,
        NativeChildSettlementReporter,
    };
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;

    // Fixed observation fields only; no Child, staging or execution authority.
    #[derive(Default)]
    pub(in crate::processor_isolation) struct Recorder {
        scope_begins: AtomicU32,
        scope_ends: AtomicU32,
        child_begins: AtomicU32,
        child_ends: AtomicU32,
        settled: AtomicU32,
        unconfirmed: AtomicU32,
        unavailable: AtomicBool,
        reject_child: bool,
    }

    impl Recorder {
        pub(in crate::processor_isolation) fn registrar(
            reject_child: bool,
        ) -> (Arc<Self>, NativeChildSettlementRegistrar) {
            let recorder = Arc::new(Self { reject_child, ..Self::default() });
            let registrar = NativeChildSettlementRegistrar::try_new(recorder.clone())
                .expect("root observation scope");
            (recorder, registrar)
        }

        pub(in crate::processor_isolation) fn assert_finished(&self, rejected: bool) {
            assert_eq!(self.scope_begins.load(Ordering::SeqCst), 1);
            assert_eq!(self.scope_ends.load(Ordering::SeqCst), 1);
            assert_eq!(self.child_begins.load(Ordering::SeqCst), 1);
            let finishes = u32::from(!rejected);
            assert_eq!(self.child_ends.load(Ordering::SeqCst), finishes);
            assert_eq!(self.settled.load(Ordering::SeqCst), finishes);
            assert_eq!(self.unconfirmed.load(Ordering::SeqCst), 0);
            assert_eq!(self.unavailable.load(Ordering::SeqCst), rejected);
        }
    }

    impl NativeChildSettlementReporter for Recorder {
        fn begin_scope(&self) -> Result<(), NativeChildSettlementError> {
            self.scope_begins.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn end_scope(&self) {
            self.scope_ends.fetch_add(1, Ordering::SeqCst);
        }
        fn begin_child(&self) -> Result<(), NativeChildSettlementError> {
            self.child_begins.fetch_add(1, Ordering::SeqCst);
            if self.reject_child {
                Err(NativeChildSettlementError::Unavailable)
            } else {
                Ok(())
            }
        }
        fn end_child(&self, settlement: NativeChildSettlement) {
            self.child_ends.fetch_add(1, Ordering::SeqCst);
            match settlement {
                NativeChildSettlement::Settled => {
                    self.settled.fetch_add(1, Ordering::SeqCst);
                }
                NativeChildSettlement::Unconfirmed => {
                    self.unconfirmed.fetch_add(1, Ordering::SeqCst);
                    self.unavailable();
                }
            }
        }
        fn unavailable(&self) {
            self.unavailable.store(true, Ordering::SeqCst);
        }
    }

    const NOTE_GATE_ID: &str = "org.mondrian.fixture.note-gate";

    fn inputs() -> (PathBuf, PathBuf, AudioRenderContract, [u8; 16]) {
        let helper = PathBuf::from(
            std::env::var_os("MONDRIAN_CLAP_TEST_HELPER").expect("current normal Product helper"),
        );
        let plugin = PathBuf::from(
            std::env::var_os("MONDRIAN_INTERNAL_NOTE_GATE_DLL")
                .expect("original fixed B239 Note gate DLL"),
        );
        let accepted = Path::new("E:/codex-artifact-storage/mondrian-3349-plugin-host-sprint/clap-note-gate-build-target-r2/debug/mondrian_clap_note_gate_fixture.dll");
        assert!(helper.is_absolute() && fs::metadata(&helper).unwrap().is_file());
        assert_eq!(
            plugin.canonicalize().unwrap(),
            accepted.canonicalize().unwrap()
        );
        assert_eq!(fs::metadata(&plugin).unwrap().len(), 214_528);
        let hash: String = fingerprint_clap_binary(&plugin)
            .unwrap()
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect();
        assert_eq!(
            hash,
            "B239C594F9547EE3A9F3FA107622A6FE663AAF73425E11248BDB1E0A0F32AF1D"
        );
        let render = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Mono,
            max_block_frames: 14,
            processing_mode: AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 4096,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let mut state = [0; 16];
        state[..4].copy_from_slice(b"MNG1");
        state[4..12].copy_from_slice(&0.5_f64.to_le_bytes());
        state[12..].copy_from_slice(&2_u32.to_le_bytes());
        (helper, plugin, render, state)
    }

    fn assert_parameters(parameters: &[super::super::ClapParameterDescriptor]) {
        assert_eq!(
            parameters
                .iter()
                .map(|parameter| (
                    parameter.id,
                    parameter.name.as_str(),
                    parameter.current_value,
                    parameter.read_only
                ))
                .collect::<Vec<_>>(),
            [
                (17, "Gain", 0.5, false),
                (19, "End scenario", 2.0, false),
                (23, "Exact Off count", 0.0, true),
            ]
        );
    }

    fn assert_normal_probe(probe: &ClapProbe, plugin: &Path) {
        let registration = &probe.registration;
        assert_eq!(registration.plugin_id, NOTE_GATE_ID);
        assert_eq!(
            registration.binary_sha256,
            fingerprint_clap_binary(plugin).unwrap()
        );
        assert_eq!(registration.main_audio_input, ClapMainAudioInput::Present);
        assert_eq!(registration.main_audio_output_id, 0x201);
        assert!(registration.auxiliary_inputs.buses.is_empty());
        assert!(registration.auxiliary_outputs.buses.is_empty());
        assert!(probe.input_names.is_empty());
        assert!(probe.captured_state.is_none());
        assert_parameters(&registration.parameters);
        assert_eq!(
            registration.execution_contract.algorithmic_latency_frames(),
            0
        );
        assert_eq!(
            registration.execution_contract.tail(),
            AudioProcessorTail::Finite(4)
        );
        assert!(registration.execution_contract.requires_state_entry());
        assert!(registration.execution_contract.offline_capable());
    }

    #[test]
    #[ignore = "current normal Product and the original fixed B239 DLL; actual observed Probe4"]
    fn observed_clap_normal_probe_reports_original_child_settled() {
        let (helper, plugin, render, state) = inputs();
        let (recorder, registrar) = Recorder::registrar(false);
        // The real decoder requires NormalProbe4/Processing3, not a synthetic response.
        let probe = probe_clap_plugin_registration_with_names(
            &helper,
            &plugin,
            NOTE_GATE_ID,
            render,
            Some(&state),
            false,
            Some(&registrar),
        )
        .expect("actual observed normal probe");
        assert_normal_probe(&probe, &plugin);
        drop(registrar);
        recorder.assert_finished(false);
    }

    #[test]
    #[ignore = "current normal Product and the original fixed B239 DLL; actual observed Offer5"]
    fn observed_clap_note_offer_reports_original_child_settled() {
        let (helper, plugin, render, state) = inputs();
        let (recorder, registrar) = Recorder::registrar(false);
        let offer = probe_clap_note_processing_offer_with_native_child_settlement(
            &helper,
            &plugin,
            NOTE_GATE_ID,
            render,
            Some(&state),
            Some(&registrar),
        )
        .expect("actual observed full Note offer");
        assert_eq!(offer.schema_version, 6);
        assert_eq!(offer.notes.schema_version, 2);
        assert_eq!(offer.notes.plugin_id, NOTE_GATE_ID);
        assert_eq!(
            offer.notes.binary_sha256,
            fingerprint_clap_binary(&plugin).unwrap()
        );
        assert_eq!(
            offer.notes.state_sha256,
            super::super::clap_note_input_metadata::state_digest(Some(&state))
        );
        assert_eq!(offer.notes.sample_rate, render.sample_rate);
        assert_eq!(offer.notes.channel_layout, render.channel_layout);
        assert_eq!(offer.notes.max_block_frames, render.max_block_frames);
        assert!(offer.notes.offline);
        assert_eq!(
            offer.notes.contract.completion,
            crate::AudioNoteCompletionPolicy::NativeEnd
        );
        assert_eq!(
            offer
                .notes
                .contract
                .ports
                .iter()
                .map(|port| (
                    port.native_id,
                    port.native_index,
                    port.port_key.as_str(),
                    port.channel_count
                ))
                .collect::<Vec<_>>(),
            [
                (0, 0, "clap.note-input.0", 16),
                (0x8000_0017, 1, "clap.note-input.2147483671", 16),
            ]
        );
        assert_eq!(
            offer.notes.contract.limits,
            super::super::clap_note_input_contract::offered_limits()
        );
        assert_eq!(offer.main_audio_input(), ClapMainAudioInput::Present);
        assert_eq!(offer.main_audio_output_id(), 0x201);
        assert_eq!(offer.auxiliary_inputs().len(), 0);
        assert!(offer.auxiliary_outputs().buses.is_empty());
        assert_parameters(offer.parameters());
        assert_eq!(offer.latency_frames(), 0);
        assert_eq!(offer.tail(), AudioProcessorTail::Finite(4));
        assert_eq!(offer.processing_mode(), AudioProcessingMode::Offline);
        drop(registrar);
        recorder.assert_finished(false);
    }

    #[test]
    #[ignore = "current normal Product and the original fixed B239 DLL; rejected observer still probes"]
    fn rejected_child_registration_preserves_actual_clap_probe_result() {
        let (helper, plugin, render, state) = inputs();
        let (recorder, registrar) = Recorder::registrar(true);
        let probe = probe_clap_plugin_registration_with_names(
            &helper,
            &plugin,
            NOTE_GATE_ID,
            render,
            Some(&state),
            false,
            Some(&registrar),
        )
        .expect("observation refusal must not become discovery refusal");
        assert_normal_probe(&probe, &plugin);
        drop(registrar);
        recorder.assert_finished(true);
    }
}
