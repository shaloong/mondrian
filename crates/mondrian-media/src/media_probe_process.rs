//! Isolated, bounded execution for physical media probing.
//!
//! The worker process owns every potentially blocking FFmpeg call. The parent
//! retains cancellation and deadline authority through the shared supervised
//! process Module and accepts only one versioned, bounded response. No Project
//! or Asset Library capability crosses this process seam.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use mondrian_core::{ExecutionCancellationToken, MediaFileFingerprint, MediaProbeSnapshot};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::process_supervisor::{
    run_supervised_command, SupervisedProcessError, SupervisedProcessPolicy,
    SupervisedStreamCapture,
};

/// Hidden product-executable mode used by the isolated media Probe Helper.
pub const MEDIA_PROBE_WORKER_ARGUMENT: &str = "--internal-media-probe-worker-v1";

const MEDIA_PROBE_PROTOCOL_VERSION: u32 = 1;
const MEDIA_PROBE_STDOUT_LIMIT_BYTES: usize = 8 * 1024 * 1024;
const MEDIA_PROBE_STDERR_LIMIT_BYTES: usize = 64 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct MediaProbeWorkerEnvelope {
    schema_version: u32,
    result: MediaProbeWorkerResult,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum MediaProbeWorkerResult {
    Completed {
        prepared: Box<IsolatedMediaProbeSnapshot>,
    },
    Failed {
        detail: String,
    },
}

/// Immutable physical-source facts prepared by the isolated Probe Helper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IsolatedMediaProbeSnapshot {
    /// Canonical physical path observed inside the Helper.
    pub canonical_path: PathBuf,
    /// Complete conservative source revision observed around the probe.
    pub source_fingerprint: MediaFileFingerprint,
    /// Stable metadata contract produced from the same source revision.
    pub probe: MediaProbeSnapshot,
}

/// Typed failure from an isolated physical-media probe.
#[derive(Debug, Error)]
pub enum IsolatedMediaProbeError {
    /// Process creation, cancellation, deadline, pipe, or bounded-capture failure.
    #[error(transparent)]
    Supervisor(#[from] SupervisedProcessError),
    /// The Helper exited unsuccessfully before publishing a protocol response.
    #[error("media Probe Helper exited unsuccessfully ({status}): {detail}")]
    WorkerExit {
        /// Platform-formatted process exit status.
        status: String,
        /// Bounded stderr tail retained by the supervisor.
        detail: String,
    },
    /// The Helper completed the probe and returned a structured media failure.
    #[error("media Probe Helper rejected the source: {detail}")]
    ProbeFailed {
        /// Stable parent-side diagnostic detail.
        detail: String,
    },
    /// The Helper response was missing, malformed, or from another protocol.
    #[error("invalid media Probe Helper response: {detail}")]
    Protocol {
        /// Stable protocol rejection detail.
        detail: String,
    },
    /// The typed response was valid but a consuming domain rejected its facts.
    #[error("media Probe Helper snapshot was rejected: {detail}")]
    SnapshotRejected {
        /// Stable validation detail from the consuming domain.
        detail: String,
    },
}

impl IsolatedMediaProbeError {
    /// Return whether cancellation terminated the supervised Helper.
    pub fn is_canceled(&self) -> bool {
        matches!(self, Self::Supervisor(error) if error.is_canceled())
    }

