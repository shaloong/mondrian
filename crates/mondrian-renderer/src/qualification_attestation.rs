//! Qualification-only execution attestation shared by real-device GPU gates.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;

const MAXIMUM_TEST_EXECUTABLE_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Evidence emitted from inside the exact GPU test process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GpuColorGateExecutionAttestation {
    /// One-time authority challenge identity.
    pub challenge_id: String,
    /// SHA-256 of the complete pre-issued challenge manifest.
    pub challenge_manifest_sha256: String,
    /// SHA-256 of the one-time challenge nonce echoed only by the test process.
    pub challenge_nonce_sha256: String,
    /// Exact row run bound by the authority challenge.
    pub cell_run_id: String,
    /// Exact clean source revision used to build the test.
    pub source_revision: String,
    /// Release runtime image associated with the qualification row.
    pub runtime_image_sha256: String,
    /// SHA-256 of the trusted supervisor script.
    pub producer_script_sha256: String,
    /// SHA-256 of the currently executing test binary.
    pub test_executable_sha256: String,
    /// OS process identifier, retained only for transcript correlation.
    pub process_id: u32,
    /// Monotonic wall-clock-independent process-local timestamp projection.
    pub recorded_unix_nanos: u128,
}

/// Read and validate the authority context when a qualification gate is active.
pub fn gpu_color_gate_execution_attestation() -> Result<Option<GpuColorGateExecutionAttestation>> {
    if std::env::var_os("MONDRIAN_GPU_COLOR_GATE_MEASUREMENT_OUTPUT").is_none() {
        return Ok(None);
    }
    let challenge_id = required_environment("MONDRIAN_QUALIFICATION_CHALLENGE_ID")?;
    let challenge_manifest_sha256 =
        required_sha256_environment("MONDRIAN_QUALIFICATION_CHALLENGE_MANIFEST_SHA256")?;
    let challenge_nonce = required_environment("MONDRIAN_QUALIFICATION_CHALLENGE_NONCE")?;
    if challenge_nonce.len() < 32 {
        bail!("qualification challenge nonce must contain at least 256 bits of encoded entropy");
    }
    let cell_run_id = required_environment("MONDRIAN_QUALIFICATION_CELL_RUN_ID")?;
    let source_revision = required_environment("MONDRIAN_QUALIFICATION_SOURCE_REVISION")?;
    if source_revision.len() != 40 || !source_revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("qualification source revision is not a full Git SHA-1");
    }
    let runtime_image_sha256 =
        required_sha256_environment("MONDRIAN_QUALIFICATION_RUNTIME_IMAGE_SHA256")?;
    let producer_script_sha256 =
        required_sha256_environment("MONDRIAN_QUALIFICATION_PRODUCER_SHA256")?;
    let executable = std::env::current_exe().context("resolve GPU gate executable")?;
    let test_executable_sha256 = hash_file(&executable)?;
    let recorded_unix_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("qualification clock precedes Unix epoch")?
        .as_nanos();
    Ok(Some(GpuColorGateExecutionAttestation {
        challenge_id,
        challenge_manifest_sha256,
        challenge_nonce_sha256: hex_digest(challenge_nonce.as_bytes()),
        cell_run_id,
        source_revision: source_revision.to_ascii_lowercase(),
        runtime_image_sha256,
        producer_script_sha256,
        test_executable_sha256,
        process_id: std::process::id(),
        recorded_unix_nanos,
    }))
}

fn required_environment(name: &'static str) -> Result<String> {
    let value = std::env::var(name).with_context(|| format!("missing {name}"))?;
    if value.trim().is_empty() {
        bail!("{name} is empty");
    }
    Ok(value)
}

fn required_sha256_environment(name: &'static str) -> Result<String> {
    let value = required_environment(name)?.to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{name} is not a SHA-256 identity");
    }
    Ok(value)
}

fn hash_file(path: &std::path::Path) -> Result<String> {
    let metadata = std::fs::metadata(path).context("inspect GPU gate executable")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAXIMUM_TEST_EXECUTABLE_BYTES
    {
        bail!("GPU gate executable is empty, non-file, or oversized");
    }
    let mut file = File::open(path).context("open GPU gate executable")?;
    let mut buffer = [0_u8; 1024 * 1024];
    let mut digest = Sha256::new();
    loop {
        let read = file.read(&mut buffer).context("hash GPU gate executable")?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
