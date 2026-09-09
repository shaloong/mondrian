//! Independent bounded verification for a finished single-file export artifact.

use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mondrian_core::ExecutionCancellationToken;
use mondrian_media::{
    run_supervised_command, SupervisedProcessError, SupervisedProcessPolicy,
    SupervisedStreamCapture,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::validator::{probe_export_output_until, ExportOutputProbe};

/// Stable implementation identity bound into every independent report.
pub const INDEPENDENT_EXPORT_ARTIFACT_VALIDATOR_ID: &str =
    "mondrian-export-independent-full-decode-v2";

const HASH_STDOUT_LIMIT: usize = 256;
const PROGRESS_STDERR_LIMIT: usize = 64 * 1024;

/// Explicit resource limits for one independent artifact verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndependentExportArtifactPolicy {
    /// Largest regular file admitted for hashing and full decode.
    maximum_artifact_bytes: u64,
    /// Maximum wall time for snapshot, probes, full decode, identity recheck and cleanup.
    decode_timeout: Duration,
}

impl IndependentExportArtifactPolicy {
    /// Construct a nonzero bounded policy.
    pub fn new(
        maximum_artifact_bytes: u64,
        decode_timeout: Duration,
    ) -> Result<Self, IndependentExportArtifactVerificationError> {
        if maximum_artifact_bytes == 0 {
            return Err(IndependentExportArtifactVerificationError::InvalidPolicy(
                "maximum artifact bytes must be nonzero",
            ));
        }
        if decode_timeout.is_zero() {
            return Err(IndependentExportArtifactVerificationError::InvalidPolicy(
                "decode timeout must be nonzero",
            ));
        }
        Ok(Self { maximum_artifact_bytes, decode_timeout })
    }

    /// Largest regular file admitted for hashing and full decode.
    pub const fn maximum_artifact_bytes(self) -> u64 {
        self.maximum_artifact_bytes
    }

    /// Maximum wall time for the complete verification and consuming cleanup.
    pub const fn decode_timeout(self) -> Duration {
        self.decode_timeout
    }
}

/// Canonical structured evidence produced after complete independent decode.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndependentExportArtifactReport {
    /// Evidence schema version.
    pub schema_version: u32,
    /// Stable verifier implementation identity.
    pub validator_id: &'static str,
    /// Stable caller-owned Export job/artifact identity covered by this report.
    pub artifact_id: String,
    /// Exact artifact size observed before and after validation.
    pub artifact_bytes: u64,
    /// SHA-256 of the encoded artifact bytes.
    pub artifact_sha256: String,
    /// Existing typed container/stream/opening-frame evidence.
    pub probe: ExportOutputProbe,
    /// Whether the full decode included a video stream.
    pub decoded_video: bool,
    /// Whether the full decode included an audio stream.
    pub decoded_audio: bool,
    /// Final video-frame count reported by the decoding FFmpeg process.
    pub decoded_video_frames: u64,
    /// Final decoded output time reported by FFmpeg.
    pub decoded_duration_us: u64,
    /// SHA-256 emitted by FFmpeg over all decoded output stream bytes.
    pub decoded_content_sha256: String,
    /// The exact encoded snapshot was fallibly consumed before success publication.
    pub snapshot_removed: bool,
}

/// Self-contained report plus its canonical JSON digest.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndependentExportArtifactReceipt {
    /// Complete independently derived report.
    report: IndependentExportArtifactReport,
    /// SHA-256 of the canonical serialized report bytes.
    validation_report_sha256: String,
    /// Raw bounded native output retained independently of the deterministic report digest.
    decode_execution: IndependentArtifactNativeObservation,
}

impl IndependentExportArtifactReceipt {
    /// Complete independently derived report.
    pub const fn report(&self) -> &IndependentExportArtifactReport {
        &self.report
    }

    /// SHA-256 of the canonical serialized report bytes.
    pub fn validation_report_sha256(&self) -> &str {
        &self.validation_report_sha256
    }

    /// Serialize the full success, including native stdout/stderr and cleanup.
    pub fn evidence(&self) -> &Self {
        self
    }

    /// Original native decoder output and consuming process-owner cleanup.
    pub const fn native_execution(&self) -> &IndependentArtifactNativeObservation {
        &self.decode_execution
    }
}

/// Original native output and consuming cleanup observed before validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndependentArtifactNativeObservation {
    /// Actual native exit status.
    pub exit_status: String,
    /// Bounded complete stdout.
    pub stdout: Vec<u8>,
    /// Bounded complete stderr/progress output.
    pub stderr: Vec<u8>,
    /// Whether stdout exceeded its bound.
    pub stdout_truncated: bool,
    /// Whether stderr exceeded its bound.
    pub stderr_truncated: bool,
    /// Original child and pipe-owner closure facts.
    pub cleanup: mondrian_media::SupervisedProcessCleanupReceipt,
}