    /// Return whether the admitted monotonic deadline terminated the Helper.
    pub fn is_deadline_exceeded(&self) -> bool {
        matches!(self, Self::Supervisor(error) if error.is_deadline_exceeded())
    }
}

/// Probe one physical media source in a supervised Helper process.
///
/// `helper_executable` must implement [`MEDIA_PROBE_WORKER_ARGUMENT`]. The
/// complete child process is killed and reaped when `cancellation` fires or
/// `deadline` elapses. Stdout is a strict versioned protocol channel; stderr
/// is a bounded diagnostic tail.
pub fn prepare_media_probe_isolated(
    helper_executable: &Path,
    path: &Path,
    cancellation: &ExecutionCancellationToken,
    deadline: Instant,
) -> Result<IsolatedMediaProbeSnapshot, IsolatedMediaProbeError> {
    let mut command = Command::new(helper_executable);
    command.arg(MEDIA_PROBE_WORKER_ARGUMENT).arg(path);
    run_probe_helper_command(&mut command, cancellation, deadline)
}

fn run_probe_helper_command(
    command: &mut Command,
    cancellation: &ExecutionCancellationToken,
    deadline: Instant,
) -> Result<IsolatedMediaProbeSnapshot, IsolatedMediaProbeError> {
    let output = run_supervised_command(
        command,
        None,
        SupervisedProcessPolicy {
            pipe_stdin: false,
            stdout: SupervisedStreamCapture::Head {
                limit_bytes: MEDIA_PROBE_STDOUT_LIMIT_BYTES,
                reject_excess: true,
            },
            stderr: SupervisedStreamCapture::Tail { limit_bytes: MEDIA_PROBE_STDERR_LIMIT_BYTES },
            deadline: Some(deadline),
            ..SupervisedProcessPolicy::default()
        },
        cancellation,
    )?;
    if !output.status.success() {
        return Err(IsolatedMediaProbeError::WorkerExit {
            status: output.status.to_string(),
            detail: bounded_worker_detail(&output.stderr),
        });
    }
    decode_worker_response(&output.stdout)
}

/// Run the isolated media Probe Helper mode in the current executable.
///
/// The caller must dispatch this before initializing the product UI. Exactly
/// one native path argument is accepted and exactly one JSON envelope is
/// written to stdout. Probe failures remain structured responses; protocol or
/// stdout failures terminate the Helper unsuccessfully.
pub fn run_media_probe_worker() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os();
    let _executable = arguments.next();
    anyhow::ensure!(
        arguments.next().as_deref() == Some(OsStr::new(MEDIA_PROBE_WORKER_ARGUMENT)),
        "invalid internal media Probe Helper mode"
    );
    let path = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("internal media Probe Helper requires one source path"))?;
    anyhow::ensure!(
        arguments.next().is_none(),
        "unexpected media Probe Helper argument"
    );

    let result = match prepare_worker_probe(&path) {
        Ok(prepared) => MediaProbeWorkerResult::Completed { prepared: Box::new(prepared) },
        Err(error) => MediaProbeWorkerResult::Failed { detail: error.to_string() },
    };
    write_worker_response(
        &mut io::stdout().lock(),
        &MediaProbeWorkerEnvelope {
            schema_version: MEDIA_PROBE_PROTOCOL_VERSION,
            result,
        },
    )?;
    Ok(())
}

fn write_worker_response(
    writer: &mut dyn Write,
    response: &MediaProbeWorkerEnvelope,
) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *writer, response)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn decode_worker_response(
    bytes: &[u8],
) -> Result<IsolatedMediaProbeSnapshot, IsolatedMediaProbeError> {
    let response: MediaProbeWorkerEnvelope =
        serde_json::from_slice(bytes).map_err(|error| IsolatedMediaProbeError::Protocol {
            detail: format!("response JSON is invalid: {error}"),
        })?;
    if response.schema_version != MEDIA_PROBE_PROTOCOL_VERSION {
        return Err(IsolatedMediaProbeError::Protocol {
            detail: format!(
                "expected schema {}, observed {}",
                MEDIA_PROBE_PROTOCOL_VERSION, response.schema_version
            ),
        });
    }
    match response.result {
        MediaProbeWorkerResult::Completed { prepared } => Ok(*prepared),
        MediaProbeWorkerResult::Failed { detail } => {
            Err(IsolatedMediaProbeError::ProbeFailed { detail })
        }
    }
}

fn prepare_worker_probe(path: &Path) -> anyhow::Result<IsolatedMediaProbeSnapshot> {
    let canonical_path = path.canonicalize()?;
    let source_fingerprint = MediaFileFingerprint::capture(&canonical_path);
    anyhow::ensure!(
        source_fingerprint.authorizes_reuse(),
        "media source does not expose complete conservative revision evidence"
    );
    let probe = crate::info::probe_media_info(&canonical_path)?;
    let verified_fingerprint = MediaFileFingerprint::capture(&canonical_path);
    anyhow::ensure!(
        verified_fingerprint.authorizes_reuse() && verified_fingerprint == source_fingerprint,
        "media source changed while the isolated probe was running"
    );
    Ok(IsolatedMediaProbeSnapshot { canonical_path, source_fingerprint, probe })
}

