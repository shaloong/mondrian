//! Independent bounded verification for a finished single-file export artifact.

use std::fs::File;
use std::io::{BufReader, Read, Write};
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

use crate::validator::{probe_export_output_cancellable, ExportOutputProbe};

/// Stable implementation identity bound into every independent report.
pub const INDEPENDENT_EXPORT_ARTIFACT_VALIDATOR_ID: &str =
    "mondrian-export-independent-full-decode-v1";

const HASH_STDOUT_LIMIT: usize = 256;
const PROGRESS_STDERR_LIMIT: usize = 64 * 1024;

/// Explicit resource limits for one independent artifact verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndependentExportArtifactPolicy {
    /// Largest regular file admitted for hashing and full decode.
    maximum_artifact_bytes: u64,
    /// Maximum wall time granted to the full FFmpeg decode process.
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

    /// Maximum wall time granted to the full FFmpeg decode process.
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
}

/// Self-contained report plus its canonical JSON digest.
#[derive(Debug, Clone, PartialEq)]
pub struct IndependentExportArtifactReceipt {
    /// Complete independently derived report.
    report: IndependentExportArtifactReport,
    /// SHA-256 of the canonical serialized report bytes.
    validation_report_sha256: String,
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
    let artifact_id = artifact_id.into();
    if artifact_id.is_empty() || artifact_id.len() > 256 {
        return Err(IndependentExportArtifactVerificationError::InvalidArtifactId);
    }
    let snapshot = snapshot_artifact(path, policy.maximum_artifact_bytes, cancellation)?;
    let artifact_bytes = snapshot.bytes;
    let artifact_sha256 = snapshot.sha256.clone();
    let probe = probe_export_output_cancellable(snapshot.file.path(), cancellation)
        .map_err(IndependentExportArtifactVerificationError::Probe)?;
    let decoded_video = probe.video.is_some();
    let decoded_audio = probe.audio.is_some();
    if !decoded_video && !decoded_audio {
        return Err(IndependentExportArtifactVerificationError::NoDecodableStreams);
    }
    let decode = full_decode(snapshot.file.path(), policy.decode_timeout, cancellation)?;
    if decoded_video && decode.video_frames == 0 {
        return Err(IndependentExportArtifactVerificationError::MissingVideoFrames);
    }
    if probe.duration_secs.is_some_and(|duration| duration > 0.0) && decode.duration_us == 0 {
        return Err(IndependentExportArtifactVerificationError::MissingDecodeDuration);
    }

    if let Some(expected_duration_secs) = probe.duration_secs.filter(|duration| *duration > 0.0) {
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

    let (final_bytes, final_sha256) =
        hash_published_artifact(path, policy.maximum_artifact_bytes, cancellation)?;
    if final_bytes != artifact_bytes || final_sha256 != artifact_sha256 {
        return Err(IndependentExportArtifactVerificationError::ArtifactChanged);
    }

    let report = IndependentExportArtifactReport {
        schema_version: 1,
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
    };
    let report_bytes = serde_json::to_vec(&report)
        .map_err(IndependentExportArtifactVerificationError::SerializeReport)?;
    let validation_report_sha256 = format!("{:x}", Sha256::digest(&report_bytes));
    Ok(IndependentExportArtifactReceipt { report, validation_report_sha256 })
}

struct ImmutableArtifactSnapshot {
    file: tempfile::NamedTempFile,
    bytes: u64,
    sha256: String,
}

fn snapshot_artifact(
    path: &Path,
    maximum_bytes: u64,
    cancellation: &ExecutionCancellationToken,
) -> Result<ImmutableArtifactSnapshot, IndependentExportArtifactVerificationError> {
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
    let (bytes, sha256) = copy_and_hash_bounded(
        BufReader::new(source),
        snapshot.as_file_mut(),
        maximum_bytes,
        cancellation,
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
    Ok(ImmutableArtifactSnapshot { file: snapshot, bytes, sha256 })
}

fn hash_published_artifact(
    path: &Path,
    maximum_bytes: u64,
    cancellation: &ExecutionCancellationToken,
) -> Result<(u64, String), IndependentExportArtifactVerificationError> {
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
    )?;
    require_direct_regular_file(path)?;
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
) -> Result<(u64, String), IndependentExportArtifactVerificationError> {
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if cancellation.is_canceled() {
            return Err(IndependentExportArtifactVerificationError::Cancelled);
        }
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
    Ok((total, format!("{:x}", digest.finalize())))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FullDecodeEvidence {
    video_frames: u64,
    duration_us: u64,
    content_sha256: String,
}

fn full_decode(
    path: &Path,
    timeout: Duration,
    cancellation: &ExecutionCancellationToken,
) -> Result<FullDecodeEvidence, IndependentExportArtifactVerificationError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(IndependentExportArtifactVerificationError::DeadlineOverflow)?;
    let mut command = mondrian_media::ffmpeg_command();
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
    if !output.status.success() {
        return Err(IndependentExportArtifactVerificationError::DecodeFailed {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let content_sha256 = parse_hash_output(&output.stdout)?;
    let (video_frames, duration_us) = parse_final_progress(&output.stderr)?;
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
    Probe(String),
    /// Probe found no selected media streams.
    #[error("export artifact has no independently decodable video or audio stream")]
    NoDecodableStreams,
    /// Decode deadline could not be represented.
    #[error("independent export decode deadline overflowed")]
    DeadlineOverflow,
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(first, second);
        let tiny_policy =
            IndependentExportArtifactPolicy::new(1, Duration::from_secs(30)).expect("tiny policy");
        assert!(matches!(
            verify_export_artifact(&path, "export-job-1", tiny_policy),
            Err(IndependentExportArtifactVerificationError::ArtifactTooLarge { .. })
        ));
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
        probe_export_output_cancellable(&truncated, &ExecutionCancellationToken::new())
            .expect("opening-frame probe still succeeds");
        assert!(verify_export_artifact(&truncated, "export-job-truncated", policy).is_err());
    }
}
