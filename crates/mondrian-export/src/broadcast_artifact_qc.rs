//! Independently reopen and stream the final encoded picture through shared QC.

use crate::artifact_verifier::{hash_published_artifact_until, snapshot_artifact_until};
use crate::validator::{validate_export_output_until, ExportValidationExpectations};
use mondrian_broadcast::{
    BroadcastArtifactQcReport, BroadcastArtifactQcSession, BroadcastQcProfile,
};
use mondrian_core::ExecutionCancellationToken;
use mondrian_media::{
    SupervisedProcessCleanupReceipt, SupervisedProcessPolicy, SupervisedStreamCapture,
};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::Path,
    time::{Duration, Instant},
};

/// Actual final-object scan and consuming native decoder closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishedBroadcastArtifactReceipt {
    /// Encoded object identity and exact decoded-frame coverage.
    pub artifact: BroadcastArtifactQcReport,
    /// Actual decoder process and pipe-worker settlement.
    pub decoder_cleanup: SupervisedProcessCleanupReceipt,
}

/// A failed scan retains both partial content evidence and independent cleanup.
#[derive(Debug, thiserror::Error)]
#[error(
    "final broadcast artifact scan failed: {cause}; snapshot cleanup: {snapshot_cleanup_error:?}"
)]
pub struct FinishedBroadcastArtifactError {
    /// Original typed operation failure, including native cleanup when applicable.
    #[source]
    pub cause: anyhow::Error,
    /// Partial raw content observation; never promoted to a complete scan.
    pub artifact: Option<Box<BroadcastArtifactQcReport>>,
    /// Decoder exit and pipe closure observed before a later scan/identity failure.
    pub decoder_cleanup: Option<Box<SupervisedProcessCleanupReceipt>>,
    /// Failure to consume the temporary encoded-object snapshot.
    pub snapshot_cleanup_error: Option<String>,
}

/// Serializable diagnostic preserving a rejected scan and native cleanup facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishedBroadcastArtifactFailure {
    /// Operation error for diagnostics.
    pub error: String,
    /// Partial final-object evidence, if decoding was admitted.
    pub artifact: Option<BroadcastArtifactQcReport>,
    /// Raw cleanup attached to a failed supervised decoder.
    pub decoder_cleanup: Option<SupervisedProcessCleanupReceipt>,
    /// Independent temporary object cleanup failure.
    pub snapshot_cleanup_error: Option<String>,
}

impl FinishedBroadcastArtifactError {
    /// Preserve this failure in the queue's durable job diagnostics.
    pub fn diagnostics(&self) -> FinishedBroadcastArtifactFailure {
        let decoder_cleanup = self
            .decoder_cleanup
            .as_deref()
            .cloned()
            .or_else(|| {
                self.cause
                    .chain()
                    .find_map(|error| {
                        error.downcast_ref::<mondrian_media::SupervisedProcessError>()
                    })
                    .and_then(|error| match error {
                        mondrian_media::SupervisedProcessError::Cleanup { cleanup, .. } => {
                            Some(cleanup.as_ref().clone())
                        }
                        _ => None,
                    })
            })
            .or_else(|| {
                self.cause.chain().find_map(|error| {
                    match error.downcast_ref::<crate::validator::ExportValidationError>() {
                        Some(crate::validator::ExportValidationError::ProbeOutput {
                            output,
                            ..
                        }) => Some(output.cleanup.clone()),
                        _ => None,
                    }
                })
            });
        FinishedBroadcastArtifactFailure {
            error: format!("{:#}", self.cause),
            artifact: self.artifact.as_deref().cloned(),
            decoder_cleanup,
            snapshot_cleanup_error: self.snapshot_cleanup_error.clone(),
        }
    }
}

