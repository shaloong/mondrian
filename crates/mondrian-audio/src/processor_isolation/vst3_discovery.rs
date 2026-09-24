//! Deadline-bound inspection of a user-selected VST3 binary.

use super::vst3_worker::{fingerprint, parameter_descriptors, MAX_STATE_BYTES};
use super::{Vst3ParameterDescriptor, Vst3PluginRegistration};
use crate::{
    AudioProcessingMode, AudioProcessorExecutionContract, AudioProcessorHostError,
    AudioProcessorTail, AudioRenderContract,
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
    binary_path: PathBuf,
    operation: DiscoveryOperation,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DiscoveryOperation {
    List,
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
#[serde(tag = "kind", rename_all = "snake_case")]
enum DiscoveryResponse {
    List {
        descriptors: Vec<Vst3PluginDescriptor>,
    },
    Probe {
        class_id: String,
        vendor: Option<String>,
        latency_frames: u32,
        tail_frames: u32,
        parameters: Vec<Vst3ParameterDescriptor>,
        current_values: Vec<(u32, f64)>,
        captured_state: Option<Vec<u8>>,
    },
}

/// List only audio-effect classes from a selected binary in a deadline-bound child.
pub fn scan_vst3_binary_descriptors(
    helper_executable: &Path,
    binary_path: &Path,
) -> Result<Vec<Vst3PluginDescriptor>, AudioProcessorHostError> {
    let (path, before) = canonical_binary(binary_path)?;
    let response = request(helper_executable, &path, DiscoveryOperation::List)?;
    if fingerprint(&path)? != before {
        return Err(unavailable(
            "VST3 binary changed during descriptor discovery",
        ));
    }
    let DiscoveryResponse::List { descriptors } = response else {
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
    let registration = Vst3PluginRegistration {
        class_id: actual_id,
        vendor,
        binary_path: path,
        binary_sha256: before,
        execution_contract: contract,
        parameters,
    };
    Ok(Vst3Probe { registration, current_values, captured_state })
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
    let request: DiscoveryRequest = serde_json::from_slice(&bytes)
        .map_err(|error| invalid(format!("VST3 discovery request is invalid: {error}")))?;
    let path = fs::canonicalize(&request.binary_path)
        .map_err(|error| unavailable(format!("VST3 binary is unavailable: {error}")))?;
    if !path.is_file() || path != request.binary_path {
        return Err(invalid(
            "VST3 discovery requires an exact canonical binary path",
        ));
    }
    let response = match request.operation {
        DiscoveryOperation::List => list_loaded(&path)?,
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
    Ok(DiscoveryResponse::List { descriptors })
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
    let mut host = Vst3Host::builder()
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
        .set_process_mode(if offline {
            ProcessMode::Offline
        } else {
            ProcessMode::Realtime
        })
        .map_err(|error| invalid(format!("VST3 probe mode is unsupported: {error}")))?;
    let buses = plugin
        .audio_bus_layout()
        .map_err(|error| invalid(format!("VST3 probe bus query failed: {error}")))?;
    if buses.inputs.len() != 1
        || buses.outputs.len() != 1
        || !buses.inputs[0].active
        || !buses.outputs[0].active
        || buses.inputs[0].channel_count != channels
        || buses.outputs[0].channel_count != channels
    {
        return Err(invalid(
            "VST3 probe requires one active matching main input/output bus",
        ));
    }
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
        class_id: plugin.info().uid.clone(),
        vendor: (!plugin.info().vendor.is_empty()).then(|| plugin.info().vendor.clone()),
        latency_frames: plugin.latency_samples(),
        tail_frames: plugin.tail_samples(),
        parameters,
        current_values,
        captured_state,
    })
}

fn canonical_binary(path: &Path) -> Result<(PathBuf, [u8; 32]), AudioProcessorHostError> {
    if !path.is_absolute() {
        return Err(invalid("VST3 binary path must be absolute"));
    }
    let path = fs::canonicalize(path)
        .map_err(|error| unavailable(format!("VST3 binary is unavailable: {error}")))?;
    if !path.is_file() {
        return Err(invalid("VST3 binary path must name a regular file"));
    }
    let hash = fingerprint(&path)?;
    Ok((path, hash))
}

fn request(
    helper_executable: &Path,
    binary_path: &Path,
    operation: DiscoveryOperation,
) -> Result<DiscoveryResponse, AudioProcessorHostError> {
    if !helper_executable.is_absolute() {
        return Err(invalid("VST3 discovery helper path must be absolute"));
    }
    let temp = tempfile::tempdir()
        .map_err(|error| unavailable(format!("VST3 discovery staging failed: {error}")))?;
    let request_path = temp.path().join("request.json");
    let response_path = temp.path().join("response.json");
    let bytes =
        serde_json::to_vec(&DiscoveryRequest { binary_path: binary_path.to_path_buf(), operation })
            .map_err(|error| invalid(format!("VST3 discovery request encoding failed: {error}")))?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err(invalid("VST3 discovery request exceeds the metadata limit"));
    }
    fs::write(&request_path, bytes)
        .map_err(|error| unavailable(format!("VST3 discovery request write failed: {error}")))?;
    let mut child = Command::new(helper_executable)
        .arg(VST3_DISCOVERY_WORKER_ARGUMENT)
        .env(REQUEST_ENV, &request_path)
        .env(RESPONSE_ENV, &response_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| unavailable(format!("VST3 discovery worker failed to start: {error}")))?;
    let deadline = Instant::now() + DEADLINE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => {
                return Err(unavailable(format!(
                    "VST3 discovery worker failed: {status}"
                )))
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AudioProcessorHostError::WorkerDeadlineExceeded {
                    operation: "VST3 discovery",
                });
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(unavailable(format!(
                    "VST3 discovery worker wait failed: {error}"
                )));
            }
        }
    }
    let bytes = read_bounded(&response_path, MAX_RESPONSE_BYTES)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| invalid(format!("VST3 discovery response is invalid: {error}")))
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
