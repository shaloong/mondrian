//! Typed admission for checked-in commercial endurance workloads.
//!
//! Workload bytes are parsed and bound to the compiled phase before any product
//! owner starts. Missing external fixtures/providers may yield typed `NotRun`;
//! malformed or mismatched contracts are always hard errors.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use mondrian_platform::{EndurancePhaseKind, EndurancePhaseRequirement};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

const WORKLOAD_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_WORKLOAD_BYTES: u64 = 16 * 1024;
const MICROS_PER_HOUR: u64 = 3_600_000_000;
const EXACT_PROGRAM_FRAMES_PER_SECOND: u64 = 60;

const PLAYBACK_REFERENCE_WORKLOAD_ID: &str = "mondrian-col047/playback-reference/v1";
const CONTINUOUS_EXPORT_WORKLOAD_ID: &str = "mondrian-col047/continuous-export/v1";
const CONCURRENT_RECOVERY_WORKLOAD_ID: &str = "mondrian-col047/concurrent-recovery/v1";

/// One concrete product capability required before an endurance phase may start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EnduranceWorkloadCapability {
    /// Exact Timeline picture/audio fixture can run continuously.
    TimelinePlaybackFixture,
    /// Immutable Sequence snapshot and source fixture can be exported repeatedly.
    FrozenExportFixture,
    /// A real audio output device is eligible to own Playback Clock.
    AudioOutputDevice,
    /// A physical Reference Output provider is open.
    PhysicalReferenceOutput,
    /// The physical provider reports continuous external-reference lock.
    ExternalReferenceLock,
    /// Published artifacts can be independently reopened and fully decoded.
    IndependentExportVerifier,
    /// Product seek can produce a sealed recovery receipt.
    SeekRecovery,
    /// Surface/device reopen can produce a sealed recovery receipt.
    SurfaceDeviceReopenRecovery,
    /// Export cancel and retry can produce a sealed recovery receipt.
    ExportCancelRetryRecovery,
    /// Bounded cache pressure and recovery can produce a sealed receipt.
    CachePressureRecovery,
}

/// Result of attempting to admit one exact, already prepared workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndurancePhaseAdmission {
    /// Every prerequisite was observed; the concrete runtime may start owners.
    Started,
    /// External prerequisites were absent; no product work may have started.
    NotRun(EnduranceNotRunAdmission),
}

/// Typed proof that an exact valid workload could not start for external reasons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnduranceNotRunAdmission {
    phase_id: String,
    workload_id: String,
    missing_capabilities: Vec<EnduranceWorkloadCapability>,
}

impl EnduranceNotRunAdmission {
    fn new(
        phase_id: String,
        workload_id: String,
        missing_capabilities: Vec<EnduranceWorkloadCapability>,
    ) -> Self {
        debug_assert!(!missing_capabilities.is_empty());
        Self { phase_id, workload_id, missing_capabilities }
    }

    /// Exact phase that remained unstarted.
    pub fn phase_id(&self) -> &str {
        &self.phase_id
    }

    /// Exact parsed workload that remained unstarted.
    pub fn workload_id(&self) -> &str {
        &self.workload_id
    }

    /// Complete ordered prerequisite inventory absent at admission.
    pub fn missing_capabilities(&self) -> &[EnduranceWorkloadCapability] {
        &self.missing_capabilities
    }
}

/// Exact capability inventory observed before attempting phase owner creation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnduranceWorkloadCapabilityInventory {
    capabilities: BTreeSet<EnduranceWorkloadCapability>,
}

impl EnduranceWorkloadCapabilityInventory {
    /// Construct an inventory from independently observed product capabilities.
    pub fn new(capabilities: impl IntoIterator<Item = EnduranceWorkloadCapability>) -> Self {
        Self { capabilities: capabilities.into_iter().collect() }
    }

    fn missing(
        &self,
        required: &[EnduranceWorkloadCapability],
    ) -> Vec<EnduranceWorkloadCapability> {
        required
            .iter()
            .copied()
            .filter(|capability| !self.capabilities.contains(capability))
            .collect()
    }
}

/// Parsed, hash-bound workload contract safe to hand to a concrete runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEnduranceWorkload {
    phase_id: String,
    workload_id: String,
    kind: EndurancePhaseKind,
    required_capabilities: Vec<EnduranceWorkloadCapability>,
    recovery_cycle_count: u32,
}

