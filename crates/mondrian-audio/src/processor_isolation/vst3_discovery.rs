//! Deadline-bound inspection of a user-selected VST3 binary.

use super::protocol::{auxiliary_from_wire, auxiliary_to_wire, AuxiliaryBusWire};
use super::vst3_worker::{
    fingerprint, parameter_descriptors, prepare_audio_inputs, MAX_STATE_BYTES,
};
use super::{
    Vst3EventBusContractV1, Vst3MainAudioInput, Vst3ParameterDescriptor, Vst3PluginRegistration,
};
use crate::{
    AudioProcessingMode, AudioProcessorAuxiliaryInputContract, AudioProcessorExecutionContract,
    AudioProcessorHostError, AudioProcessorInputName, AudioProcessorTail, AudioRenderContract,
};
use mondrian_core::AudioChannelLayout;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use vst3_host::discovery::get_detailed_plugin_info;
use vst3_host::plugin::ProcessMode;
use vst3_host::Vst3Host;

/// Hidden mode for VST3 class listing and render-contract probing.
pub const VST3_DISCOVERY_WORKER_ARGUMENT: &str = "--internal-vst3-discovery-v1";
const REQUEST_ENV: &str = "MONDRIAN_INTERNAL_VST3_DISCOVERY_REQUEST";
const RESPONSE_ENV: &str = "MONDRIAN_INTERNAL_VST3_DISCOVERY_RESPONSE";
const DEADLINE: Duration = Duration::from_secs(10);
const MAX_REQUEST_BYTES: u64 = 512 * 1024;
const MAX_RESPONSE_BYTES: u64 = 512 * 1024;
const MAX_DESCRIPTORS: usize = 4096;
const DISCOVERY_VERSION: u32 = 4;