/// Scan every final encoded video frame without duration-sized memory or raw spooling.
///
/// The exact output contract is reprobed against a private encoded-file snapshot.
/// Matrix/range decoding preserves encoded RGB values; no transfer, display view,
/// frame duplication, legalizer, scaling, or tone mapping is applied.
pub fn verify_finished_broadcast_artifact(
    path: &Path,
    expectations: &ExportValidationExpectations,
    profile: BroadcastQcProfile,
    expected_frames: u64,
    maximum_artifact_bytes: u64,
    timeout: Duration,
    cancellation: &ExecutionCancellationToken,
) -> Result<FinishedBroadcastArtifactReceipt, FinishedBroadcastArtifactError> {
    let deadline =
        Instant::now()
            .checked_add(timeout)
            .filter(|_| !timeout.is_zero())
            .ok_or_else(|| FinishedBroadcastArtifactError {
                cause: anyhow::anyhow!("zero or overflowing original artifact deadline"),
                artifact: None,
                decoder_cleanup: None,
                snapshot_cleanup_error: None,
            })?;
    verify_finished_broadcast_artifact_until(
        path,
        expectations,
        profile,
        expected_frames,
        maximum_artifact_bytes,
        deadline,
        cancellation,
    )
}

pub(crate) fn verify_finished_broadcast_artifact_until(
    path: &Path,
    expectations: &ExportValidationExpectations,
    profile: BroadcastQcProfile,
    expected_frames: u64,
    maximum_artifact_bytes: u64,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> Result<FinishedBroadcastArtifactReceipt, FinishedBroadcastArtifactError> {
    let mut partial = None;
    let mut observed_cleanup = None;
    let snapshot = snapshot_artifact_until(path, maximum_artifact_bytes, cancellation, deadline)
        .map_err(|cause| {
            let snapshot_cleanup_error = match &cause {
                crate::artifact_verifier::IndependentExportArtifactVerificationError::SnapshotCleanup { cleanup, .. } => Some(cleanup.to_string()),
                _ => None,
            };
            FinishedBroadcastArtifactError { cause: cause.into(), artifact: None, decoder_cleanup: None, snapshot_cleanup_error }
        })?;
    let snapshot_path = snapshot.file.into_temp_path();
    let outcome = (|| -> anyhow::Result<FinishedBroadcastArtifactReceipt> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Deny writes and replacement for the whole native decoder lifetime.
            options.share_mode(1);
        }
        let _snapshot_read_lease = options.open(&snapshot_path)?;
        anyhow::ensure!(
            hash_published_artifact_until(
                &snapshot_path,
                maximum_artifact_bytes,
                cancellation,
                deadline
            )? == (snapshot.bytes, snapshot.sha256.clone()),
            "snapshot changed before read lease"
        );
        let digest: [u8; 32] = decode_sha256(&snapshot.sha256)?;
        let probe =
            validate_export_output_until(&snapshot_path, expectations, cancellation, deadline)?;
        let video =
            probe.video.as_ref().ok_or_else(|| anyhow::anyhow!("final QC requires video"))?;
        let tags = profile
            .signal_color_space
            .ffmpeg_tags()
            .ok_or_else(|| anyhow::anyhow!("unsupported encoded signal color"))?;
        anyhow::ensure!(
            probe.video_stream_count == 1
                && video.width == Some(profile.active_picture.raster_width)
                && video.height == Some(profile.active_picture.raster_height)
                && video.color_primaries.as_deref() == Some(tags.color_primaries)
                && video.color_transfer.as_deref() == Some(tags.color_trc),
            "artifact raster or signal differs from frozen QC profile"
        );
        let range = match video.color_range.as_deref() {
            Some("tv") => "limited",
            Some("pc") => "full",
            _ => anyhow::bail!("artifact range must be explicit"),
        };
        let matrix = match video.color_matrix.as_deref() {
            Some("rgb") => "gbr",
            Some("bt709") => "709",
            Some("bt2020nc") => "2020_ncl",
            Some("bt470bg") => "470bg",
            Some("smpte170m") => "170m",
            _ => anyhow::bail!("artifact matrix is absent or unsupported"),
        };
        let mut session =
            BroadcastArtifactQcSession::new(profile, expected_frames, 256 * 1024 * 1024)?;
        let mut command = mondrian_media::ffmpeg_command()?;
        command.args(["-hide_banner", "-loglevel", "error", "-xerror", "-err_detect", "explode", "-nostdin", "-noautorotate", "-apply_cropping", "1", "-i"])
            .arg(&snapshot_path)
            .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"])
            .arg(format!("zscale=matrixin={matrix}:rangein={range}:matrix=gbr:range=full:dither=none,format=gbrpf32le"))
            .args(["-autoscale", "0", "-fps_mode", "passthrough", "-c:v", "rawvideo", "-pix_fmt", "gbrpf32le", "-f", "rawvideo", "pipe:1"]);
        let decoded = mondrian_media::run_supervised_command_streaming_stdout(
            &mut command,
            SupervisedProcessPolicy {
                deadline: Some(deadline),
                stderr: SupervisedStreamCapture::Tail { limit_bytes: 64 * 1024 },
                ..SupervisedProcessPolicy::default()
            },
            &|| cancellation.is_canceled(),
            |bytes| session.push(bytes).map_err(io::Error::other),
        );
        if let Ok(output) = &decoded {
            observed_cleanup = Some(output.cleanup.clone());
        }
        let verified = (|| -> anyhow::Result<_> {
            let output = decoded?;
            anyhow::ensure!(
                output.status.success() && output.cleanup.all_resources_released(),
                "decoder failed: {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            let final_identity = hash_published_artifact_until(
                path,
                maximum_artifact_bytes,
                cancellation,
                deadline,
            )?;
            let decoded_identity = hash_published_artifact_until(
                &snapshot_path,
                maximum_artifact_bytes,
                cancellation,
                deadline,
            )?;
            anyhow::ensure!(
                final_identity == (snapshot.bytes, snapshot.sha256.clone())
                    && decoded_identity == final_identity,
                "encoded artifact identity changed during scan"
            );
            Ok(output.cleanup)
        })();
        let artifact = session.finish(digest, snapshot.bytes, verified.is_ok())?;
        partial = Some(artifact.clone());
        let decoder_cleanup = verified?;
        anyhow::ensure!(
            artifact.verify_evidence() && artifact.scan.complete,
            "decoder frame coverage is incomplete"
        );
        Ok(FinishedBroadcastArtifactReceipt { artifact, decoder_cleanup })
    })();
    let cleanup = snapshot_path.close().err().map(|error| error.to_string());
    let outcome = if cancellation.is_canceled() || Instant::now() >= deadline {
        let detail = if cancellation.is_canceled() {
            "artifact verification canceled through snapshot cleanup"
        } else {
            "original artifact deadline exceeded through snapshot cleanup"
        };
        Err(match outcome {
            Ok(_) => anyhow::anyhow!(detail),
            Err(primary) => primary.context(detail),
        })
    } else {
        outcome
    };
    match (outcome, cleanup) {
        (Ok(receipt), None) => Ok(receipt),
        (result, snapshot_cleanup_error) => Err(FinishedBroadcastArtifactError {
            cause: result
                .err()
                .unwrap_or_else(|| anyhow::anyhow!("encoded snapshot could not be consumed")),
            artifact: partial.map(Box::new),
            decoder_cleanup: observed_cleanup.map(Box::new),
            snapshot_cleanup_error,
        }),
    }
}

