//! Bounded finished-artifact scan. The decode Adapter supplies unmodified
//! planar GBR Float32 little-endian samples after independently opening the
//! encoded file; this Module owns frame boundaries, coverage, and QC meaning.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    BroadcastQcError, BroadcastQcFrame, BroadcastQcProfile, BroadcastQcReport, BroadcastQcSession,
};

const HARD_MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;

/// Receipt binding a complete decoded content scan to immutable artifact bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BroadcastArtifactQcReport {
    /// Independent finished-artifact receipt schema.
    pub schema_version: u32,
    /// SHA-256 of the exact encoded object read by the decoder.
    pub artifact_sha256: [u8; 32],
    /// Encoded object size, independently checked after decode.
    pub artifact_bytes: u64,
    /// Exact frozen export frame count.
    pub expected_frames: u64,
    /// Actual decoded GBR float byte count.
    pub decoded_bytes: u64,
    /// Exact planar GBR Float32 bytes per decoded raster.
    pub decoded_frame_bytes: u64,
    /// SHA-256 of the exact ordered raw decoder output.
    pub decoded_sha256: [u8; 32],
    /// Same frozen QC profile evaluated on the decoded final artifact.
    pub scan: BroadcastQcReport,
    /// Digest of the artifact binding, exact coverage, decoded bytes, and QC evidence.
    pub evidence_sha256: [u8; 32],
}

impl BroadcastArtifactQcReport {
    /// Verify binding and coverage without promoting an incomplete diagnostic.
    pub fn verify_evidence(&self) -> bool {
        self.schema_version == 1
            && self.expected_frames != 0
            && self.decoded_frame_bytes != 0
            && self.scan.verify_evidence()
            && self.evidence_sha256 == self.digest()
            && (!self.scan.complete
                || (self.artifact_bytes != 0
                    && self.scan.analyzed_frames == self.expected_frames
                    && self.scan.first_frame == Some(0)
                    && self.scan.last_frame == self.expected_frames.checked_sub(1)
                    && self.expected_frames.checked_mul(self.decoded_frame_bytes)
                        == Some(self.decoded_bytes)))
    }

    fn digest(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"mondrian-broadcast-encoded-artifact-qc-v1");
        hash.update(self.schema_version.to_be_bytes());
        hash.update(self.artifact_sha256);
        for value in [
            self.artifact_bytes,
            self.expected_frames,
            self.decoded_bytes,
            self.decoded_frame_bytes,
        ] {
            hash.update(value.to_be_bytes());
        }
        hash.update(self.decoded_sha256);
        hash.update(self.scan.evidence_sha256);
        hash.finalize().into()
    }
}

/// A malformed, excessive, or incomplete decoder stream never proves a pass.
#[derive(Debug, thiserror::Error)]
pub enum BroadcastArtifactQcError {
    /// Invalid or unbounded expected frame inventory.
    #[error("artifact QC requires a nonzero frame count and a bounded decoded raster")]
    InvalidBounds,
    /// Bounded frame allocation was rejected before decode admission.
    #[error("artifact QC could not reserve its bounded frame buffers")]
    AllocationFailed,
    /// Decoder emitted more frames than the frozen export range.
    #[error("artifact decoder exceeded the exact frame inventory")]
    ExcessFrames,
    /// A prior stream failure cannot be cleared by sending more data.
    #[error("artifact QC stream is already invalid")]
    InvalidStream,
    /// Shared QC analyzer rejected the decoded content.
    #[error(transparent)]
    Analysis(#[from] BroadcastQcError),
}

/// One-frame buffering regardless of artifact duration or pipe chunking.
pub struct BroadcastArtifactQcSession {
    scan: BroadcastQcSession,
    expected_frames: u64,
    frames: u64,
    frame_bytes: usize,
    pending: Vec<u8>,
    rgba: Vec<[f32; 4]>,
    decoded_bytes: u64,
    digest: Sha256,
    invalid: bool,
}

impl BroadcastArtifactQcSession {
    /// Reserve a bounded raster before a decoder process is admitted.
    pub fn new(
        profile: BroadcastQcProfile,
        expected_frames: u64,
        maximum_frame_bytes: usize,
    ) -> Result<Self, BroadcastArtifactQcError> {
        let pixels = (profile.active_picture.raster_width as usize)
            .checked_mul(profile.active_picture.raster_height as usize)
            .ok_or(BroadcastArtifactQcError::InvalidBounds)?;
        let frame_bytes = pixels.checked_mul(12).ok_or(BroadcastArtifactQcError::InvalidBounds)?;
        if expected_frames == 0
            || frame_bytes == 0
            || frame_bytes > maximum_frame_bytes.min(HARD_MAX_FRAME_BYTES)
            || expected_frames.checked_mul(frame_bytes as u64).is_none()
        {
            return Err(BroadcastArtifactQcError::InvalidBounds);
        }
        let scan = BroadcastQcSession::new(profile)?;
        let mut pending = Vec::new();
        pending
            .try_reserve_exact(frame_bytes)
            .map_err(|_| BroadcastArtifactQcError::AllocationFailed)?;
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(pixels)
            .map_err(|_| BroadcastArtifactQcError::AllocationFailed)?;
        rgba.resize(pixels, [0.0; 4]);
        Ok(Self {
            scan,
            expected_frames,
            frames: 0,
            frame_bytes,
            pending,
            rgba,
            decoded_bytes: 0,
            digest: Sha256::new(),
            invalid: false,
        })
    }