/// Independently serializable failure; absent fields were not observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndependentExportArtifactFailureEvidence {
    /// Failure evidence schema.
    pub schema_version: u32,
    /// Full typed error chain rendered for diagnostics.
    pub error: String,
    /// Raw decode output when the native supervisor returned it.
    pub decode_execution: Option<IndependentArtifactNativeObservation>,
    /// Actual native output from a rejected or malformed probe, when available.
    pub probe_execution: Option<IndependentArtifactNativeObservation>,
    /// Raw cleanup from a failed probe or decoder, even without completed output.
    pub child_cleanup: Option<mondrian_media::SupervisedProcessCleanupReceipt>,
    /// Whether this verifier acquired a snapshot requiring consuming cleanup.
    pub snapshot_admitted: bool,
    /// Whether the owned snapshot was actually removed.
    pub snapshot_removed: bool,
    /// Original fallible snapshot removal error.
    pub snapshot_cleanup_error: Option<String>,
}

/// Independently reopen, probe, fully decode, and rehash one finished artifact.
pub fn verify_export_artifact(
    path: &Path,
    artifact_id: impl Into<String>,
    policy: IndependentExportArtifactPolicy,
) -> Result<IndependentExportArtifactReceipt, IndependentExportArtifactVerificationError> {
    verify_export_artifact_cancellable(
        path,
        artifact_id,
        policy,
        &ExecutionCancellationToken::new(),
    )
}

/// Independently verify one artifact while retaining caller cancellation authority.
pub fn verify_export_artifact_cancellable(
    path: &Path,
    artifact_id: impl Into<String>,
    policy: IndependentExportArtifactPolicy,
    cancellation: &ExecutionCancellationToken,
) -> Result<IndependentExportArtifactReceipt, IndependentExportArtifactVerificationError> {
    let deadline = Instant::now()
        .checked_add(policy.decode_timeout)
        .ok_or(IndependentExportArtifactVerificationError::DeadlineOverflow)?;
    verify_export_artifact_until(
        path,
        artifact_id,
        policy.maximum_artifact_bytes,
        deadline,
        cancellation,
    )
}