/// One audio-effect class exported by an installed VST3 library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vst3PluginDescriptor {
    /// Canonical VST3 class identity.
    pub class_id: String,
    /// Plugin display name.
    pub name: String,
    /// Factory vendor.
    pub vendor: Option<String>,
    /// Plugin version string.
    pub version: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscoveryRequest {
    schema_version: u32,
    binary_path: PathBuf,
    operation: DiscoveryOperation,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DiscoveryOperation {
    List {},
    Probe {
        class_id: String,
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        max_block_frames: usize,
        offline: bool,
        state: Option<Vec<u8>>,
        capture_default_state: bool,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[expect(
    clippy::large_enum_variant,
    reason = "cold-path bounded metadata stays inline below 1 KiB to avoid another heap owner; this enum never enters DSP"
)]
pub(super) enum DiscoveryResponse {
    #[serde(rename = "list_v4")]
    List {
        schema_version: u32,
        descriptors: Vec<Vst3PluginDescriptor>,
    },
    #[serde(rename = "probe_v4")]
    Probe {
        schema_version: u32,
        main_audio_input: Vst3MainAudioInput,
        event_bus_contract: Vst3EventBusContractV1,
        class_id: String,
        vendor: Option<String>,
        latency_frames: u32,
        tail_frames: u32,
        parameters: Vec<Vst3ParameterDescriptor>,
        current_values: Vec<(u32, f64)>,
        captured_state: Option<Vec<u8>>,
        auxiliary_buses: Vec<AuxiliaryBusWire>,
        input_names: Vec<AudioProcessorInputName>,
    },
}

/// List only audio-effect classes from a selected binary in a deadline-bound child.
pub fn scan_vst3_binary_descriptors(
    helper_executable: &Path,
    binary_path: &Path,
) -> Result<Vec<Vst3PluginDescriptor>, AudioProcessorHostError> {
    let (path, before) = canonical_binary(binary_path)?;
    let response = request(helper_executable, &path, DiscoveryOperation::List {}, None)?;
    if fingerprint(&path)? != before {
        return Err(unavailable(
            "VST3 binary changed during descriptor discovery",
        ));
    }
    let DiscoveryResponse::List { descriptors, .. } = response else {
        return Err(invalid("VST3 discovery returned the wrong response type"));
    };
    if descriptors.len() > MAX_DESCRIPTORS {
        return Err(invalid("VST3 library exceeds descriptor capacity"));
    }
    let mut ids = std::collections::BTreeSet::new();
    for descriptor in &descriptors {
        if descriptor.class_id.len() != 32
            || !descriptor.class_id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || descriptor.name.is_empty()
            || descriptor.name.len() > 256
            || !ids.insert(&descriptor.class_id)
        {
            return Err(invalid("VST3 class descriptor is invalid or duplicated"));
        }
    }
    Ok(descriptors)
}

/// Probe one exact class, render mode, and optional state in an isolated child.
pub fn probe_vst3_plugin_registration(
    helper_executable: &Path,
    binary_path: &Path,
    class_id: &str,
    render: AudioRenderContract,
    state: Option<&[u8]>,
) -> Result<Vst3PluginRegistration, AudioProcessorHostError> {
    probe_vst3_plugin_registration_with_state(
        helper_executable,
        binary_path,
        class_id,
        render,
        state,
        false,
    )
    .map(|probe| probe.registration)
}

pub(super) struct Vst3Probe {
    pub(super) registration: Vst3PluginRegistration,
    pub(super) input_names: Vec<AudioProcessorInputName>,
    pub(super) current_values: Vec<(u32, f64)>,
    pub(super) captured_state: Option<Vec<u8>>,
}

pub(super) fn probe_vst3_plugin_registration_with_state(
    helper_executable: &Path,
    binary_path: &Path,
    class_id: &str,
    render: AudioRenderContract,
    state: Option<&[u8]>,
    capture_default_state: bool,
) -> Result<Vst3Probe, AudioProcessorHostError> {
    probe_vst3_plugin_registration_with_state_and_settlement(
        helper_executable,
        binary_path,
        class_id,
        render,
        state,
        capture_default_state,
        None,
    )
}

pub(super) fn probe_vst3_plugin_registration_with_state_and_settlement(
    helper_executable: &Path,
    binary_path: &Path,
    class_id: &str,
    render: AudioRenderContract,
    state: Option<&[u8]>,
    capture_default_state: bool,
    native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
) -> Result<Vst3Probe, AudioProcessorHostError> {
    if class_id.len() != 32 || !class_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("VST3 class ID must be 32 hexadecimal characters"));
    }
    if state.is_some_and(|state| state.len() > MAX_STATE_BYTES) {
        return Err(invalid("VST3 state exceeds the admitted limit"));
    }
    let (path, before) = canonical_binary(binary_path)?;
    let response = request(
        helper_executable,
        &path,
        DiscoveryOperation::Probe {
            class_id: class_id.to_owned(),
            sample_rate: render.sample_rate,
            channel_layout: render.channel_layout,
            max_block_frames: render.max_block_frames,
            offline: render.processing_mode == AudioProcessingMode::Offline,
            state: state.map(ToOwned::to_owned),
            capture_default_state,
        },
        native_child_settlement,
    )?;
    if fingerprint(&path)? != before {
        return Err(unavailable("VST3 binary changed during contract probing"));
    }
    let DiscoveryResponse::Probe {
        class_id: actual_id,
        vendor,
        latency_frames,
        tail_frames,
        parameters,
        current_values,
        captured_state,
        auxiliary_buses,
        input_names,
        event_bus_contract,
        main_audio_input,
        ..
    } = response
    else {
        return Err(invalid("VST3 probe returned the wrong response type"));
    };
    if actual_id != class_id
        || captured_state
            .as_ref()
            .is_some_and(|state| state.len() > MAX_STATE_BYTES || !capture_default_state)
        || current_values.len() != parameters.len()
        || current_values.iter().zip(&parameters).any(|((id, value), parameter)| {
            *id != parameter.id || !value.is_finite() || !(0.0..=1.0).contains(value)
        })
    {
        return Err(invalid(
            "VST3 probe identity, state, or parameter values are invalid",
        ));
    }
    let tail = match tail_frames {
        0 => AudioProcessorTail::None,
        u32::MAX => AudioProcessorTail::Infinite,
        frames => AudioProcessorTail::Finite(frames as usize),
    };
    let contract = AudioProcessorExecutionContract::new(
        latency_frames as usize,
        tail,
        true,
        render.processing_mode == AudioProcessingMode::Realtime,
        render.processing_mode == AudioProcessingMode::Offline,
        0,
    )?;
    let auxiliary_inputs =
        decode_probe_inputs(auxiliary_buses, &input_names, render.channel_layout)?;
    main_audio_input.validate_auxiliary(&auxiliary_inputs)?;
    let registration = Vst3PluginRegistration {
        class_id: actual_id,
        vendor,
        binary_path: path,
        binary_sha256: before,
        execution_contract: contract,
        main_audio_input,
        auxiliary_inputs,
        parameters,
        event_bus_contract,
    };
    Ok(Vst3Probe {
        registration,
        input_names,
        current_values,
        captured_state,
    })
}

// Names are an optional presentation snapshot keyed by the admitted buses. Their
// transport order is not a second routing identity; preserve the actual snapshot.
fn decode_probe_inputs(
    auxiliary_buses: Vec<AuxiliaryBusWire>,
    input_names: &[AudioProcessorInputName],
    layout: AudioChannelLayout,
) -> Result<AudioProcessorAuxiliaryInputContract, AudioProcessorHostError> {
    if auxiliary_buses.len() > 31 {
        return Err(invalid("VST3 probe returned excessive auxiliary ports"));
    }
    let auxiliary_inputs = auxiliary_from_wire(auxiliary_buses);
    auxiliary_inputs.validate(layout)?;
    AudioProcessorInputName::validate_all(input_names, &auxiliary_inputs)?;
    Ok(auxiliary_inputs)
}

/// Execute VST3 discovery before application or device initialization.
pub fn run_vst3_discovery_worker() -> Result<(), AudioProcessorHostError> {
    let mut args = std::env::args_os();
    let _executable = args.next();
    if args.next().as_deref() != Some(OsStr::new(VST3_DISCOVERY_WORKER_ARGUMENT))
        || args.next().is_some()
    {
        return Err(invalid("invalid VST3 discovery worker arguments"));
    }
    let request_path = std::env::var_os(REQUEST_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("VST3 discovery request path is missing"))?;
    let response_path = std::env::var_os(RESPONSE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("VST3 discovery response path is missing"))?;
    let bytes = read_bounded(&request_path, MAX_REQUEST_BYTES)?;
    let request = decode_request(&bytes)?;
    let path = fs::canonicalize(&request.binary_path)
        .map_err(|error| unavailable(format!("VST3 binary is unavailable: {error}")))?;
    if !path.is_file() || path != request.binary_path {
        return Err(invalid(
            "VST3 discovery requires an exact canonical binary path",
        ));
    }
    let response = match request.operation {
        DiscoveryOperation::List {} => list_loaded(&path)?,
        DiscoveryOperation::Probe {
            class_id,
            sample_rate,
            channel_layout,
            max_block_frames,
            offline,
            state,
            capture_default_state,
        } => probe_loaded(
            &path,
            &class_id,
            sample_rate,
            channel_layout,
            max_block_frames,
            offline,
            state.as_deref(),
            capture_default_state,
        )?,
    };
    let bytes = serde_json::to_vec(&response)
        .map_err(|error| invalid(format!("VST3 discovery response encoding failed: {error}")))?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(invalid(
            "VST3 discovery response exceeds the metadata limit",
        ));
    }
    fs::write(response_path, bytes)
        .map_err(|error| unavailable(format!("VST3 discovery response write failed: {error}")))
}