    /// Consume bounded or arbitrarily fragmented pipe chunks without retaining
    /// more than one incomplete frame. Reject the first byte of an extra frame.
    pub fn push(&mut self, mut bytes: &[u8]) -> Result<(), BroadcastArtifactQcError> {
        if self.invalid {
            return Err(BroadcastArtifactQcError::InvalidStream);
        }
        while !bytes.is_empty() {
            if self.frames == self.expected_frames {
                self.invalid = true;
                return Err(BroadcastArtifactQcError::ExcessFrames);
            }
            let take = bytes.len().min(self.frame_bytes - self.pending.len());
            self.pending.extend_from_slice(&bytes[..take]);
            self.digest.update(&bytes[..take]);
            self.decoded_bytes += take as u64;
            bytes = &bytes[take..];
            if self.pending.len() == self.frame_bytes {
                let pixels = self.rgba.len();
                let sample = |offset: usize| {
                    f32::from_le_bytes([
                        self.pending[offset],
                        self.pending[offset + 1],
                        self.pending[offset + 2],
                        self.pending[offset + 3],
                    ])
                };
                for (index, rgba) in self.rgba.iter_mut().enumerate() {
                    *rgba = [
                        sample((pixels * 2 + index) * 4),
                        sample(index * 4),
                        sample((pixels + index) * 4),
                        1.0,
                    ];
                }
                if let Err(error) =
                    self.scan.push(BroadcastQcFrame { frame_index: self.frames, rgba: &self.rgba })
                {
                    self.invalid = true;
                    return Err(error.into());
                }
                self.frames += 1;
                self.pending.clear();
            }
        }
        Ok(())
    }

    /// Consume the analyzer after decoder EOF, process settlement, and object
    /// identity checks. Failed decode, truncation, and extra data stay Incomplete.
    pub fn finish(
        self,
        artifact_sha256: [u8; 32],
        artifact_bytes: u64,
        decoder_and_identity_verified: bool,
    ) -> Result<BroadcastArtifactQcReport, BroadcastArtifactQcError> {
        let complete = decoder_and_identity_verified
            && artifact_bytes != 0
            && !self.invalid
            && self.frames == self.expected_frames
            && self.pending.is_empty();
        let mut scan = self.scan.finish(complete)?;
        scan.record_encoded_artifact_rescan(artifact_sha256);
        let mut report = BroadcastArtifactQcReport {
            schema_version: 1,
            artifact_sha256,
            artifact_bytes,
            expected_frames: self.expected_frames,
            decoded_bytes: self.decoded_bytes,
            decoded_frame_bytes: self.frame_bytes as u64,
            decoded_sha256: self.digest.finalize().into(),
            scan,
            evidence_sha256: [0; 32],
        };
        report.evidence_sha256 = report.digest();
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BroadcastQcObligationKind, BroadcastQcObservationTap, BroadcastQcRule,
        BroadcastQcRuleStatus, BroadcastQcSeverity, BroadcastQcVerdict, QcActivePicture,
    };

    fn profile() -> BroadcastQcProfile {
        BroadcastQcProfile {
            id: "artifact-test".to_owned(),
            edition: "1".to_owned(),
            source_sha256: [1; 32],
            signal_color_space: mondrian_core::ColorSpace::Rec709,
            observation_tap: BroadcastQcObservationTap::DeliveryPictureAfterLegalizer,
            active_picture: QcActivePicture::full(2, 1),
            rules: vec![BroadcastQcRule::SignalExcursion {
                rule_id: "range".to_owned(),
                tolerance_per_mille: 0,
                maximum_coverage_ppm: 0,
                severity: BroadcastQcSeverity::Fail,
            }],
            maximum_retained_findings: 8,
            require_regulatory_flash_analysis: false,
            require_encoded_artifact_revalidation: true,
        }
    }