/// Verify and consume one artifact snapshot under the caller's original deadline.
pub fn verify_export_artifact_until(
    path: &Path,
    artifact_id: impl Into<String>,
    maximum_artifact_bytes: u64,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> Result<IndependentExportArtifactReceipt, IndependentExportArtifactVerificationError> {
    let artifact_id = artifact_id.into();
    if artifact_id.is_empty() || artifact_id.len() > 256 {
        return Err(IndependentExportArtifactVerificationError::InvalidArtifactId);
    }
    if maximum_artifact_bytes == 0 {
        return Err(IndependentExportArtifactVerificationError::InvalidPolicy(
            "maximum artifact bytes must be nonzero",
        ));
    }
    let snapshot = snapshot_artifact_until(path, maximum_artifact_bytes, cancellation, deadline)?;
    let artifact_bytes = snapshot.bytes;
    let artifact_sha256 = snapshot.sha256.clone();
    let snapshot_path = snapshot.file.into_temp_path();
    let mut decode_execution = None;
    let outcome = (|| -> Result<_, IndependentExportArtifactVerificationError> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let _snapshot_lease = options
            .open(&snapshot_path)
            .map_err(IndependentExportArtifactVerificationError::SnapshotIo)?;
        if hash_published_artifact_until(
            &snapshot_path,
            maximum_artifact_bytes,
            cancellation,
            deadline,
        )? != (artifact_bytes, artifact_sha256.clone())
        {
            return Err(IndependentExportArtifactVerificationError::ArtifactChanged);
        }
        let probe = probe_export_output_until(&snapshot_path, cancellation, deadline)
            .map_err(IndependentExportArtifactVerificationError::Probe)?;
        let decoded_video = probe.video.is_some();
        let decoded_audio = probe.audio.is_some();
        if !decoded_video && !decoded_audio {
            return Err(IndependentExportArtifactVerificationError::NoDecodableStreams);
        }
        let decode = full_decode(
            &snapshot_path,
            deadline,
            cancellation,
            &mut decode_execution,
        )?;
        if decoded_video && decode.video_frames == 0 {
            return Err(IndependentExportArtifactVerificationError::MissingVideoFrames);
        }
        if probe.duration_secs.is_some_and(|duration| duration > 0.0) && decode.duration_us == 0 {
            return Err(IndependentExportArtifactVerificationError::MissingDecodeDuration);
        }
        if let Some(expected_duration_secs) = probe.duration_secs.filter(|duration| *duration > 0.0)
        {
            let decoded_duration_secs = decode.duration_us as f64 / 1_000_000.0;
            let tolerance = expected_duration_secs.mul_add(0.005, 0.05).max(0.05);
            if (decoded_duration_secs - expected_duration_secs).abs() > tolerance {
                return Err(
                    IndependentExportArtifactVerificationError::DecodeDurationMismatch {
                        expected_secs: expected_duration_secs,
                        decoded_secs: decoded_duration_secs,
                        tolerance_secs: tolerance,
                    },
                );
            }
        }
        let expected_identity = (artifact_bytes, artifact_sha256.clone());
        if hash_published_artifact_until(path, maximum_artifact_bytes, cancellation, deadline)?
            != expected_identity
            || hash_published_artifact_until(
                &snapshot_path,
                maximum_artifact_bytes,
                cancellation,
                deadline,
            )? != expected_identity
        {
            return Err(IndependentExportArtifactVerificationError::ArtifactChanged);
        }
        Ok(IndependentExportArtifactReport {
            schema_version: 2,
            validator_id: INDEPENDENT_EXPORT_ARTIFACT_VALIDATOR_ID,
            artifact_id,
            artifact_bytes,
            artifact_sha256,
            probe,
            decoded_video,
            decoded_audio,
            decoded_video_frames: decode.video_frames,
            decoded_duration_us: decode.duration_us,
            decoded_content_sha256: decode.content_sha256,
            snapshot_removed: false,
        })
    })();
    let cleanup = snapshot_path.close();
    let snapshot_removed = cleanup.is_ok();
    let snapshot_cleanup_error = cleanup.err().map(|error| error.to_string());
    let outcome = outcome.and_then(|mut report| {
        check_artifact_boundary(cancellation, Some(deadline))?;
        if let Some(error) = &snapshot_cleanup_error {
            return Err(IndependentExportArtifactVerificationError::SnapshotIo(
                io::Error::other(error.clone()),
            ));
        }
        report.snapshot_removed = true;
        let report_bytes = serde_json::to_vec(&report)
            .map_err(IndependentExportArtifactVerificationError::SerializeReport)?;
        let validation_report_sha256 = format!("{:x}", Sha256::digest(&report_bytes));
        let decode_execution = decode_execution
            .clone()
            .ok_or(IndependentExportArtifactVerificationError::MissingNativeObservation)?;
        check_artifact_boundary(cancellation, Some(deadline))?;
        Ok(IndependentExportArtifactReceipt { report, validation_report_sha256, decode_execution })
    });
    outcome.map_err(|source| {
        let mut evidence = source.evidence();
        evidence.decode_execution = decode_execution.clone();
        if evidence.child_cleanup.is_none() {
            evidence.child_cleanup = decode_execution.as_ref().map(|native| native.cleanup.clone());
        }
        evidence.snapshot_removed = snapshot_removed;
        evidence.snapshot_admitted = true;
        evidence.snapshot_cleanup_error = snapshot_cleanup_error;
        IndependentExportArtifactVerificationError::Verification {
            source: Box::new(source),
            evidence: Box::new(evidence),
        }
    })
}
pub(crate) struct ImmutableArtifactSnapshot {
    pub(crate) file: tempfile::NamedTempFile,
    pub(crate) bytes: u64,
    pub(crate) sha256: String,
}

pub(crate) fn snapshot_artifact_until(
    path: &Path,
    maximum_bytes: u64,
    cancellation: &ExecutionCancellationToken,
    deadline: Instant,
) -> Result<ImmutableArtifactSnapshot, IndependentExportArtifactVerificationError> {
    snapshot_artifact_inner(path, maximum_bytes, cancellation, Some(deadline))
}