fn decode_sha256(value: &str) -> anyhow::Result<[u8; 32]> {
    anyhow::ensure!(
        value.len() == 64 && value.is_ascii(),
        "invalid artifact digest"
    );
    let mut bytes = [0; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validator::{ExpectedStream, ExpectedVideoConstraints};
    use mondrian_broadcast::{
        BroadcastQcObservationTap, BroadcastQcRule, BroadcastQcSeverity, BroadcastQcVerdict,
        QcActivePicture,
    };

    #[test]
    fn finished_broadcast_preserves_real_limited_range_under_and_over_shoot() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let cancellation = ExecutionCancellationToken::new();
        let expectations = ExportValidationExpectations {
            container: crate::preset::Container::Mkv,
            video: ExpectedStream::Required(ExpectedVideoConstraints {
                width: Some(16),
                height: Some(16),
                ..Default::default()
            }),
            audio: ExpectedStream::Forbidden,
            expected_duration_secs: None,
        };
        let profile = BroadcastQcProfile {
            id: "encoded-excursions".to_owned(),
            edition: "1".to_owned(),
            source_sha256: [2; 32],
            signal_color_space: mondrian_core::ColorSpace::Rec709,
            observation_tap: BroadcastQcObservationTap::DeliveryPictureAfterLegalizer,
            active_picture: QcActivePicture::full(16, 16),
            rules: vec![BroadcastQcRule::SignalExcursion {
                rule_id: "range".to_owned(),
                tolerance_per_mille: 0,
                maximum_coverage_ppm: 0,
                severity: BroadcastQcSeverity::Fail,
            }],
            maximum_retained_findings: 8,
            require_regulatory_flash_analysis: false,
            require_encoded_artifact_revalidation: true,
        };
        for luma in [0_u16, 1023] {
            let mut raw = Vec::new();
            for value in [luma, 512, 512] {
                for _ in 0..256 {
                    raw.extend_from_slice(&value.to_le_bytes());
                }
            }
            let path = directory.path().join(format!("excursion-{luma}.mkv"));
            let mut command = mondrian_media::ffmpeg_command().expect("encode admission");
            command
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "rawvideo",
                    "-pixel_format",
                    "yuv444p10le",
                    "-video_size",
                    "16x16",
                    "-framerate",
                    "25",
                    "-i",
                    "pipe:0",
                    "-frames:v",
                    "1",
                    "-an",
                    "-c:v",
                    "ffv1",
                    "-threads",
                    "1",
                    "-level",
                    "3",
                    "-pix_fmt",
                    "yuv444p10le",
                    "-color_primaries",
                    "bt709",
                    "-color_trc",
                    "bt709",
                    "-colorspace",
                    "bt709",
                    "-color_range",
                    "tv",
                ])
                .arg(&path);
            let output = mondrian_media::run_supervised_command(
                &mut command,
                Some(raw),
                SupervisedProcessPolicy {
                    pipe_stdin: true,
                    deadline: Some(Instant::now() + Duration::from_secs(30)),
                    ..Default::default()
                },
                &cancellation,
            )
            .expect("native lossless encoder");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let receipt = verify_finished_broadcast_artifact(
                &path,
                &expectations,
                profile.clone(),
                1,
                1024 * 1024,
                Duration::from_secs(30),
                &cancellation,
            )
            .expect("complete encoded excursion scan");
            assert_eq!(
                receipt.artifact.scan.verdict,
                BroadcastQcVerdict::Fail,
                "encoded Y={luma} must not be clipped into legal range"
            );
            assert_eq!(receipt.artifact.scan.analyzed_frames, 1);
            assert!(!receipt.artifact.scan.findings.is_empty());
            assert!(receipt.decoder_cleanup.all_resources_released());
        }
        let error = verify_finished_broadcast_artifact(
            &directory.path().join("absent.mkv"),
            &expectations,
            profile,
            1,
            1024,
            Duration::ZERO,
            &cancellation,
        )
        .expect_err("deadline fails before any snapshot admission");
        assert!(error.cause.to_string().contains("deadline"));
        assert!(error.artifact.is_none());
    }

    #[test]
    fn finished_broadcast_artifact_rescan_checks_real_frame_inventory_and_cancellation() {
        let directory = tempfile::tempdir().expect("artifact directory");
        let path = directory.path().join("three-frames.mp4");
        let cancellation = ExecutionCancellationToken::new();
        let mut command = mondrian_media::ffmpeg_command().expect("FFmpeg admission");
        command
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                "color=c=gray:s=32x16:r=25:d=0.12",
                "-an",
                "-c:v",
                "libx264",
                "-threads",
                "1",
                "-pix_fmt",
                "yuv420p",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
                "-colorspace",
                "bt709",
                "-color_range",
                "tv",
                "-x264-params",
                "colorprim=bt709:transfer=bt709:colormatrix=bt709:range=tv",
            ])
            .arg(&path);
        let output = mondrian_media::run_supervised_command(
            &mut command,
            None,
            SupervisedProcessPolicy {
                deadline: Some(Instant::now() + Duration::from_secs(30)),
                ..SupervisedProcessPolicy::default()
            },
            &cancellation,
        )
        .expect("native encoder");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.cleanup.all_resources_released());
        let expectations = ExportValidationExpectations {
            container: crate::preset::Container::Mp4,
            video: ExpectedStream::Required(ExpectedVideoConstraints {
                width: Some(32),
                height: Some(16),
                ..Default::default()
            }),
            audio: ExpectedStream::Forbidden,
            expected_duration_secs: None,
        };
        let profile = BroadcastQcProfile {
            id: "final-artifact".to_owned(),
            edition: "1".to_owned(),
            source_sha256: [1; 32],
            signal_color_space: mondrian_core::ColorSpace::Rec709,
            observation_tap: BroadcastQcObservationTap::DeliveryPictureAfterLegalizer,
            active_picture: QcActivePicture::full(32, 16),
            rules: vec![BroadcastQcRule::SignalExcursion {
                rule_id: "excursion".to_owned(),
                tolerance_per_mille: 1,
                maximum_coverage_ppm: 0,
                severity: BroadcastQcSeverity::Fail,
            }],
            maximum_retained_findings: 8,
            require_regulatory_flash_analysis: false,
            require_encoded_artifact_revalidation: true,
        };
        let receipt = verify_finished_broadcast_artifact(
            &path,
            &expectations,
            profile.clone(),
            3,
            1024 * 1024,
            Duration::from_secs(30),
            &cancellation,
        )
        .expect("complete encoded rescan");
        assert_eq!(receipt.artifact.scan.verdict, BroadcastQcVerdict::Pass);
        assert!(receipt.artifact.verify_evidence());
        assert!(receipt.decoder_cleanup.all_resources_released());
        for expected in [2, 4] {
            let failure = verify_finished_broadcast_artifact(
                &path,
                &expectations,
                profile.clone(),
                expected,
                1024 * 1024,
                Duration::from_secs(30),
                &cancellation,
            )
            .expect_err("wrong frame inventory");
            let diagnostic = failure.diagnostics();
            assert_eq!(
                diagnostic.artifact.expect("partial scan").scan.verdict,
                BroadcastQcVerdict::Incomplete
            );
            assert!(diagnostic
                .decoder_cleanup
                .expect("native closure retained")
                .all_resources_released());
            assert!(diagnostic.snapshot_cleanup_error.is_none());
        }
        let rotated = directory.path().join("rotated.mp4");
        let mut rotate = mondrian_media::ffmpeg_command().expect("remux admission");
        // The legacy rotate metadata tag is silently ignored by some FFmpeg
        // builds. Override the input display matrix so the remux really carries
        // the non-identity transform that this independent scan must reject.
        rotate
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-display_rotation:v:0",
                "90",
                "-i",
            ])
            .arg(&path)
            .args(["-c", "copy"])
            .arg(&rotated);
        let remux = mondrian_media::run_supervised_command(
            &mut rotate,
            None,
            SupervisedProcessPolicy {
                deadline: Some(Instant::now() + Duration::from_secs(30)),
                ..Default::default()
            },
            &cancellation,
        )
        .expect("rotation remux");
        assert!(remux.status.success());
        let failure = verify_finished_broadcast_artifact(
            &rotated,
            &expectations,
            profile.clone(),
            3,
            1024 * 1024,
            Duration::from_secs(30),
            &cancellation,
        )
        .expect_err("non-square display rotation must reject");
        assert!(format!("{:#}", failure.cause).contains("display matrix"));
        assert!(failure.artifact.is_none());
        assert!(failure.snapshot_cleanup_error.is_none());
        cancellation.cancel();
        let failure = verify_finished_broadcast_artifact(
            &path,
            &expectations,
            profile,
            3,
            1024 * 1024,
            Duration::from_secs(30),
            &cancellation,
        )
        .expect_err("canceled admission");
        assert!(failure.artifact.is_none());
    }
}