    fn bytes(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|value| value.to_le_bytes()).collect()
    }

    #[test]
    fn artifact_qc_chunking_preserves_identical_raw_content_and_report() {
        let input = bytes(&[0.25; 12]);
        let mut baseline = BroadcastArtifactQcSession::new(profile(), 2, 24).expect("session");
        baseline.push(&input).expect("all bytes");
        let expected = baseline.finish([2; 32], 99, true).expect("report");
        assert_eq!(expected.scan.verdict, BroadcastQcVerdict::Pass);
        assert!(expected.scan.verify_evidence());
        assert!(expected.verify_evidence());
        let mut tampered = expected.clone();
        tampered.artifact_sha256[0] ^= 1;
        assert!(!tampered.verify_evidence());
        let mut tampered = expected.clone();
        tampered.expected_frames += 1;
        tampered.evidence_sha256 = tampered.digest();
        assert!(!tampered.verify_evidence());
        assert_eq!(
            expected.scan.obligations[0].status,
            BroadcastQcRuleStatus::Pass
        );
        for chunk_size in 1..=input.len() {
            let mut session = BroadcastArtifactQcSession::new(profile(), 2, 24).expect("session");
            for chunk in input.chunks(chunk_size) {
                session.push(chunk).expect("fragment");
            }
            assert_eq!(session.finish([2; 32], 99, true).expect("report"), expected);
        }
    }

    #[test]
    fn artifact_qc_partial_extra_failed_identity_and_nonfinite_never_pass() {
        let input = bytes(&[0.25; 6]);
        for count in 0..input.len() {
            let mut session = BroadcastArtifactQcSession::new(profile(), 1, 24).expect("session");
            session.push(&input[..count]).expect("partial");
            assert_eq!(
                session.finish([2; 32], 99, true).expect("report").scan.verdict,
                BroadcastQcVerdict::Incomplete
            );
        }
        let mut extra = BroadcastArtifactQcSession::new(profile(), 1, 24).expect("session");
        extra.push(&input).expect("frame");
        assert!(matches!(
            extra.push(&[0]),
            Err(BroadcastArtifactQcError::ExcessFrames)
        ));
        assert_eq!(
            extra.finish([2; 32], 99, true).expect("report").scan.verdict,
            BroadcastQcVerdict::Incomplete
        );
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut session = BroadcastArtifactQcSession::new(profile(), 1, 24).expect("session");
            let _ = session.push(&bytes(&[invalid; 6]));
            assert_eq!(
                session.finish([2; 32], 99, true).expect("report").scan.verdict,
                BroadcastQcVerdict::Incomplete
            );
        }
        let mut session = BroadcastArtifactQcSession::new(profile(), 1, 24).expect("session");
        session.push(&input).expect("frame");
        assert_eq!(
            session.finish([2; 32], 99, false).expect("report").scan.verdict,
            BroadcastQcVerdict::Incomplete
        );
    }

    #[test]
    fn artifact_qc_never_promotes_pse_or_ignores_postencode_excursions() {
        let mut selected = profile();
        selected.require_regulatory_flash_analysis = true;
        let mut session =
            BroadcastArtifactQcSession::new(selected.clone(), 1, 24).expect("session");
        session.push(&bytes(&[0.25; 6])).expect("frame");
        let report = session.finish([2; 32], 99, true).expect("report");
        assert_eq!(report.scan.verdict, BroadcastQcVerdict::Warn);
        assert!(report.scan.obligations.iter().any(|item| item.kind
            == BroadcastQcObligationKind::RegulatoryPhotosensitiveFlash
            && item.status == BroadcastQcRuleStatus::NotTested));
        let mut session = BroadcastArtifactQcSession::new(selected, 1, 24).expect("session");
        session.push(&bytes(&[1.01; 6])).expect("frame");
        assert_eq!(
            session.finish([2; 32], 99, true).expect("report").scan.verdict,
            BroadcastQcVerdict::Fail
        );
        assert!(BroadcastArtifactQcSession::new(profile(), 0, 24).is_err());
        assert!(BroadcastArtifactQcSession::new(profile(), 1, 23).is_err());
        assert!(BroadcastArtifactQcSession::new(profile(), u64::MAX, 24).is_err());
    }
}
