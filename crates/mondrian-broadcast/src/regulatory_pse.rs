//! External regulatory PSE evidence. No built-in luma heuristic grants approval.
use crate::BroadcastArtifactQcReport;
use serde::{Deserialize, Serialize};

/// Caller-supplied trust anchor for an independently approved, frozen analyzer.
/// Approval authenticity is established outside Mondrian; a digest alone is not certification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegulatoryPseApproval {
    /// Protocol schema, currently one.
    pub schema_version: u32,
    /// External approval authority, supplied by the commissioning organization.
    pub approval_authority: String,
    /// External approval identifier.
    pub approval_id: String,
    /// SHA-256 of the original approval document.
    pub approval_document_sha256: [u8; 32],
    /// Exact standard edition covered by that approval (never an alias such as latest).
    pub standard_edition: String,
    /// Fixed provider identifier.
    pub provider_id: String,
    /// Fixed provider build/version.
    pub provider_version: String,
    /// Hash of the executable protocol adapter approved for this provider.
    pub executable_sha256: [u8; 32],
    /// Hash of the exact provider-native approved analysis profile.
    pub approved_profile_sha256: [u8; 32],
    /// Corresponding Mondrian frozen delivery profile.
    pub qc_profile_fingerprint: [u8; 32],
}

impl RegulatoryPseApproval {
    /// Reject missing trust anchors and ambiguous editions before native admission.
    pub fn validate(&self) -> bool {
        self.schema_version == 1
            && [
                &self.approval_authority,
                &self.approval_id,
                &self.provider_id,
                &self.provider_version,
            ]
            .iter()
            .all(|value| !value.trim().is_empty() && value.len() <= 256)
            && matches!(self.standard_edition.as_str(), "ITU-R BT.1702-3 (11/2023)")
            && [
                self.approval_document_sha256,
                self.executable_sha256,
                self.approved_profile_sha256,
                self.qc_profile_fingerprint,
            ]
            .iter()
            .all(|hash| *hash != [0; 32])
    }
}

/// External analyzer outcome; incomplete scans never discharge a regulatory obligation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegulatoryPseVerdict {
    /// Complete analysis passed the approved rules.
    Pass,
    /// Complete analysis detected a regulatory failure.
    Fail,
    /// Provider could not complete the approved analysis.
    Incomplete,
}

/// Strict stdout protocol from one external analyzer invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegulatoryPseResponse {
    /// Protocol schema, currently one.
    pub schema_version: u32,
    /// Unpredictable request identity echoed by this invocation.
    pub request_nonce: String,
    /// Frozen trust anchor echoed in full, including executable and approved profile hashes.
    pub approval: RegulatoryPseApproval,
    /// Digest of the *same* finished-artifact QC report supplied in this request.
    pub artifact_qc_evidence_sha256: [u8; 32],
    /// Actual encoded object analyzed by the provider.
    pub artifact_sha256: [u8; 32],
    /// Actual encoded object size.
    pub artifact_bytes: u64,
    /// First analyzed frame, zero-based.
    pub first_frame: u64,
    /// Last analyzed frame, inclusive.
    pub last_frame: u64,
    /// Contiguous analyzed frame count, without skipped frames.
    pub analyzed_frames: u64,
    /// Analysis outcome, independently interpreted by the approved provider.
    pub verdict: RegulatoryPseVerdict,
    /// Native provider report identity; the protocol adapter must retain the native report.
    pub native_report_sha256: [u8; 32],
    /// Bounded diagnostic, not an alternative verdict authority.
    pub detail: String,
}