fn list_loaded(path: &Path) -> Result<DiscoveryResponse, AudioProcessorHostError> {
    let detailed = get_detailed_plugin_info(path)
        .map_err(|error| unavailable(format!("VST3 class inspection failed: {error}")))?;
    let vendor = (!detailed.factory.vendor.is_empty()).then_some(detailed.factory.vendor);
    let descriptors = detailed
        .classes
        .into_iter()
        .filter(|class| class.category.contains("Audio Module Class"))
        .map(|class| Vst3PluginDescriptor {
            class_id: class.class_id,
            name: class.name,
            vendor: vendor.clone(),
            version: (!class.version.is_empty()).then_some(class.version),
        })
        .collect();
    Ok(DiscoveryResponse::List { schema_version: DISCOVERY_VERSION, descriptors })
}

#[allow(clippy::too_many_arguments)]
fn probe_loaded(
    path: &Path,
    class_id: &str,
    sample_rate: u32,
    layout: AudioChannelLayout,
    max_block_frames: usize,
    offline: bool,
    state: Option<&[u8]>,
    capture_default_state: bool,
) -> Result<DiscoveryResponse, AudioProcessorHostError> {
    let channels = match layout {
        AudioChannelLayout::Mono => 1,
        AudioChannelLayout::Stereo => 2,
        _ => return Err(invalid("VST3 probe channel layout is unsupported")),
    };
    if sample_rate == 0 || max_block_frames == 0 || max_block_frames > i32::MAX as usize {
        return Err(invalid("VST3 probe render extent is invalid"));
    }
    if state.is_some_and(|state| state.len() > MAX_STATE_BYTES) {
        return Err(invalid("VST3 probe state exceeds the limit"));
    }
    let mode = if offline {
        ProcessMode::Offline
    } else {
        ProcessMode::Realtime
    };
    let mut host = Vst3Host::builder()
        .plugin_interfaces(super::vst3_worker::product_plugin_interfaces())
        .initial_process_mode(mode)
        .sample_rate(f64::from(sample_rate))
        .block_size(max_block_frames)
        .input_channels(channels)
        .output_channels(channels)
        .build()
        .map_err(|error| unavailable(format!("VST3 probe host creation failed: {error}")))?;
    let mut plugin = host
        .load_plugin_class(path, class_id)
        .map_err(|error| unavailable(format!("VST3 probe class load failed: {error}")))?;
    if plugin.info().uid != class_id {
        return Err(invalid("VST3 probe loaded a different class"));
    }
    if let Some(state) = state {
        plugin
            .load_state(state)
            .map_err(|error| unavailable(format!("VST3 probe state restore failed: {error}")))?;
    }
    plugin
        .set_process_mode(mode)
        .map_err(|error| invalid(format!("VST3 probe mode is unsupported: {error}")))?;
    let (main_audio_input, audio_inputs) = prepare_audio_inputs(&mut plugin, layout)?;
    let event_bus_contract = Vst3EventBusContractV1::observe(&plugin)?;
    let parameters = parameter_descriptors(&plugin)?;
    let current_values = plugin
        .get_parameters()
        .map_err(|error| invalid(format!("VST3 probe parameter query failed: {error}")))?
        .into_iter()
        .map(|parameter| (parameter.id, parameter.value))
        .collect();
    let captured_state = if capture_default_state {
        let state = plugin
            .save_state()
            .map_err(|error| unavailable(format!("VST3 default state capture failed: {error}")))?;
        if state.len() > MAX_STATE_BYTES {
            return Err(invalid("VST3 captured state exceeds the limit"));
        }
        Some(state)
    } else {
        None
    };
    Ok(DiscoveryResponse::Probe {
        schema_version: DISCOVERY_VERSION,
        main_audio_input,
        event_bus_contract,
        class_id: plugin.info().uid.clone(),
        vendor: (!plugin.info().vendor.is_empty()).then(|| plugin.info().vendor.clone()),
        latency_frames: plugin.latency_samples(),
        tail_frames: plugin.tail_samples(),
        parameters,
        current_values,
        captured_state,
        auxiliary_buses: auxiliary_to_wire(&audio_inputs.contract),
        input_names: audio_inputs.names,
    })
}