impl PreparedEnduranceWorkload {
    /// Parse and validate the exact workload required by one compiled phase.
    pub fn load(
        requirement: &EndurancePhaseRequirement,
        path: &Path,
    ) -> Result<Self, EnduranceWorkloadError> {
        let bytes = read_bounded_regular_file(path)?;
        let digest = lower_sha256(&bytes);
        if digest != requirement.workload_sha256 {
            return Err(EnduranceWorkloadError::DigestMismatch);
        }
        match requirement.kind {
            EndurancePhaseKind::PlaybackReference => {
                let contract: PlaybackReferenceWorkload = parse_contract(&bytes)?;
                contract.validate(requirement)?;
                Ok(Self {
                    phase_id: contract.phase_id,
                    workload_id: contract.workload_id,
                    kind: requirement.kind,
                    required_capabilities: vec![
                        EnduranceWorkloadCapability::TimelinePlaybackFixture,
                        EnduranceWorkloadCapability::AudioOutputDevice,
                        EnduranceWorkloadCapability::PhysicalReferenceOutput,
                        EnduranceWorkloadCapability::ExternalReferenceLock,
                    ],
                    recovery_cycle_count: 0,
                })
            }
            EndurancePhaseKind::ContinuousExport => {
                let contract: ContinuousExportWorkload = parse_contract(&bytes)?;
                contract.validate(requirement)?;
                Ok(Self {
                    phase_id: contract.phase_id,
                    workload_id: contract.workload_id,
                    kind: requirement.kind,
                    required_capabilities: vec![
                        EnduranceWorkloadCapability::FrozenExportFixture,
                        EnduranceWorkloadCapability::IndependentExportVerifier,
                    ],
                    recovery_cycle_count: 0,
                })
            }
            EndurancePhaseKind::ConcurrentRecovery => {
                let contract: ConcurrentRecoveryWorkload = parse_contract(&bytes)?;
                contract.validate(requirement)?;
                Ok(Self {
                    phase_id: contract.phase_id,
                    workload_id: contract.workload_id,
                    kind: requirement.kind,
                    required_capabilities: vec![
                        EnduranceWorkloadCapability::TimelinePlaybackFixture,
                        EnduranceWorkloadCapability::FrozenExportFixture,
                        EnduranceWorkloadCapability::AudioOutputDevice,
                        EnduranceWorkloadCapability::PhysicalReferenceOutput,
                        EnduranceWorkloadCapability::ExternalReferenceLock,
                        EnduranceWorkloadCapability::IndependentExportVerifier,
                        EnduranceWorkloadCapability::SeekRecovery,
                        EnduranceWorkloadCapability::SurfaceDeviceReopenRecovery,
                        EnduranceWorkloadCapability::ExportCancelRetryRecovery,
                        EnduranceWorkloadCapability::CachePressureRecovery,
                    ],
                    recovery_cycle_count: contract.recovery_cycle_count,
                })
            }
        }
    }

    /// Stable workload identity sealed by the checked-in contract.
    pub fn workload_id(&self) -> &str {
        &self.workload_id
    }

    /// Exact profile phase identity sealed by the workload.
    pub fn phase_id(&self) -> &str {
        &self.phase_id
    }

    /// Workload family the concrete runtime must start.
    pub const fn kind(&self) -> EndurancePhaseKind {
        self.kind
    }

    /// Required completed recovery cycles; zero outside recovery.
    pub const fn recovery_cycle_count(&self) -> u32 {
        self.recovery_cycle_count
    }

    /// Admit only when every phase prerequisite was observed before owner start.
    pub fn admit(
        &self,
        inventory: &EnduranceWorkloadCapabilityInventory,
    ) -> EndurancePhaseAdmission {
        let missing = inventory.missing(&self.required_capabilities);
        if missing.is_empty() {
            EndurancePhaseAdmission::Started
        } else {
            EndurancePhaseAdmission::NotRun(EnduranceNotRunAdmission::new(
                self.phase_id.clone(),
                self.workload_id.clone(),
                missing,
            ))
        }
    }
}