fn bounded_worker_detail(stderr: &[u8]) -> String {
    let detail = String::from_utf8_lossy(stderr).trim().to_owned();
    if detail.is_empty() {
        "no stderr detail".to_owned()
    } else {
        detail
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::thread;
    use std::time::Duration;

    const CHILD_MODE_ENV: &str = "MONDRIAN_MEDIA_PROBE_PROCESS_TEST_CHILD";

    fn child_command(mode: &str) -> Command {
        let executable = env::current_exe().expect("current test executable");
        let mut command = Command::new(executable);
        command
            .arg("--exact")
            .arg("media_probe_process::tests::probe_process_child_entry")
            .arg("--nocapture")
            .env(CHILD_MODE_ENV, mode);
        command
    }

    fn probe_fixture() -> IsolatedMediaProbeSnapshot {
        IsolatedMediaProbeSnapshot {
            canonical_path: PathBuf::from("test.mov"),
            source_fingerprint: MediaFileFingerprint::default(),
            probe: MediaProbeSnapshot {
                duration: std::time::Duration::ZERO,
                file_size: 0,
                container: "test".to_owned(),
                video_streams: Vec::new(),
                audio_streams: Vec::new(),
                has_video: false,
                has_audio: false,
            },
        }
    }

    #[test]
    fn protocol_round_trips_one_complete_probe() {
        let expected = probe_fixture();
        let bytes = serde_json::to_vec(&MediaProbeWorkerEnvelope {
            schema_version: MEDIA_PROBE_PROTOCOL_VERSION,
            result: MediaProbeWorkerResult::Completed { prepared: Box::new(expected.clone()) },
        })
        .expect("serialize response");

        let observed = decode_worker_response(&bytes).expect("decode response");

        assert_eq!(observed, expected);
    }

    #[test]
    fn protocol_preserves_probe_failure_without_fabricating_snapshot() {
        let bytes = serde_json::to_vec(&MediaProbeWorkerEnvelope {
            schema_version: MEDIA_PROBE_PROTOCOL_VERSION,
            result: MediaProbeWorkerResult::Failed { detail: "unsupported source".to_owned() },
        })
        .expect("serialize response");

        let error = decode_worker_response(&bytes).expect_err("probe failure must remain terminal");

        assert!(matches!(
            error,
            IsolatedMediaProbeError::ProbeFailed { detail } if detail == "unsupported source"
        ));
    }

    #[test]
    fn protocol_rejects_another_schema_and_trailing_payload() {
        let wrong_schema = serde_json::to_vec(&MediaProbeWorkerEnvelope {
            schema_version: MEDIA_PROBE_PROTOCOL_VERSION + 1,
            result: MediaProbeWorkerResult::Completed { prepared: Box::new(probe_fixture()) },
        })
        .expect("serialize response");
        assert!(matches!(
            decode_worker_response(&wrong_schema),
            Err(IsolatedMediaProbeError::Protocol { .. })
        ));

        let mut trailing = wrong_schema;
        trailing.extend_from_slice(b"{}\n");
        assert!(matches!(
            decode_worker_response(&trailing),
            Err(IsolatedMediaProbeError::Protocol { .. })
        ));
    }

    #[test]
    fn probe_process_child_entry() {
        let Ok(mode) = env::var(CHILD_MODE_ENV) else {
            return;
        };
        match mode.as_str() {
            "sleep" => thread::sleep(Duration::from_secs(30)),
            other => panic!("unknown media probe child mode {other}"),
        }
    }

    #[test]
    fn parent_cancellation_terminates_and_reaps_probe_helper() {
        let mut command = child_command("sleep");
        let cancellation = ExecutionCancellationToken::new();
        let trigger = cancellation.clone();
        let cancel_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            trigger.cancel();
        });
        let started = Instant::now();

        let error = run_probe_helper_command(
            &mut command,
            &cancellation,
            Instant::now() + Duration::from_secs(10),
        )
        .expect_err("cancellation must terminate the Probe Helper");
        cancel_thread.join().expect("cancellation trigger");

        assert!(error.is_canceled());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn monotonic_deadline_terminates_and_reaps_probe_helper() {
        let mut command = child_command("sleep");
        let started = Instant::now();

        let error = run_probe_helper_command(
            &mut command,
            &ExecutionCancellationToken::new(),
            Instant::now() + Duration::from_millis(40),
        )
        .expect_err("deadline must terminate the Probe Helper");

        assert!(error.is_deadline_exceeded());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
