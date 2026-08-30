//! Isolated and qualification-gated AAF helper Adapter.

use crate::{InterchangeError, InterchangeLimits};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(crate) const AAF_CFB_MAGIC: [u8; 8] = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];
/// Bridge JSON contract required by this release's AAF Toolchain Interface.
pub const AAF_BRIDGE_CONTRACT_VERSION: u32 = 1;

pub(crate) fn validate_aaf_binary(bytes: &[u8]) -> Result<(), InterchangeError> {
    if !bytes.starts_with(&AAF_CFB_MAGIC) {
        return Err(InterchangeError::AafHelperFailed {
            reason: "AAF artifact is not a Compound File Binary document".to_owned(),
        });
    }
    Ok(())
}

/// Helper identity contract returned by `--identity`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AafToolchainIdentity {
    /// Stable implementation key, e.g. `mondrian.pyaaf2.helper`.
    pub implementation: String,
    /// Pinned helper release.
    pub version: String,
    /// Bridge contract implemented by the helper.
    pub bridge_contract_version: u32,
    /// Underlying AAF library and exact version.
    pub engine: String,
}

impl AafToolchainIdentity {
    pub(crate) fn validate(&self, maximum_string_bytes: usize) -> Result<(), InterchangeError> {
        let valid_text = |value: &str| {
            !value.is_empty()
                && value.len() <= maximum_string_bytes
                && !value.chars().any(char::is_control)
        };
        if self.bridge_contract_version != AAF_BRIDGE_CONTRACT_VERSION
            || !valid_text(&self.implementation)
            || !valid_text(&self.version)
            || !valid_text(&self.engine)
        {
            return Err(InterchangeError::AafHelperUnavailable {
                reason:
                    "helper identity is empty, unbounded, unsafe, or uses the wrong bridge contract"
                        .to_owned(),
            });
        }
        Ok(())
    }
}

/// Qualified AAF binary translation Interface.
pub trait AafToolchain: Send + Sync {
    /// Read one AAF binary into the exact versioned bridge document.
    fn decode_aaf(
        &self,
        input: &[u8],
        limits: InterchangeLimits,
    ) -> Result<Vec<u8>, InterchangeError>;
    /// Write one bridge document into a metadata-only AAF binary.
    fn encode_aaf(
        &self,
        bridge: &[u8],
        limits: InterchangeLimits,
    ) -> Result<Vec<u8>, InterchangeError>;
    /// Qualified immutable helper identity.
    fn identity(&self) -> &AafToolchainIdentity;
}

/// Adjacent executable helper that passed an exact identity handshake.
#[derive(Debug, Clone)]
pub struct QualifiedAafProcessToolchain {
    executable: PathBuf,
    identity: AafToolchainIdentity,
}

impl QualifiedAafProcessToolchain {
    /// Qualify one helper executable before it can process project data.
    pub fn qualify(
        executable: impl AsRef<Path>,
        expected: &AafToolchainIdentity,
        limits: InterchangeLimits,
    ) -> Result<Self, InterchangeError> {
        limits.validate()?;
        expected.validate(limits.max_string_bytes)?;
        let executable = executable.as_ref().to_path_buf();
        let output = run_helper(&executable, "identity", None, limits)?;
        let actual: AafToolchainIdentity = serde_json::from_slice(&output).map_err(|error| {
            InterchangeError::AafHelperUnavailable {
                reason: format!("invalid identity response: {error}"),
            }
        })?;
        actual.validate(limits.max_string_bytes)?;
        if &actual != expected {
            return Err(InterchangeError::AafHelperUnavailable {
                reason: format!("expected {expected:?}, received {actual:?}"),
            });
        }
        Ok(Self { executable, identity: actual })
    }
}

impl AafToolchain for QualifiedAafProcessToolchain {
    fn decode_aaf(
        &self,
        input: &[u8],
        limits: InterchangeLimits,
    ) -> Result<Vec<u8>, InterchangeError> {
        run_helper(&self.executable, "decode", Some(input), limits)
    }
    fn encode_aaf(
        &self,
        bridge: &[u8],
        limits: InterchangeLimits,
    ) -> Result<Vec<u8>, InterchangeError> {
        run_helper(&self.executable, "encode", Some(bridge), limits)
    }
    fn identity(&self) -> &AafToolchainIdentity {
        &self.identity
    }
}

fn run_helper(
    executable: &Path,
    operation: &str,
    input: Option<&[u8]>,
    limits: InterchangeLimits,
) -> Result<Vec<u8>, InterchangeError> {
    let temp = tempfile::Builder::new().prefix("mondrian-aaf-").tempdir()?;
    let input_path = temp.path().join("input.bin");
    let output_path = temp.path().join("output.bin");
    let stderr_path = temp.path().join("stderr.txt");
    if let Some(input) = input {
        if input.len() > limits.max_bytes {
            return Err(InterchangeError::LimitExceeded {
                limit_name: "AAF helper input bytes",
                actual: input.len(),
                maximum: limits.max_bytes,
            });
        }
        fs::write(&input_path, input)?;
    }
    let stderr = fs::File::create(&stderr_path)?;
    let mut command = Command::new(executable);
    command
        .arg(format!("--{operation}"))
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr));
    if input.is_some() {
        command.arg("--input").arg(&input_path).arg("--output").arg(&output_path);
    } else {
        command.arg("--output").arg(&output_path);
    }
    let mut child = command
        .spawn()
        .map_err(|error| InterchangeError::AafHelperUnavailable { reason: error.to_string() })?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= Duration::from_millis(limits.helper_timeout_ms) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(InterchangeError::AafHelperFailed {
                reason: "deadline exceeded".to_owned(),
            });
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stderr = read_bounded(
        &stderr_path,
        limits.max_helper_output_bytes,
        "AAF helper stderr",
    )?;
    if !status.success() {
        return Err(InterchangeError::AafHelperFailed {
            reason: String::from_utf8_lossy(&stderr).into_owned(),
        });
    }
    read_bounded(&output_path, limits.max_bytes, "AAF helper output")
}

fn read_bounded(
    path: &Path,
    maximum: usize,
    name: &'static str,
) -> Result<Vec<u8>, InterchangeError> {
    let metadata = fs::metadata(path).map_err(|error| InterchangeError::AafHelperFailed {
        reason: format!("{name} missing: {error}"),
    })?;
    let actual = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    if actual > maximum {
        return Err(InterchangeError::LimitExceeded { limit_name: name, actual, maximum });
    }
    fs::read(path).map_err(InterchangeError::from)
}
