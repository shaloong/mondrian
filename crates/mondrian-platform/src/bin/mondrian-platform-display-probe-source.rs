use anyhow::{bail, Context, Result};
use mondrian_platform::SystemPlatformService;
use mondrian_platform_core::{
    DisplayHdrProbe, DisplayHdrProbeDetails, DisplayProbeBackend, DisplayProfileProbe,
    DisplayProfileProbeTarget,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAXIMUM_REQUEST_BYTES: u64 = 1024 * 1024;
const MAXIMUM_PROFILE_BYTES: u64 = 64 * 1024 * 1024;
const MAXIMUM_EXECUTABLE_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureRequest {
    schema_version: u32,
    capture_id: String,
    sequence: u32,
    scenario: String,
    probe_kind: ProbeKind,
    expected_backend: String,
    cell_id: String,
    cell_run_id: String,
    source_revision: String,
    release_candidate_id: String,
    runtime_image_sha256: String,
    environment_sha256: String,
    display_identity: String,
    native_display_path_id: String,
    display_inventory_sha256: String,
    target: ProbeTarget,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProbeKind {
    Icc,
    Hdr,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProbeTarget {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    native_display_id: Option<u64>,
}

#[derive(Serialize)]
struct CaptureTranscript {
    schema_version: u32,
    producer_id: &'static str,
    producer_executable_sha256: String,
    supervisor_script_sha256: String,
    process_id: u32,
    captured_unix_nanos: u128,
    authority_challenge: ChallengeAttestation,
    capture_id: String,
    sequence: u32,
    scenario: String,
    probe_kind: ProbeKind,
    expected_backend: String,
    cell_id: String,
    cell_run_id: String,
    source_revision: String,
    release_candidate_id: String,
    runtime_image_sha256: String,
    environment_sha256: String,
    display_identity: String,
    native_display_path_id: String,
    display_inventory_sha256: String,
    target: ProbeTarget,
    result: NativeProbeResult,
}

#[derive(Serialize)]
struct ChallengeAttestation {
    challenge_id: String,
    manifest_sha256: String,
    nonce_sha256: String,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NativeProbeResult {
    Icc {
        discovery_available: bool,
        backend: Option<String>,
        display_device_name: Option<String>,
        resolved_native_display_path_id: Option<String>,
        source_reference: Option<String>,
        profile_payload: Option<PayloadIdentity>,
        error: Option<String>,
    },
    Hdr {
        discovery_available: bool,
        backend: Option<String>,
        display_device_name: Option<String>,
        resolved_native_display_path_id: Option<String>,
        details: NativeHdrDetails,
        error: Option<String>,
    },
}

#[derive(Serialize)]
struct PayloadIdentity {
    sha256: String,
    byte_length: u64,
}

#[derive(Serialize)]
struct NativeHdrDetails {
    hdr_supported: Option<bool>,
    hdr_enabled: Option<bool>,
    wide_color_supported: Option<bool>,
    wide_color_active: Option<bool>,
    force_disabled: Option<bool>,
    bits_per_color_channel: Option<u32>,
    color_encoding: Option<String>,
    active_transfer_function: Option<String>,
    supported_transfer_functions: Vec<String>,
    sdr_reference_white_nits: Option<u32>,
    min_luminance_millinits: Option<u32>,
    max_luminance_nits: Option<u32>,
    current_headroom_ppm: Option<u32>,
    potential_headroom_ppm: Option<u32>,
    reference_headroom_ppm: Option<u32>,
}

impl From<DisplayHdrProbeDetails> for NativeHdrDetails {
    fn from(details: DisplayHdrProbeDetails) -> Self {
        Self {
            hdr_supported: details.hdr_supported,
            hdr_enabled: details.hdr_enabled,
            wide_color_supported: details.wide_color_supported,
            wide_color_active: details.wide_color_active,
            force_disabled: details.force_disabled,
            bits_per_color_channel: details.bits_per_color_channel,
            color_encoding: details.color_encoding,
            active_transfer_function: details.active_transfer_function,
            supported_transfer_functions: details.supported_transfer_functions,
            sdr_reference_white_nits: details.sdr_reference_white_nits,
            min_luminance_millinits: details.min_luminance_millinits,
            max_luminance_nits: details.max_luminance_nits,
            current_headroom_ppm: details.current_headroom_ppm,
            potential_headroom_ppm: details.potential_headroom_ppm,
            reference_headroom_ppm: details.reference_headroom_ppm,
        }
    }
}

fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let request_path = PathBuf::from(arguments.next().context("missing capture request path")?);
    let transcript_path =
        PathBuf::from(arguments.next().context("missing transcript output path")?);
    let profile_output_path = PathBuf::from(arguments.next().context("missing ICC output path")?);
    if arguments.next().is_some() {
        bail!("platform display probe producer accepts exactly three paths");
    }
    let request_bytes = read_bounded(&request_path, MAXIMUM_REQUEST_BYTES, "capture request")?;
    let request: CaptureRequest =
        serde_json::from_slice(&request_bytes).context("parse capture request")?;
    validate_request(&request)?;
    let challenge = challenge_attestation(&request)?;
    let executable = std::env::current_exe().context("resolve display probe producer image")?;
    let producer_executable_sha256 = hash_file(&executable, MAXIMUM_EXECUTABLE_BYTES)?;
    let supervisor_script_sha256 = required_sha256("MONDRIAN_QUALIFICATION_PRODUCER_SHA256")?;
    let target = DisplayProfileProbeTarget::new(
        (request.target.x, request.target.y),
        (request.target.width, request.target.height),
    )
    .with_native_display_id(request.target.native_display_id);
    let result = match request.probe_kind {
        ProbeKind::Icc => capture_icc(target, &profile_output_path)?,
        ProbeKind::Hdr => capture_hdr(target),
    };
    let captured_unix_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("capture clock precedes Unix epoch")?
        .as_nanos();
    let transcript = CaptureTranscript {
        schema_version: 1,
        producer_id: "mondrian-platform-display-probe-source-v1",
        producer_executable_sha256,
        supervisor_script_sha256,
        process_id: std::process::id(),
        captured_unix_nanos,
        authority_challenge: challenge,
        capture_id: request.capture_id,
        sequence: request.sequence,
        scenario: request.scenario,
        probe_kind: request.probe_kind,
        expected_backend: request.expected_backend,
        cell_id: request.cell_id,
        cell_run_id: request.cell_run_id,
        source_revision: request.source_revision,
        release_candidate_id: request.release_candidate_id,
        runtime_image_sha256: request.runtime_image_sha256,
        environment_sha256: request.environment_sha256,
        display_identity: request.display_identity,
        native_display_path_id: request.native_display_path_id,
        display_inventory_sha256: request.display_inventory_sha256,
        target: request.target,
        result,
    };
    write_create_only_json(&transcript_path, &transcript)
}

fn capture_icc(target: DisplayProfileProbeTarget, output_path: &Path) -> Result<NativeProbeResult> {
    let result = SystemPlatformService.display_icc_profile(target);
    let source_reference = result.source_reference();
    let bytes = match (&result.profile_bytes, &result.profile_path) {
        (Some(bytes), _) => Some(bytes.clone()),
        (None, Some(path)) => Some(read_bounded(
            path,
            MAXIMUM_PROFILE_BYTES,
            "native ICC profile",
        )?),
        (None, None) => None,
    };
    let profile_payload = if let Some(bytes) = bytes {
        write_create_only(output_path, &bytes)?;
        Some(PayloadIdentity {
            sha256: hex_digest(&bytes),
            byte_length: bytes.len() as u64,
        })
    } else {
        None
    };
    Ok(NativeProbeResult::Icc {
        discovery_available: result.discovery_available,
        backend: result.backend.map(backend_name).transpose()?,
        display_device_name: result.display_device_name,
        resolved_native_display_path_id: result.native_display_path_id,
        source_reference,
        profile_payload,
        error: result.error,
    })
}

fn capture_hdr(target: DisplayProfileProbeTarget) -> NativeProbeResult {
    let result = SystemPlatformService.display_hdr_state(target);
    NativeProbeResult::Hdr {
        discovery_available: result.discovery_available,
        backend: result.backend.and_then(|backend| backend_name(backend).ok()),
        display_device_name: result.display_device_name,
        resolved_native_display_path_id: result.native_display_path_id,
        details: result.details.into(),
        error: result.error,
    }
}

fn backend_name(backend: DisplayProbeBackend) -> Result<String> {
    let value = serde_json::to_value(backend).context("serialize display probe backend")?;
    value
        .as_str()
        .map(ToOwned::to_owned)
        .context("display probe backend did not serialize as a string")
}

fn validate_request(request: &CaptureRequest) -> Result<()> {
    if request.schema_version != 1
        || request.sequence == 0
        || request.target.width == 0
        || request.target.height == 0
        || request.capture_id.trim().is_empty()
        || request.scenario.trim().is_empty()
        || request.expected_backend.trim().is_empty()
        || request.native_display_path_id.trim().is_empty()
        || request.source_revision.len() != 40
        || !is_sha256(&request.runtime_image_sha256)
        || !is_sha256(&request.environment_sha256)
        || !is_sha256(&request.display_inventory_sha256)
    {
        bail!("platform display probe capture request is invalid");
    }
    Ok(())
}

fn challenge_attestation(request: &CaptureRequest) -> Result<ChallengeAttestation> {
    let challenge_id = required_environment("MONDRIAN_QUALIFICATION_CHALLENGE_ID")?;
    let manifest_sha256 = required_sha256("MONDRIAN_QUALIFICATION_CHALLENGE_MANIFEST_SHA256")?;
    let nonce = required_environment("MONDRIAN_QUALIFICATION_CHALLENGE_NONCE")?;
    if nonce.len() < 32
        || required_environment("MONDRIAN_QUALIFICATION_CELL_RUN_ID")? != request.cell_run_id
        || required_environment("MONDRIAN_QUALIFICATION_SOURCE_REVISION")?
            != request.source_revision
        || required_sha256("MONDRIAN_QUALIFICATION_RUNTIME_IMAGE_SHA256")?
            != request.runtime_image_sha256
    {
        bail!("platform display probe authority challenge does not bind the request");
    }
    Ok(ChallengeAttestation {
        challenge_id,
        manifest_sha256,
        nonce_sha256: hex_digest(nonce.as_bytes()),
    })
}

fn required_environment(name: &'static str) -> Result<String> {
    let value = std::env::var(name).with_context(|| format!("missing {name}"))?;
    if value.trim().is_empty() {
        bail!("{name} is empty");
    }
    Ok(value)
}

fn required_sha256(name: &'static str) -> Result<String> {
    let value = required_environment(name)?.to_ascii_lowercase();
    if !is_sha256(&value) {
        bail!("{name} is not a SHA-256 identity");
    }
    Ok(value)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn read_bounded(path: &Path, maximum_bytes: u64, label: &str) -> Result<Vec<u8>> {
    let metadata = std::fs::metadata(path).with_context(|| format!("inspect {label}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum_bytes {
        bail!("{label} is empty, non-file, or oversized");
    }
    std::fs::read(path).with_context(|| format!("read {label}"))
}

fn hash_file(path: &Path, maximum_bytes: u64) -> Result<String> {
    let metadata = std::fs::metadata(path).context("inspect producer executable")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum_bytes {
        bail!("producer executable is empty, non-file, or oversized");
    }
    let mut file = File::open(path).context("open producer executable")?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).context("hash producer executable")?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn write_create_only(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .context("create producer output")?;
    output.write_all(bytes).context("write producer output")?;
    output.sync_all().context("flush producer output")
}

fn write_create_only_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value).context("serialize producer transcript")?;
    write_create_only(path, &bytes)
}

fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