pub(super) fn canonical_binary(
    path: &Path,
) -> Result<(PathBuf, [u8; 32]), AudioProcessorHostError> {
    if !path.is_absolute() {
        return Err(invalid("VST3 selection path must be absolute"));
    }
    let path = fs::canonicalize(path)
        .map_err(|error| unavailable(format!("VST3 selection is unavailable: {error}")))?;
    let path = if path.is_dir() {
        if !path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("vst3"))
        {
            return Err(invalid("VST3 bundle directory must have a .vst3 extension"));
        }
        let (folder, extension) = bundle_native_layout()?;
        let mut binary = None;
        let entries = fs::read_dir(path.join(folder))
            .map_err(|error| unavailable(format!("VST3 bundle has no native binary: {error}")))?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                unavailable(format!("VST3 bundle entry is unreadable: {error}"))
            })?;
            let file_type = entry.file_type().map_err(|error| {
                unavailable(format!("VST3 bundle entry type is unavailable: {error}"))
            })?;
            if !file_type.is_file() {
                continue;
            }
            let entry_path = entry.path();
            if extension.is_some_and(|expected| {
                !entry_path
                    .extension()
                    .and_then(OsStr::to_str)
                    .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
            }) {
                continue;
            }
            if extension.is_none()
                && entry_path.file_name().and_then(OsStr::to_str).is_some_and(|name| {
                    name.starts_with('.') || name.ends_with(".plist") || name.ends_with(".txt")
                })
            {
                continue;
            }
            if binary.replace(entry_path).is_some() {
                return Err(invalid("VST3 bundle has multiple native binaries"));
            }
        }
        let binary = binary
            .ok_or_else(|| invalid("VST3 bundle has no native binary for this architecture"))?;
        let binary = fs::canonicalize(binary)
            .map_err(|error| unavailable(format!("VST3 bundle binary is unavailable: {error}")))?;
        if !binary.starts_with(&path) {
            return Err(invalid("VST3 bundle binary escapes the selected bundle"));
        }
        binary
    } else {
        path
    };
    if !path.is_file() {
        return Err(invalid("VST3 binary path must name a regular file"));
    }
    let hash = fingerprint(&path)?;
    Ok((path, hash))
}

fn bundle_native_layout() -> Result<(&'static str, Option<&'static str>), AudioProcessorHostError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok(("Contents/x86_64-win", Some("vst3"))),
        ("windows", "aarch64") => Ok(("Contents/arm64-win", Some("vst3"))),
        ("windows", "x86") => Ok(("Contents/x86-win", Some("vst3"))),
        ("linux", "x86_64") => Ok(("Contents/x86_64-linux", Some("so"))),
        ("linux", "aarch64") => Ok(("Contents/aarch64-linux", Some("so"))),
        ("linux", "x86") => Ok(("Contents/i386-linux", Some("so"))),
        ("macos", _) => Ok(("Contents/MacOS", None)),
        _ => Err(invalid("VST3 bundle layout is unsupported on this target")),
    }
}