fn snapshot_artifact_inner(
    path: &Path,
    maximum_bytes: u64,
    cancellation: &ExecutionCancellationToken,
    deadline: Option<Instant>,
) -> Result<ImmutableArtifactSnapshot, IndependentExportArtifactVerificationError> {
    check_artifact_boundary(cancellation, deadline)?;
    require_direct_regular_file(path)?;
    let source = File::open(path).map_err(|source| {
        IndependentExportArtifactVerificationError::Metadata { path: path.to_path_buf(), source }
    })?;
    if !source
        .metadata()
        .map_err(
            |source| IndependentExportArtifactVerificationError::Metadata {
                path: path.to_path_buf(),
                source,
            },
        )?
        .is_file()
    {
        return Err(IndependentExportArtifactVerificationError::NotRegularFile(
            path.to_path_buf(),
        ));
    }
    let mut snapshot = tempfile::Builder::new()
        .prefix("mondrian-export-verify-")
        .tempfile()
        .map_err(IndependentExportArtifactVerificationError::SnapshotIo)?;
    let outcome = (|| {
        let (bytes, sha256) = copy_and_hash_bounded(
            BufReader::new(source),
            snapshot.as_file_mut(),
            maximum_bytes,
            cancellation,
            deadline,
        )?;
        snapshot
            .as_file_mut()
            .flush()
            .map_err(IndependentExportArtifactVerificationError::SnapshotIo)?;
        require_direct_regular_file(path)?;
        if bytes == 0 {
            return Err(IndependentExportArtifactVerificationError::EmptyArtifact(
                path.to_path_buf(),
            ));
        }
        check_artifact_boundary(cancellation, deadline)?;
        Ok((bytes, sha256))
    })();
    match outcome {
        Ok((bytes, sha256)) => Ok(ImmutableArtifactSnapshot { file: snapshot, bytes, sha256 }),
        Err(primary) => match snapshot.close() {
            Ok(()) => Err(IndependentExportArtifactVerificationError::SnapshotClosed {
                primary: Box::new(primary),
            }),
            Err(cleanup) => Err(
                IndependentExportArtifactVerificationError::SnapshotCleanup {
                    primary: Box::new(primary),
                    cleanup,
                },
            ),
        },
    }
}

pub(crate) fn hash_published_artifact_until(
    path: &Path,
    maximum_bytes: u64,
    cancellation: &ExecutionCancellationToken,
    deadline: Instant,
) -> Result<(u64, String), IndependentExportArtifactVerificationError> {
    hash_published_artifact_inner(path, maximum_bytes, cancellation, Some(deadline))
}

fn hash_published_artifact_inner(
    path: &Path,
    maximum_bytes: u64,
    cancellation: &ExecutionCancellationToken,
    deadline: Option<Instant>,
) -> Result<(u64, String), IndependentExportArtifactVerificationError> {
    check_artifact_boundary(cancellation, deadline)?;
    require_direct_regular_file(path)?;
    let source = File::open(path).map_err(|source| {
        IndependentExportArtifactVerificationError::Metadata { path: path.to_path_buf(), source }
    })?;
    if !source
        .metadata()
        .map_err(
            |source| IndependentExportArtifactVerificationError::Metadata {
                path: path.to_path_buf(),
                source,
            },
        )?
        .is_file()
    {
        return Err(IndependentExportArtifactVerificationError::NotRegularFile(
            path.to_path_buf(),
        ));
    }
    let result = copy_and_hash_bounded(
        BufReader::new(source),
        std::io::sink(),
        maximum_bytes,
        cancellation,
        deadline,
    )?;
    require_direct_regular_file(path)?;
    check_artifact_boundary(cancellation, deadline)?;
    Ok(result)
}