/// Stable typed-workload preparation failure.
#[derive(Debug, Error)]
pub enum EnduranceWorkloadError {
    /// Workload path was absent, not a regular leaf, or could not be read.
    #[error("endurance workload is not a readable regular file: {0}")]
    InvalidFile(String),
    /// Workload bytes exceeded the fixed admission bound.
    #[error("endurance workload exceeds the fixed byte limit")]
    TooLarge,
    /// Workload bytes differed from the digest compiled into the profile.
    #[error("endurance workload digest differs from the compiled phase")]
    DigestMismatch,
    /// Workload JSON did not match the exact schema for its phase family.
    #[error("endurance workload schema is invalid: {0}")]
    InvalidSchema(String),
    /// Typed workload semantics differed from the compiled phase requirement.
    #[error("endurance workload differs from its phase requirement: {0}")]
    RequirementMismatch(&'static str),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaybackReferenceWorkload {
    schema_version: u32,
    workload_id: String,
    phase_id: String,
    wall_clock_hours: u32,
    program_frame_rate: ProgramFrameRate,
    playback_clock: PlaybackClockPolicy,
    reference_output: PhysicalReferenceOutputPolicy,
    external_reference: ExternalReferencePolicy,
    hardware_timestamp: HardwareTimestampPolicy,
    export_policy: DisabledPolicy,
    recovery_policy: NoRecoveryPolicy,
}

impl PlaybackReferenceWorkload {
    fn validate(
        &self,
        requirement: &EndurancePhaseRequirement,
    ) -> Result<(), EnduranceWorkloadError> {
        validate_identity_and_duration(
            self.schema_version,
            &self.workload_id,
            &self.phase_id,
            self.wall_clock_hours,
            PLAYBACK_REFERENCE_WORKLOAD_ID,
            requirement,
        )?;
        let _exact_policies = (
            &self.program_frame_rate,
            &self.playback_clock,
            &self.reference_output,
            &self.external_reference,
            &self.hardware_timestamp,
            &self.export_policy,
            &self.recovery_policy,
        );
        validate_program_frame_counters(self.wall_clock_hours, requirement)?;
        if !requirement.counters.require_hardware_reference_output
            || !requirement.counters.require_external_reference_lock
            || requirement.counters.minimum_verified_exports != 0
            || requirement.counters.minimum_recovery_cycles != 0
        {
            return Err(EnduranceWorkloadError::RequirementMismatch(
                "playback/reference counter policy",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinuousExportWorkload {
    schema_version: u32,
    workload_id: String,
    phase_id: String,
    wall_clock_hours: u32,
    playback_policy: DisabledPolicy,
    reference_output: DisabledPolicy,
    export_policy: RepeatExportPolicy,
    artifact_validation: ArtifactValidationPolicy,
    cancellation_policy: CancellationPolicy,
    recovery_policy: NoRecoveryPolicy,
}

impl ContinuousExportWorkload {
    fn validate(
        &self,
        requirement: &EndurancePhaseRequirement,
    ) -> Result<(), EnduranceWorkloadError> {
        validate_identity_and_duration(
            self.schema_version,
            &self.workload_id,
            &self.phase_id,
            self.wall_clock_hours,
            CONTINUOUS_EXPORT_WORKLOAD_ID,
            requirement,
        )?;
        let _exact_policies = (
            &self.playback_policy,
            &self.reference_output,
            &self.export_policy,
            &self.artifact_validation,
            &self.cancellation_policy,
            &self.recovery_policy,
        );
        if requirement.counters.require_hardware_reference_output
            || requirement.counters.require_external_reference_lock
            || requirement.counters.minimum_verified_exports == 0
            || requirement.counters.minimum_recovery_cycles != 0
            || requirement.counters.minimum_export_cancellations != 0
            || requirement.counters.maximum_export_cancellations != 0
        {
            return Err(EnduranceWorkloadError::RequirementMismatch(
                "continuous Export counter policy",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConcurrentRecoveryWorkload {
    schema_version: u32,
    workload_id: String,
    phase_id: String,
    wall_clock_hours: u32,
    program_frame_rate: ProgramFrameRate,
    playback_clock: PlaybackClockPolicy,
    reference_output: PhysicalReferenceOutputPolicy,
    external_reference: ExternalReferencePolicy,
    export_policy: RepeatExportPolicy,
    artifact_validation: ArtifactValidationPolicy,
    recovery_cycle_count: u32,
    recovery_cycle: [WorkloadRecoveryStep; 4],
    export_cancellation_count: u32,
}

impl ConcurrentRecoveryWorkload {
    fn validate(
        &self,
        requirement: &EndurancePhaseRequirement,
    ) -> Result<(), EnduranceWorkloadError> {
        validate_identity_and_duration(
            self.schema_version,
            &self.workload_id,
            &self.phase_id,
            self.wall_clock_hours,
            CONCURRENT_RECOVERY_WORKLOAD_ID,
            requirement,
        )?;
        let _exact_policies = (
            &self.program_frame_rate,
            &self.playback_clock,
            &self.reference_output,
            &self.external_reference,
            &self.export_policy,
            &self.artifact_validation,
        );
        validate_program_frame_counters(self.wall_clock_hours, requirement)?;
        let exact_cycle = [
            WorkloadRecoveryStep::Seek,
            WorkloadRecoveryStep::SurfaceDeviceReopen,
            WorkloadRecoveryStep::ExportCancelRetry,
            WorkloadRecoveryStep::CachePressure,
        ];
        if self.recovery_cycle != exact_cycle
            || self.recovery_cycle_count == 0
            || u64::from(self.recovery_cycle_count) != requirement.counters.minimum_recovery_cycles
            || self.export_cancellation_count != self.recovery_cycle_count
            || u64::from(self.export_cancellation_count)
                != requirement.counters.minimum_export_cancellations
            || requirement.counters.minimum_export_cancellations
                != requirement.counters.maximum_export_cancellations
            || !requirement.counters.require_hardware_reference_output
            || !requirement.counters.require_external_reference_lock
            || requirement.counters.minimum_verified_exports == 0
        {
            return Err(EnduranceWorkloadError::RequirementMismatch(
                "concurrent recovery policy",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
enum ProgramFrameRate {
    #[serde(rename = "60/1")]
    Sixty,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlaybackClockPolicy {
    AudioDevice,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PhysicalReferenceOutputPolicy {
    PhysicalRequired,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ExternalReferencePolicy {
    ContinuousLockRequired,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum HardwareTimestampPolicy {
    EveryCompletion,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum DisabledPolicy {
    Disabled,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RepeatExportPolicy {
    RepeatFrozenSequence,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ArtifactValidationPolicy {
    IndependentReopenContentVerify,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CancellationPolicy {
    Forbidden,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum NoRecoveryPolicy {
    None,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum WorkloadRecoveryStep {
    Seek,
    SurfaceDeviceReopen,
    ExportCancelRetry,
    CachePressure,
}

fn read_bounded_regular_file(path: &Path) -> Result<Vec<u8>, EnduranceWorkloadError> {
    let link = fs::symlink_metadata(path)
        .map_err(|error| EnduranceWorkloadError::InvalidFile(error.to_string()))?;
    if link.file_type().is_symlink() || !link.file_type().is_file() {
        return Err(EnduranceWorkloadError::InvalidFile(
            "path is not a regular leaf".to_owned(),
        ));
    }
    if link.len() > MAXIMUM_WORKLOAD_BYTES {
        return Err(EnduranceWorkloadError::TooLarge);
    }
    let bytes =
        fs::read(path).map_err(|error| EnduranceWorkloadError::InvalidFile(error.to_string()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAXIMUM_WORKLOAD_BYTES {
        return Err(EnduranceWorkloadError::TooLarge);
    }
    Ok(bytes)
}

fn parse_contract<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, EnduranceWorkloadError> {
    serde_json::from_slice(bytes)
        .map_err(|error| EnduranceWorkloadError::InvalidSchema(error.to_string()))
}

fn lower_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_identity_and_duration(
    schema_version: u32,
    workload_id: &str,
    phase_id: &str,
    wall_clock_hours: u32,
    expected_workload_id: &'static str,
    requirement: &EndurancePhaseRequirement,
) -> Result<(), EnduranceWorkloadError> {
    if schema_version != WORKLOAD_SCHEMA_VERSION
        || workload_id != expected_workload_id
        || phase_id != requirement.phase_id
    {
        return Err(EnduranceWorkloadError::RequirementMismatch(
            "schema/workload/phase identity",
        ));
    }
    if u64::from(wall_clock_hours).checked_mul(MICROS_PER_HOUR)
        != Some(requirement.minimum_duration_us)
    {
        return Err(EnduranceWorkloadError::RequirementMismatch(
            "wall-clock duration",
        ));
    }
    Ok(())
}

fn validate_program_frame_counters(
    wall_clock_hours: u32,
    requirement: &EndurancePhaseRequirement,
) -> Result<(), EnduranceWorkloadError> {
    let expected_frames = u64::from(wall_clock_hours)
        .checked_mul(3_600)
        .and_then(|seconds| seconds.checked_mul(EXACT_PROGRAM_FRAMES_PER_SECOND))
        .ok_or(EnduranceWorkloadError::RequirementMismatch(
            "program frame count overflow",
        ))?;
    if requirement.counters.minimum_playback_presented_frames != expected_frames
        || requirement.counters.minimum_reference_completed_frames != expected_frames
        || requirement.counters.minimum_reference_hardware_timestamps != expected_frames
    {
        return Err(EnduranceWorkloadError::RequirementMismatch(
            "60 fps program counter policy",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mondrian_platform::EnduranceQualificationProfile;

    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn profile() -> EnduranceQualificationProfile {
        serde_json::from_slice(
            &fs::read(root().join("tests/validation/commercial-endurance-qualification.json"))
                .expect("checked-in profile"),
        )
        .expect("profile schema")
    }

    fn workload_path(kind: EndurancePhaseKind) -> PathBuf {
        let name = match kind {
            EndurancePhaseKind::PlaybackReference => "playback-reference-v1.json",
            EndurancePhaseKind::ContinuousExport => "continuous-export-v1.json",
            EndurancePhaseKind::ConcurrentRecovery => "concurrent-recovery-v1.json",
        };
        root().join("tests/validation/endurance-workloads").join(name)
    }

    #[test]
    fn checked_in_workloads_are_typed_and_bound_to_the_compiled_profile() {
        for requirement in profile().phases {
            let workload =
                PreparedEnduranceWorkload::load(&requirement, &workload_path(requirement.kind))
                    .expect("typed checked-in workload");
            assert_eq!(workload.phase_id(), requirement.phase_id);
            assert_eq!(workload.kind(), requirement.kind);
            assert!(workload.workload_id().starts_with("mondrian-col047/"));
            assert_eq!(
                workload.recovery_cycle_count(),
                if requirement.kind == EndurancePhaseKind::ConcurrentRecovery {
                    24
                } else {
                    0
                }
            );
        }
    }

    #[test]
    fn missing_capabilities_form_an_exact_not_run_receipt_before_start() {
        let requirement = profile()
            .phases
            .into_iter()
            .find(|phase| phase.kind == EndurancePhaseKind::ConcurrentRecovery)
            .expect("recovery requirement");
        let workload =
            PreparedEnduranceWorkload::load(&requirement, &workload_path(requirement.kind))
                .expect("typed recovery workload");
        let inventory = EnduranceWorkloadCapabilityInventory::new([
            EnduranceWorkloadCapability::TimelinePlaybackFixture,
            EnduranceWorkloadCapability::FrozenExportFixture,
        ]);

        let EndurancePhaseAdmission::NotRun(receipt) = workload.admit(&inventory) else {
            panic!("missing providers must not start product work");
        };
        assert_eq!(receipt.phase_id(), requirement.phase_id);
        assert_eq!(receipt.workload_id(), workload.workload_id());
        assert_eq!(
            receipt.missing_capabilities(),
            &[
                EnduranceWorkloadCapability::AudioOutputDevice,
                EnduranceWorkloadCapability::PhysicalReferenceOutput,
                EnduranceWorkloadCapability::ExternalReferenceLock,
                EnduranceWorkloadCapability::IndependentExportVerifier,
                EnduranceWorkloadCapability::SeekRecovery,
                EnduranceWorkloadCapability::SurfaceDeviceReopenRecovery,
                EnduranceWorkloadCapability::ExportCancelRetryRecovery,
                EnduranceWorkloadCapability::CachePressureRecovery,
            ]
        );
    }

    #[test]
    fn complete_capability_inventory_starts_the_exact_workload() {
        let requirement = profile()
            .phases
            .into_iter()
            .find(|phase| phase.kind == EndurancePhaseKind::ContinuousExport)
            .expect("Export requirement");
        let workload =
            PreparedEnduranceWorkload::load(&requirement, &workload_path(requirement.kind))
                .expect("typed Export workload");
        let inventory = EnduranceWorkloadCapabilityInventory::new([
            EnduranceWorkloadCapability::FrozenExportFixture,
            EnduranceWorkloadCapability::IndependentExportVerifier,
        ]);
        assert_eq!(workload.admit(&inventory), EndurancePhaseAdmission::Started);
    }

    #[test]
    fn malformed_or_wrong_phase_workload_is_an_error_not_not_run() {
        let mut phases = profile().phases;
        let requirement = phases.remove(0);
        let wrong = workload_path(EndurancePhaseKind::ContinuousExport);
        assert!(matches!(
            PreparedEnduranceWorkload::load(&requirement, &wrong),
            Err(EnduranceWorkloadError::DigestMismatch)
        ));

        let temporary = tempfile::NamedTempFile::new().expect("temporary workload");
        fs::write(temporary.path(), b"{}").expect("write malformed workload");
        let mut matching_digest_requirement = requirement;
        matching_digest_requirement.workload_sha256 = lower_sha256(b"{}");
        assert!(matches!(
            PreparedEnduranceWorkload::load(&matching_digest_requirement, temporary.path()),
            Err(EnduranceWorkloadError::InvalidSchema(_))
        ));
    }
}