fn request(
    helper_executable: &Path,
    binary_path: &Path,
    operation: DiscoveryOperation,
    native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
) -> Result<DiscoveryResponse, AudioProcessorHostError> {
    if !helper_executable.is_absolute() {
        return Err(invalid("VST3 discovery helper path must be absolute"));
    }
    let temp = tempfile::tempdir()
        .map_err(|error| unavailable(format!("VST3 discovery staging failed: {error}")))?;
    let request_path = temp.path().join("request.json");
    let response_path = temp.path().join("response.json");
    let bytes = serde_json::to_vec(&DiscoveryRequest {
        schema_version: DISCOVERY_VERSION,
        binary_path: binary_path.to_path_buf(),
        operation,
    })
    .map_err(|error| invalid(format!("VST3 discovery request encoding failed: {error}")))?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err(invalid("VST3 discovery request exceeds the metadata limit"));
    }
    fs::write(&request_path, bytes)
        .map_err(|error| unavailable(format!("VST3 discovery request write failed: {error}")))?;
    let mut process = {
        let mut command = Command::new(helper_executable);
        command
            .arg(VST3_DISCOVERY_WORKER_ARGUMENT)
            .env(REQUEST_ENV, &request_path)
            .env(RESPONSE_ENV, &response_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        super::supervisor::configure_background_worker_start(&mut command);
        // Own Child and its staging immediately. No fallible post-spawn step may
        // release a bare child or delete files while termination is unconfirmed.
        super::supervisor::ProcessOwner::with_staging(
            command.spawn().map_err(|error| {
                unavailable(format!("VST3 discovery worker failed to start: {error}"))
            })?,
            temp,
        )
    };
    if let Some(registrar) = native_child_settlement {
        // Observation failure is sticky in this domain, never a new discovery
        // refusal. The same original owner retains Child and staging throughout.
        let _ = process.bind_native_child_settlement(registrar);
    }
    finish_discovery_request(process, &response_path)
}

// One real production consumer and the owned-child controls share this exact
// wait/read/decode scope. The owner retains staging until complete response use.
pub(super) fn finish_discovery_request(
    mut process: super::supervisor::ProcessOwner,
    response_path: &Path,
) -> Result<DiscoveryResponse, AudioProcessorHostError> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        match process.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => {
                return Err(unavailable(format!(
                    "VST3 discovery worker failed: {status}"
                )))
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                return Err(retire_discovery_failure(
                    &mut process,
                    AudioProcessorHostError::WorkerDeadlineExceeded { operation: "VST3 discovery" },
                ));
            }
            Err(error) => {
                return Err(retire_discovery_failure(
                    &mut process,
                    unavailable(format!("VST3 discovery worker wait failed: {error}")),
                ));
            }
        }
    }
    let bytes = read_bounded(response_path, MAX_RESPONSE_BYTES)?;
    decode_response(&bytes)
}

fn decode_request(bytes: &[u8]) -> Result<DiscoveryRequest, AudioProcessorHostError> {
    let request: DiscoveryRequest = serde_json::from_slice(bytes)
        .map_err(|error| invalid(format!("VST3 discovery request is invalid: {error}")))?;
    if request.schema_version != DISCOVERY_VERSION {
        return Err(invalid("unsupported VST3 discovery request version"));
    }
    Ok(request)
}

fn decode_response(bytes: &[u8]) -> Result<DiscoveryResponse, AudioProcessorHostError> {
    let response: DiscoveryResponse = serde_json::from_slice(bytes)
        .map_err(|error| invalid(format!("VST3 discovery response is invalid: {error}")))?;
    let version = match &response {
        DiscoveryResponse::List { schema_version, .. }
        | DiscoveryResponse::Probe { schema_version, .. } => *schema_version,
    };
    if version != DISCOVERY_VERSION {
        return Err(invalid("unsupported VST3 discovery response version"));
    }
    Ok(response)
}