fn require_direct_regular_file(
    path: &Path,
) -> Result<(), IndependentExportArtifactVerificationError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| {
        IndependentExportArtifactVerificationError::Metadata { path: path.to_path_buf(), source }
    })?;
    if !metadata.file_type().is_file() {
        return Err(IndependentExportArtifactVerificationError::NotRegularFile(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

fn copy_and_hash_bounded(
    mut source: impl Read,
    mut destination: impl Write,
    maximum_bytes: u64,
    cancellation: &ExecutionCancellationToken,
    deadline: Option<Instant>,
) -> Result<(u64, String), IndependentExportArtifactVerificationError> {
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        check_artifact_boundary(cancellation, deadline)?;
        let remaining = maximum_bytes.saturating_sub(total);
        let read_limit = if remaining >= buffer.len() as u64 {
            buffer.len()
        } else {
            remaining as usize + 1
        };
        let read = source
            .read(&mut buffer[..read_limit])
            .map_err(IndependentExportArtifactVerificationError::SnapshotIo)?;
        if read == 0 {
            break;
        }
        let read_u64 = read as u64;
        let actual = total.saturating_add(read_u64);
        if actual > maximum_bytes {
            return Err(
                IndependentExportArtifactVerificationError::ArtifactTooLarge {
                    actual,
                    maximum: maximum_bytes,
                },
            );
        }
        destination
            .write_all(&buffer[..read])
            .map_err(IndependentExportArtifactVerificationError::SnapshotIo)?;
        digest.update(&buffer[..read]);
        total = actual;
    }
    check_artifact_boundary(cancellation, deadline)?;
    Ok((total, format!("{:x}", digest.finalize())))
}

fn check_artifact_boundary(
    cancellation: &ExecutionCancellationToken,
    deadline: Option<Instant>,
) -> Result<(), IndependentExportArtifactVerificationError> {
    if cancellation.is_canceled() {
        return Err(IndependentExportArtifactVerificationError::Cancelled);
    }
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(IndependentExportArtifactVerificationError::DeadlineExceeded);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FullDecodeEvidence {
    video_frames: u64,
    duration_us: u64,
    content_sha256: String,
}

fn full_decode(
    path: &Path,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
    observed: &mut Option<IndependentArtifactNativeObservation>,
) -> Result<FullDecodeEvidence, IndependentExportArtifactVerificationError> {
    check_artifact_boundary(cancellation, Some(deadline))?;
    let mut command = mondrian_media::ffmpeg_command()
        .map_err(IndependentExportArtifactVerificationError::CommandAdmission)?;
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-xerror",
            "-err_detect",
            "explode",
            "-nostats",
            "-stats_period",
            "86400",
            "-progress",
            "pipe:2",
            "-i",
        ])
        .arg(path)
        .args([
            "-map", "0:v?", "-map", "0:a?", "-sn", "-dn", "-f", "hash", "-hash", "sha256", "pipe:1",
        ]);
    let policy = SupervisedProcessPolicy {
        pipe_stdin: false,
        stdout: SupervisedStreamCapture::Head {
            limit_bytes: HASH_STDOUT_LIMIT,
            reject_excess: true,
        },
        stderr: SupervisedStreamCapture::Head {
            limit_bytes: PROGRESS_STDERR_LIMIT,
            reject_excess: true,
        },
        deadline: Some(deadline),
        ..SupervisedProcessPolicy::default()
    };
    let output = run_supervised_command(&mut command, None, policy, cancellation)
        .map_err(IndependentExportArtifactVerificationError::DecodeProcess)?;
    *observed = Some(IndependentArtifactNativeObservation {
        exit_status: output.status.to_string(),
        stdout: output.stdout.clone(),
        stderr: output.stderr.clone(),
        stdout_truncated: output.stdout_truncated,
        stderr_truncated: output.stderr_truncated,
        cleanup: output.cleanup.clone(),
    });
    if !output.cleanup.all_resources_released() {
        return Err(IndependentExportArtifactVerificationError::UnsettledNativeOwner);
    }
    if !output.status.success() {
        return Err(IndependentExportArtifactVerificationError::DecodeFailed {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let content_sha256 = parse_hash_output(&output.stdout)?;
    let (video_frames, duration_us) = parse_final_progress(&output.stderr)?;
    check_artifact_boundary(cancellation, Some(deadline))?;
    Ok(FullDecodeEvidence { video_frames, duration_us, content_sha256 })
}

fn parse_hash_output(bytes: &[u8]) -> Result<String, IndependentExportArtifactVerificationError> {
    let raw = std::str::from_utf8(bytes)
        .map_err(|_| IndependentExportArtifactVerificationError::InvalidHashOutput)?
        .trim();
    let digest = raw
        .strip_prefix("SHA256=")
        .ok_or(IndependentExportArtifactVerificationError::InvalidHashOutput)?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(IndependentExportArtifactVerificationError::InvalidHashOutput);
    }
    Ok(digest.to_ascii_lowercase())
}

fn parse_final_progress(
    bytes: &[u8],
) -> Result<(u64, u64), IndependentExportArtifactVerificationError> {
    let progress = std::str::from_utf8(bytes)
        .map_err(|_| IndependentExportArtifactVerificationError::InvalidProgress)?;
    let mut block_frame = None;
    let mut block_duration_us = None;
    let mut terminal = None;
    for line in progress.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        match key {
            "frame" => {
                block_frame = value.trim().parse::<u64>().ok();
            }
            "out_time_us" => {
                block_duration_us = value.trim().parse::<u64>().ok();
            }
            "progress" if value.trim() == "end" => {
                if terminal.is_some() {
                    return Err(IndependentExportArtifactVerificationError::InvalidProgress);
                }
                let duration_us = block_duration_us
                    .ok_or(IndependentExportArtifactVerificationError::InvalidProgress)?;
                terminal = Some((block_frame.unwrap_or(0), duration_us));
            }
            "progress" if value.trim() == "continue" => {
                block_frame = None;
                block_duration_us = None;
            }
            _ => {}
        }
    }
    terminal.ok_or(IndependentExportArtifactVerificationError::InvalidProgress)
}

/// Stable independent-verification failure.
#[derive(Debug, Error)]
pub enum IndependentExportArtifactVerificationError {
    /// Verification failed after snapshot admission; all consuming evidence is retained.
    #[error("{source}")]
    Verification {
        /// Original operation failure.
        #[source]
        source: Box<IndependentExportArtifactVerificationError>,
        /// Independent original native and filesystem closure facts.
        evidence: Box<IndependentExportArtifactFailureEvidence>,
    },
    /// Native output was not accompanied by successful consuming cleanup.
    #[error("independent artifact native decoder ownership did not settle")]
    UnsettledNativeOwner,
    /// Internal evidence was absent after an otherwise successful native decode.
    #[error("independent artifact native observation is missing")]
    MissingNativeObservation,
    /// Policy omitted a hard resource bound.
    #[error("invalid independent export verification policy: {0}")]
    InvalidPolicy(&'static str),
    /// The caller did not provide one bounded stable artifact identity.
    #[error("independent export artifact identity must contain 1..=256 bytes")]
    InvalidArtifactId,
    /// Artifact metadata could not be read.
    #[error("cannot inspect export artifact {path}: {source}")]
    Metadata {
        /// Artifact path.
        path: PathBuf,
        /// Filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// Artifact is a directory, link, or another non-regular file.
    #[error("export artifact is not a regular file: {0}")]
    NotRegularFile(PathBuf),
    /// Artifact contains no bytes.
    #[error("export artifact is empty: {0}")]
    EmptyArtifact(PathBuf),
    /// Artifact exceeded the admitted byte bound.
    #[error("export artifact has {actual} bytes, exceeding the {maximum}-byte verification bound")]
    ArtifactTooLarge {
        /// Minimum byte count observed before the verifier stopped reading.
        actual: u64,
        /// Maximum byte count admitted by the policy.
        maximum: u64,
    },
    /// Creating, filling, or hashing the verifier-owned snapshot failed.
    #[error("independent export artifact snapshot failed: {0}")]
    SnapshotIo(#[source] std::io::Error),
    /// Caller cancellation interrupted snapshot or post-decode hashing.
    #[error("independent export artifact verification was cancelled")]
    Cancelled,
    /// Existing typed probe failed.
    #[error("export artifact probe failed: {0}")]
    Probe(#[source] crate::validator::ExportValidationError),
    /// Exact executable admission failed before any decode child was created.
    #[error("independent decode command admission failed: {0}")]
    CommandAdmission(#[source] mondrian_media::FfmpegCommandError),
    /// Probe found no selected media streams.
    #[error("export artifact has no independently decodable video or audio stream")]
    NoDecodableStreams,
    /// Decode deadline could not be represented.
    #[error("independent export decode deadline overflowed")]
    DeadlineOverflow,
    /// Original end-to-end artifact verification deadline expired.
    #[error("independent export artifact original deadline exceeded")]
    DeadlineExceeded,
    /// Snapshot preparation failed and its consuming removal also failed.
    #[error("artifact snapshot failed: {primary}; consuming removal failed: {cleanup}")]
    SnapshotCleanup {
        /// Original operation failure.
        #[source]
        primary: Box<IndependentExportArtifactVerificationError>,
        /// Independent removal failure.
        cleanup: std::io::Error,
    },
    /// Preparation failed after acquiring a snapshot; its exact removal succeeded.
    #[error("artifact snapshot preparation failed after successful consuming removal: {primary}")]
    SnapshotClosed {
        /// Original preparation failure.
        #[source]
        primary: Box<IndependentExportArtifactVerificationError>,
    },
    /// Supervised FFmpeg execution failed or exceeded its bound.
    #[error("independent export decode process failed: {0}")]
    DecodeProcess(#[source] SupervisedProcessError),
    /// FFmpeg returned a non-success terminal status.
    #[error("independent export decode failed ({status}): {stderr}")]
    DecodeFailed {
        /// FFmpeg process exit status.
        status: String,
        /// Bounded FFmpeg diagnostic output.
        stderr: String,
    },
    /// FFmpeg hash output was absent or malformed.
    #[error("independent export decode emitted invalid SHA-256 evidence")]
    InvalidHashOutput,
    /// FFmpeg did not publish one complete terminal progress block.
    #[error("independent export decode emitted invalid terminal progress evidence")]
    InvalidProgress,
    /// A probed video stream decoded no frames.
    #[error("independent export decode produced no video frames")]
    MissingVideoFrames,
    /// A positive-duration artifact produced no decoded duration evidence.
    #[error("independent export decode produced no duration evidence")]
    MissingDecodeDuration,
    /// The complete decode duration disagreed with the independently probed duration.
    #[error(
        "independent export decode duration {decoded_secs:.6}s differs from probed {expected_secs:.6}s beyond {tolerance_secs:.6}s"
    )]
    DecodeDurationMismatch {
        /// Duration derived by the typed output probe.
        expected_secs: f64,
        /// Terminal duration derived by the full decode process.
        decoded_secs: f64,
        /// Explicit comparison tolerance.
        tolerance_secs: f64,
    },
    /// Artifact bytes changed while independent verification was running.
    #[error("export artifact changed during independent verification")]
    ArtifactChanged,
    /// Structured evidence could not be serialized.
    #[error("cannot serialize independent export verification report: {0}")]
    SerializeReport(#[source] serde_json::Error),
}

impl IndependentExportArtifactVerificationError {
    /// Serialize observed failure facts without converting native cleanup to prose.
    pub fn evidence(&self) -> IndependentExportArtifactFailureEvidence {
        if let Self::Verification { evidence, .. } = self {
            return (**evidence).clone();
        }
        let mut result = IndependentExportArtifactFailureEvidence {
            schema_version: 1,
            error: self.to_string(),
            decode_execution: None,
            probe_execution: None,
            child_cleanup: None,
            snapshot_admitted: false,
            snapshot_removed: false,
            snapshot_cleanup_error: None,
        };
        let mut current: Option<&(dyn std::error::Error + 'static)> = Some(self);
        while let Some(error) = current {
            if let Some(crate::validator::ExportValidationError::ProbeOutput { output, .. }) =
                error.downcast_ref::<crate::validator::ExportValidationError>()
            {
                result.child_cleanup = Some(output.cleanup.clone());
                result.probe_execution = Some(IndependentArtifactNativeObservation {
                    exit_status: output.status.to_string(),
                    stdout: output.stdout.clone(),
                    stderr: output.stderr.clone(),
                    stdout_truncated: output.stdout_truncated,
                    stderr_truncated: output.stderr_truncated,
                    cleanup: output.cleanup.clone(),
                });
            }
            if let Some(SupervisedProcessError::Cleanup { cleanup, .. }) =
                error.downcast_ref::<SupervisedProcessError>()
            {
                result.child_cleanup = Some(cleanup.as_ref().clone());
            }
            if let Some(Self::SnapshotCleanup { cleanup, .. }) = error.downcast_ref::<Self>() {
                result.snapshot_admitted = true;
                result.snapshot_cleanup_error = Some(cleanup.to_string());
            }
            if let Some(Self::SnapshotClosed { .. }) = error.downcast_ref::<Self>() {
                result.snapshot_admitted = true;
                result.snapshot_removed = true;
            }
            current = error.source();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_artifact_deadline_precedes_snapshot_or_source_open() {
        let root = tempfile::tempdir().expect("fixture root");
        let absent = root.path().join("must-not-be-opened.mp4");
        let cancellation = ExecutionCancellationToken::new();
        let deadline = Instant::now();
        let failure =
            verify_export_artifact_until(&absent, "expired", 1024, deadline, &cancellation)
                .expect_err("original deadline must precede source open");
        assert!(matches!(
            failure,
            IndependentExportArtifactVerificationError::DeadlineExceeded
        ));
        assert!(!failure.evidence().snapshot_admitted);
        assert!(!failure.evidence().snapshot_removed);
        assert!(failure.evidence().child_cleanup.is_none());
        assert!(matches!(
            snapshot_artifact_until(&absent, 1024, &cancellation, deadline),
            Err(IndependentExportArtifactVerificationError::DeadlineExceeded)
        ));
        assert!(matches!(
            hash_published_artifact_until(&absent, 1024, &cancellation, deadline),
            Err(IndependentExportArtifactVerificationError::DeadlineExceeded)
        ));
        assert_eq!(
            std::fs::read_dir(root.path()).expect("root inventory").count(),
            0
        );
    }

    #[test]
    fn copy_checks_original_deadline_after_slow_read() {
        struct SlowRead;
        impl Read for SlowRead {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                std::thread::sleep(Duration::from_millis(15));
                Ok(0)
            }
        }
        let deadline = Instant::now() + Duration::from_millis(5);
        assert!(matches!(
            copy_and_hash_bounded(
                SlowRead,
                std::io::sink(),
                1024,
                &ExecutionCancellationToken::new(),
                Some(deadline)
            ),
            Err(IndependentExportArtifactVerificationError::DeadlineExceeded)
        ));
    }

    #[test]
    fn independent_invalid_media_retains_probe_native_and_snapshot_closure() {
        let root = tempfile::tempdir().expect("source root");
        let path = root.path().join("invalid.mp4");
        std::fs::write(&path, b"not an encoded media file").expect("invalid fixture");
        let evidence = verify_export_artifact_until(
            &path,
            "invalid-media",
            1024,
            Instant::now() + Duration::from_secs(10),
            &ExecutionCancellationToken::new(),
        )
        .expect_err("real ffprobe must reject invalid bytes")
        .evidence();
        assert!(evidence.snapshot_admitted);
        assert!(evidence.snapshot_removed);
        assert!(evidence.snapshot_cleanup_error.is_none());
        assert!(evidence.decode_execution.is_none());
        let native = evidence.probe_execution.expect("real completed probe output");
        assert!(!native.stderr.is_empty());
        assert!(native.cleanup.all_resources_released());
        assert_eq!(evidence.child_cleanup, Some(native.cleanup));
    }

    #[test]
    fn parser_requires_exact_hash_and_terminal_progress() {
        assert_eq!(
            parse_hash_output(
                b"SHA256=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n"
            )
            .expect("hash"),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert!(parse_hash_output(b"SHA256=abcd\n").is_err());
        assert_eq!(
            parse_final_progress(
                b"frame=1\nout_time_us=100\nprogress=continue\nframe=5\nout_time_us=900\nprogress=end\n"
            )
            .expect("terminal progress"),
            (5, 900)
        );
        assert!(parse_final_progress(b"frame=5\nprogress=continue\n").is_err());
        assert!(parse_final_progress(
            b"frame=1\nout_time_us=100\nprogress=continue\nframe=5\nprogress=end\n"
        )
        .is_err());
    }

    #[test]
    fn independent_verifier_reopens_and_fully_decodes_real_media() {
        let root = tempfile::tempdir().expect("temp root");
        let path = root.path().join("artifact.mp4");
        let status = mondrian_media::ffmpeg_command()
            .expect("admit fixture FFmpeg command")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:rate=5:duration=3",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=48000:duration=3",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-movflags",
                "+faststart",
            ])
            .arg(&path)
            .status()
            .expect("launch fixture encoder");
        assert!(status.success());
        let policy =
            IndependentExportArtifactPolicy::new(16 * 1024 * 1024, Duration::from_secs(30))
                .expect("policy");

        let first = verify_export_artifact(&path, "export-job-1", policy).expect("verify artifact");
        let second =
            verify_export_artifact(&path, "export-job-1", policy).expect("repeat verification");

        assert!(first.report.decoded_video);
        assert!(first.report.decoded_audio);
        assert_eq!(first.report.artifact_id, "export-job-1");
        assert_eq!(first.report.probe.video_stream_count, 1);
        assert_eq!(first.report.probe.audio_stream_count, 1);
        assert_eq!(first.report.decoded_video_frames, 15);
        assert!(first.report.decoded_duration_us > 0);
        assert_eq!(first.report.artifact_sha256.len(), 64);
        assert_eq!(first.report.decoded_content_sha256.len(), 64);
        assert_eq!(first.report, second.report);
        assert_eq!(
            first.validation_report_sha256,
            second.validation_report_sha256
        );
        assert!(first.decode_execution.cleanup.all_resources_released());
        assert!(first.report.snapshot_removed);
        assert!(!first.decode_execution.stdout.is_empty());
        let tiny_policy =
            IndependentExportArtifactPolicy::new(1, Duration::from_secs(30)).expect("tiny policy");
        let bound_failure = verify_export_artifact(&path, "export-job-1", tiny_policy)
            .expect_err("byte bound must reject before any native process");
        assert!(
            matches!(bound_failure, IndependentExportArtifactVerificationError::SnapshotClosed { ref primary }
            if matches!(**primary, IndependentExportArtifactVerificationError::ArtifactTooLarge { .. }))
        );
        assert!(bound_failure.evidence().snapshot_removed);
        assert!(bound_failure.evidence().snapshot_admitted);
        assert!(bound_failure.evidence().decode_execution.is_none());
        let cancellation = ExecutionCancellationToken::new();
        cancellation.cancel();
        assert!(matches!(
            verify_export_artifact_cancellable(&path, "export-job-1", policy, &cancellation),
            Err(IndependentExportArtifactVerificationError::Cancelled)
        ));

        let truncated = root.path().join("truncated.mp4");
        std::fs::copy(&path, &truncated).expect("copy truncated fixture");
        let truncated_file = std::fs::OpenOptions::new()
            .write(true)
            .open(&truncated)
            .expect("open truncated fixture");
        let original_len = truncated_file.metadata().expect("fixture metadata").len();
        truncated_file
            .set_len(original_len.saturating_mul(4) / 5)
            .expect("truncate after opening frame window");
        crate::validator::probe_export_output_cancellable(
            &truncated,
            &ExecutionCancellationToken::new(),
        )
        .expect("opening-frame probe still succeeds");
        let failure = verify_export_artifact(&truncated, "export-job-truncated", policy)
            .expect_err("truncated tail must fail complete decode")
            .evidence();
        assert!(failure.snapshot_admitted);
        assert!(failure.snapshot_removed);
        assert!(failure.snapshot_cleanup_error.is_none());
        assert!(failure.decode_execution.is_some());
        assert!(failure.child_cleanup.is_some());
        assert!(serde_json::to_vec(&failure).is_ok());
    }
}