impl RegulatoryPseResponse {
    /// Bind approval, invocation, final bytes, frozen profile and exact full coverage.
    pub fn verifies(
        &self,
        approval: &RegulatoryPseApproval,
        nonce: &str,
        artifact: &BroadcastArtifactQcReport,
    ) -> bool {
        approval.validate()
            && artifact.verify_evidence()
            && artifact.scan.complete
            && self.schema_version == 1
            && self.approval == *approval
            && !nonce.is_empty()
            && nonce.len() <= 128
            && self.request_nonce == nonce
            && self.artifact_qc_evidence_sha256 == artifact.evidence_sha256
            && self.artifact_sha256 == artifact.artifact_sha256
            && self.artifact_bytes == artifact.artifact_bytes
            && approval.qc_profile_fingerprint == artifact.scan.profile_fingerprint
            && self.first_frame == 0
            && self.last_frame.checked_add(1) == Some(artifact.expected_frames)
            && self.analyzed_frames == artifact.expected_frames
            && self.native_report_sha256 != [0; 32]
            && self.detail.len() <= 16 * 1024
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BroadcastArtifactQcSession, BroadcastQcObservationTap, BroadcastQcProfile, BroadcastQcRule,
        BroadcastQcSeverity, BroadcastQcVerdict, QcActivePicture,
    };
    fn fixture() -> (
        BroadcastArtifactQcReport,
        RegulatoryPseApproval,
        RegulatoryPseResponse,
    ) {
        let profile = BroadcastQcProfile {
            id: "synthetic-protocol-only".to_owned(),
            edition: "fixture-1".to_owned(),
            source_sha256: [3; 32],
            signal_color_space: mondrian_core::ColorSpace::Rec709,
            observation_tap: BroadcastQcObservationTap::DeliveryPictureAfterLegalizer,
            active_picture: QcActivePicture::full(1, 1),
            rules: vec![BroadcastQcRule::LumaFlashCandidate {
                rule_id: "triage-only".to_owned(),
                minimum_mean_luma_delta: 0.5,
                severity: BroadcastQcSeverity::Info,
            }],
            maximum_retained_findings: 4,
            require_regulatory_flash_analysis: true,
            require_encoded_artifact_revalidation: true,
        };
        let mut session = BroadcastArtifactQcSession::new(profile, 1, 12).expect("session");
        session.push(&[0; 12]).expect("one complete frame");
        let artifact = session.finish([7; 32], 12, true).expect("same-run scan");
        let approval = RegulatoryPseApproval {
            schema_version: 1,
            approval_authority: "SYNTHETIC TEST ONLY".to_owned(),
            approval_id: "not-a-certification".to_owned(),
            approval_document_sha256: [1; 32],
            standard_edition: "ITU-R BT.1702-3 (11/2023)".to_owned(),
            provider_id: "protocol-fixture".to_owned(),
            provider_version: "1".to_owned(),
            executable_sha256: [2; 32],
            approved_profile_sha256: [3; 32],
            qc_profile_fingerprint: artifact.scan.profile_fingerprint,
        };
        let response = RegulatoryPseResponse {
            schema_version: 1,
            request_nonce: "one-invocation".to_owned(),
            approval: approval.clone(),
            artifact_qc_evidence_sha256: artifact.evidence_sha256,
            artifact_sha256: artifact.artifact_sha256,
            artifact_bytes: artifact.artifact_bytes,
            first_frame: 0,
            last_frame: 0,
            analyzed_frames: 1,
            verdict: RegulatoryPseVerdict::Pass,
            native_report_sha256: [9; 32],
            detail: String::new(),
        };
        (artifact, approval, response)
    }
    #[test]
    fn luma_triage_cannot_discharge_regulatory_obligation_and_only_exact_scan_can_bind() {
        let (artifact, approval, response) = fixture();
        assert_eq!(artifact.scan.verdict, BroadcastQcVerdict::Warn);
        let resolved = artifact
            .scan
            .with_regulatory_pse(&artifact, &approval, "one-invocation", &response)
            .expect("bound external fixture");
        assert_eq!(resolved.verdict, BroadcastQcVerdict::Pass);
        assert!(resolved.verify_evidence());
        assert!(artifact.verify_evidence());
        assert!(resolved
            .with_regulatory_pse(&artifact, &approval, "one-invocation", &response)
            .is_none());
        for mutation in 0..15 {
            let mut altered = response.clone();
            match mutation {
                0 => altered.request_nonce = "previous-invocation".to_owned(),
                1 => altered.artifact_qc_evidence_sha256[0] ^= 1,
                2 => altered.artifact_sha256[0] ^= 1,
                3 => altered.artifact_bytes += 1,
                4 => altered.first_frame = 1,
                5 => altered.last_frame = u64::MAX,
                6 => altered.analyzed_frames = 0,
                7 => altered.approval.provider_version.push('x'),
                8 => altered.approval.executable_sha256[0] ^= 1,
                9 => altered.approval.approved_profile_sha256[0] ^= 1,
                10 => altered.approval.approval_document_sha256[0] ^= 1,
                11 => altered.approval.qc_profile_fingerprint[0] ^= 1,
                12 => altered.native_report_sha256 = [0; 32],
                13 => altered.detail = "x".repeat(16 * 1024 + 1),
                _ => altered.schema_version = 2,
            }
            assert!(
                artifact
                    .scan
                    .with_regulatory_pse(&artifact, &approval, "one-invocation", &altered)
                    .is_none(),
                "mutation {mutation}"
            );
        }
        for (verdict, expected) in [
            (RegulatoryPseVerdict::Fail, BroadcastQcVerdict::Fail),
            (
                RegulatoryPseVerdict::Incomplete,
                BroadcastQcVerdict::Incomplete,
            ),
        ] {
            let mut changed = response.clone();
            changed.verdict = verdict;
            assert_eq!(
                artifact
                    .scan
                    .with_regulatory_pse(&artifact, &approval, "one-invocation", &changed)
                    .expect("diagnostic")
                    .verdict,
                expected
            );
        }
    }
    #[test]
    fn protocol_rejects_duplicate_unknown_trailing_and_unfrozen_approval() {
        let (_, mut approval, response) = fixture();
        let json = serde_json::to_string(&response).expect("JSON");
        assert!(serde_json::from_str::<RegulatoryPseResponse>(&format!("{json} {{}}")).is_err());
        assert!(
            serde_json::from_str::<RegulatoryPseResponse>(&json.replacen(
                '{',
                "{\"schema_version\":1,",
                1
            ))
            .is_err()
        );
        assert!(
            serde_json::from_str::<RegulatoryPseResponse>(&json.replacen(
                '{',
                "{\"self_certified\":true,",
                1
            ))
            .is_err()
        );
        approval.standard_edition = "BT1702 latest".to_owned();
        assert!(!approval.validate());
        approval.standard_edition = "ITU-R BT.1702-3 (11/2023)".to_owned();
        approval.approval_authority.clear();
        assert!(!approval.validate());
    }
}