pub(super) fn retire_discovery_failure(
    process: &mut super::supervisor::ProcessOwner,
    primary: AudioProcessorHostError,
) -> AudioProcessorHostError {
    process.terminate_reported("VST3 discovery failure");
    primary
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, AudioProcessorHostError> {
    let metadata = fs::metadata(path)
        .map_err(|error| unavailable(format!("VST3 discovery file is unavailable: {error}")))?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(invalid("VST3 discovery file exceeds its limit"));
    }
    let bytes = fs::read(path)
        .map_err(|error| unavailable(format!("VST3 discovery file read failed: {error}")))?;
    if bytes.len() as u64 > limit {
        return Err(invalid("VST3 discovery file grew beyond its limit"));
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
mod bundle_tests {
    use super::*;

    #[test]
    fn selected_bundle_resolves_one_native_binary_and_rejects_ambiguous_layout() {
        let temp = tempfile::tempdir().expect("bundle fixture");
        let root = temp.path().join("Example.vst3");
        let (folder, extension) = bundle_native_layout().expect("supported test target");
        let native = root.join(folder);
        fs::create_dir_all(&native).expect("native bundle directory");
        let first = native.join(format!("processor.{}", extension.unwrap_or("bin")));
        fs::write(&first, b"reference binary").expect("first native binary");
        let (resolved, hash) = canonical_binary(&root).expect("resolve bundle");
        assert_eq!(resolved, first.canonicalize().expect("canonical binary"));
        assert_eq!(hash, fingerprint(&first).expect("fingerprint"));

        let second = native.join(format!("alternate.{}", extension.unwrap_or("bin")));
        fs::write(second, b"ambiguous").expect("second native binary");
        assert!(matches!(
            canonical_binary(&root),
            Err(AudioProcessorHostError::InvalidContract(message))
                if message.contains("multiple native binaries")
        ));
    }

    #[test]
    fn selected_bundle_rejects_wrong_extension_and_missing_native_architecture() {
        let temp = tempfile::tempdir().expect("bundle fixture");
        let ordinary = temp.path().join("Example");
        fs::create_dir_all(&ordinary).expect("ordinary directory");
        assert!(matches!(
            canonical_binary(&ordinary),
            Err(AudioProcessorHostError::InvalidContract(message))
                if message.contains(".vst3 extension")
        ));
        let bundle = temp.path().join("Missing.vst3");
        fs::create_dir_all(&bundle).expect("empty bundle");
        assert!(canonical_binary(&bundle).is_err());
    }
}

#[cfg(test)]
mod version_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn discovery_v4_refuses_legacy_missing_unknown_and_wrong_versions_in_both_directions() {
        // Frozen legacy request/response discriminators: the old request denied
        // unknown fields and the old response only recognized list/probe.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct LegacyRequest {
            binary_path: PathBuf,
            operation: DiscoveryOperation,
        }
        #[derive(Deserialize)]
        #[serde(tag = "kind")]
        enum LegacyResponse {
            #[serde(rename = "list")]
            List {},
            #[serde(rename = "probe")]
            Probe {},
        }
        #[derive(Deserialize)]
        #[serde(tag = "kind")]
        enum PreviousResponse {
            #[serde(rename = "list_v2")]
            List {},
            #[serde(rename = "probe_v2")]
            Probe {},
        }
        #[derive(Deserialize)]
        #[serde(tag = "kind")]
        enum PreviousV3Response {
            #[serde(rename = "list_v3")]
            List {},
            #[serde(rename = "probe_v3")]
            Probe {},
        }
        let legacy_request = json!({"binary_path":"legacy.vst3", "operation":{"kind":"list"}});
        let old: LegacyRequest = serde_json::from_value(legacy_request.clone()).unwrap();
        assert_eq!(old.binary_path, PathBuf::from("legacy.vst3"));
        assert!(matches!(old.operation, DiscoveryOperation::List {}));
        assert!(decode_request(&serde_json::to_vec(&legacy_request).unwrap()).is_err());
        let request = serde_json::to_value(DiscoveryRequest {
            schema_version: 4,
            binary_path: PathBuf::from("current.vst3"),
            operation: DiscoveryOperation::List {},
        })
        .unwrap();
        assert!(decode_request(&serde_json::to_vec(&request).unwrap()).is_ok());
        assert!(serde_json::from_value::<LegacyRequest>(request.clone()).is_err());
        for (field, value) in [
            ("schema_version", json!(1)),
            ("schema_version", json!(2)),
            ("schema_version", json!(3)),
            ("schema_version", json!(5)),
            ("unexpected", json!(true)),
        ] {
            let mut bad = request.clone();
            bad[field] = value;
            assert!(decode_request(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        let mut bad = request.clone();
        bad["operation"]["unexpected"] = json!(true);
        assert!(decode_request(&serde_json::to_vec(&bad).unwrap()).is_err());
        for (old_kind, new_kind) in [("list", "list_v4"), ("probe", "probe_v4")] {
            let mut current = if old_kind == "list" {
                json!({"kind":new_kind,"schema_version":4,"descriptors":[]})
            } else {
                json!({"kind":new_kind,"schema_version":4,"class_id":"0123456789ABCDEF0123456789ABCDEF","vendor":null,"latency_frames":0,"tail_frames":0,"parameters":[],"current_values":[],"captured_state":null,"auxiliary_buses":[],"input_names":[],"main_audio_input":"present","event_bus_contract":{"schema_version":1,"inputs":[],"outputs":[]}})
            };
            assert!(decode_response(&serde_json::to_vec(&current).unwrap()).is_ok());
            assert!(serde_json::from_value::<LegacyResponse>(current.clone()).is_err());
            assert!(serde_json::from_value::<PreviousResponse>(current.clone()).is_err());
            assert!(serde_json::from_value::<PreviousV3Response>(current.clone()).is_err());
            let mut previous_v3 = current.clone();
            previous_v3["kind"] = json!(format!("{old_kind}_v3"));
            previous_v3["schema_version"] = json!(3);
            previous_v3.as_object_mut().unwrap().remove("main_audio_input");
            assert!(serde_json::from_value::<PreviousV3Response>(previous_v3.clone()).is_ok());
            assert!(decode_response(&serde_json::to_vec(&previous_v3).unwrap()).is_err());
            let mut previous = current.clone();
            previous["kind"] = json!(format!("{old_kind}_v2"));
            previous["schema_version"] = json!(2);
            previous.as_object_mut().unwrap().remove("input_names");
            assert!(serde_json::from_value::<PreviousResponse>(previous.clone()).is_ok());
            assert!(decode_response(&serde_json::to_vec(&previous).unwrap()).is_err());
            let mut legacy = current.clone();
            legacy["kind"] = json!(old_kind);
            legacy.as_object_mut().unwrap().remove("schema_version");
            legacy.as_object_mut().unwrap().remove("event_bus_contract");
            assert!(serde_json::from_value::<LegacyResponse>(legacy.clone()).is_ok());
            assert!(decode_response(&serde_json::to_vec(&legacy).unwrap()).is_err());
            for (field, value) in [
                ("schema_version", json!(1)),
                ("schema_version", json!(2)),
                ("schema_version", json!(3)),
                ("schema_version", json!(5)),
                ("unexpected", json!(true)),
            ] {
                let mut bad = current.clone();
                bad[field] = value;
                assert!(decode_response(&serde_json::to_vec(&bad).unwrap()).is_err());
            }
            if old_kind == "probe" {
                for field in ["event_bus_contract", "input_names", "main_audio_input"] {
                    let mut missing = current.clone();
                    missing.as_object_mut().unwrap().remove(field);
                    assert!(decode_response(&serde_json::to_vec(&missing).unwrap()).is_err());
                }
                for shape in [json!(null), json!(false), json!("unknown")] {
                    let mut bad = current.clone();
                    bad["main_audio_input"] = shape;
                    assert!(decode_response(&serde_json::to_vec(&bad).unwrap()).is_err());
                }
                let mut null_names = current.clone();
                null_names["input_names"] = serde_json::Value::Null;
                assert!(decode_response(&serde_json::to_vec(&null_names).unwrap()).is_err());
                let mut unknown_name_field = current.clone();
                unknown_name_field["input_names"] = json!([{
                    "bus_key":"vst3.audio-input.1", "name":"Sidechain", "unexpected":true
                }]);
                assert!(
                    decode_response(&serde_json::to_vec(&unknown_name_field).unwrap()).is_err()
                );
            }
            current.as_object_mut().unwrap().remove("schema_version");
            assert!(decode_response(&serde_json::to_vec(&current).unwrap()).is_err());
        }
    }

    #[test]
    fn probe_input_names_accept_empty_and_reordered_snapshots_without_changing_bus_identity() {
        let buses = || {
            ["vst3.audio-input.1", "vst3.audio-input.2"]
                .map(|bus_key| AuxiliaryBusWire {
                    bus_key: bus_key.to_owned(),
                    channel_layout: AudioChannelLayout::Stereo,
                })
                .to_vec()
        };
        let expected = auxiliary_from_wire(buses());
        assert_eq!(
            decode_probe_inputs(buses(), &[], AudioChannelLayout::Stereo).unwrap(),
            expected
        );
        let names = vec![
            AudioProcessorInputName {
                bus_key: "vst3.audio-input.2".to_owned(),
                name: "Second native input".to_owned(),
            },
            AudioProcessorInputName {
                bus_key: "vst3.audio-input.1".to_owned(),
                name: "First native input".to_owned(),
            },
        ];
        let snapshot = serde_json::to_value(&names).unwrap();
        assert_eq!(
            decode_probe_inputs(buses(), &names, AudioChannelLayout::Stereo).unwrap(),
            expected
        );
        assert_eq!(serde_json::to_value(&names).unwrap(), snapshot);
    }

    #[test]
    fn probe_input_metadata_accepts_31_auxiliary_ports_and_refuses_32_before_conversion() {
        let buses = |count| {
            (1..=count)
                .map(|index| AuxiliaryBusWire {
                    bus_key: format!("vst3.audio-input.{index}"),
                    channel_layout: AudioChannelLayout::Stereo,
                })
                .collect()
        };
        let contract = decode_probe_inputs(buses(31), &[], AudioChannelLayout::Stereo)
            .expect("31 admitted auxiliary ports");
        assert_eq!(contract.buses.len(), 31);
        assert!(matches!(
            decode_probe_inputs(buses(32), &[], AudioChannelLayout::Stereo),
            Err(AudioProcessorHostError::InvalidContract(detail))
                if detail == "VST3 probe returned excessive auxiliary ports"
        ));
    }

    #[test]
    fn probe_input_names_refuse_duplicate_unknown_keys_and_invalid_native_text() {
        let buses = || {
            ["vst3.audio-input.1", "vst3.audio-input.2"]
                .map(|bus_key| AuxiliaryBusWire {
                    bus_key: bus_key.to_owned(),
                    channel_layout: AudioChannelLayout::Stereo,
                })
                .to_vec()
        };
        let name = |bus_key: &str, text: &str| AudioProcessorInputName {
            bus_key: bus_key.to_owned(),
            name: text.to_owned(),
        };
        for names in [
            vec![
                name("vst3.audio-input.1", "A"),
                name("vst3.audio-input.1", "B"),
            ],
            vec![name("vst3.audio-input.3", "Unknown")],
            vec![name("vst3.audio-input.1", "")],
            vec![name("vst3.audio-input.1", "line\nbreak")],
            vec![name("vst3.audio-input.1", &"x".repeat(1025))],
        ] {
            assert!(matches!(
                decode_probe_inputs(buses(), &names, AudioChannelLayout::Stereo),
                Err(AudioProcessorHostError::InvalidContract(_))
            ));
        }
    }
}

#[cfg(all(test, windows))]
mod cold_settlement_probe_controls {
    use super::super::clap_discovery::cold_settlement_probe_controls::Recorder;
    use super::*;

    #[test]
    #[ignore = "current normal Product and the existing owned stateful VST3 Gain DLL"]
    fn observed_vst3_fresh_probe_reports_original_child_settled() {
        let helper = PathBuf::from(
            std::env::var_os("MONDRIAN_VST3_TEST_HELPER").expect("current normal Product helper"),
        );
        let plugin = PathBuf::from(
            std::env::var_os("MONDRIAN_VST3_STATEFUL_TEST_PLUGIN")
                .expect("existing owned stateful VST3 Gain DLL"),
        );
        assert!(helper.is_absolute() && fs::metadata(&helper).unwrap().is_file());
        let render = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 4,
            processing_mode: AudioProcessingMode::Realtime,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 1024,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        let mut component = Vec::from(*b"MSG1");
        component.extend_from_slice(&1_u32.to_le_bytes());
        component.extend_from_slice(&0.5_f64.to_le_bytes());
        let mut state = Vec::from(*b"VST3HOST_STATE\0\0");
        state.extend_from_slice(&1_u32.to_le_bytes());
        state.extend_from_slice(&(component.len() as u32).to_le_bytes());
        state.extend_from_slice(&(component.len() as u32).to_le_bytes());
        state.extend_from_slice(&component);
        state.extend_from_slice(&component);
        let (recorder, registrar) = Recorder::registrar(false);
        let probe = probe_vst3_plugin_registration_with_state_and_settlement(
            &helper,
            &plugin,
            "AE4C8B1910234F819CA26D315F8702A1",
            render,
            Some(&state),
            true,
            Some(&registrar),
        )
        .expect("actual observed Discovery4 fresh probe");
        assert_eq!(
            probe.registration.class_id,
            "AE4C8B1910234F819CA26D315F8702A1"
        );
        assert_eq!(
            probe.registration.binary_sha256,
            fingerprint(&plugin).unwrap()
        );
        assert_eq!(
            probe.registration.main_audio_input,
            Vst3MainAudioInput::Present
        );
        assert!(probe.registration.auxiliary_inputs.buses.is_empty());
        assert!(probe.input_names.is_empty());
        assert_eq!(probe.captured_state.as_deref(), Some(state.as_slice()));
        assert_eq!(probe.current_values, [(0, 0.5)]);
        assert_eq!(probe.registration.parameters.len(), 1);
        let parameter = &probe.registration.parameters[0];
        assert_eq!((parameter.id, parameter.name.as_str()), (0, "Gain"));
        assert_eq!(parameter.default_normalized, 1.0);
        assert_eq!(parameter.step_count, 0);
        assert!(parameter.can_automate && !parameter.read_only);
        assert_eq!(
            probe.registration.execution_contract.algorithmic_latency_frames(),
            0
        );
        assert_eq!(
            probe.registration.execution_contract.tail(),
            AudioProcessorTail::None
        );
        assert!(probe.registration.execution_contract.realtime_capable());
        drop(registrar);
        recorder.assert_finished(false);
    }
}
