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

const PLAYBACK_REFERENCE_WORKLOAD_ID: &str = "mondrian-col047/playback-reference/v1";
const CONTINUOUS_EXPORT_WORKLOAD_ID: &str = "mondrian-col047/continuous-export/v1";
const CONCURRENT_RECOVERY_WORKLOAD_ID: &str = "mondrian-col047/concurrent-recovery/v1";

/// One side-effect-free prerequisite observable before phase owners start.
///
/// These variants never claim that a device Session is open or that a signal
/// remains locked. Dynamic provider/readback facts are proved by the started
/// product owners after this admission boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EndurancePreStartCapability {
    /// Loader-before-execution native object authority was attested for this process.
    PreloaderMappedImageIdentityPrepared,
    /// Timeline picture/audio fixture bindings were declared and are reachable for later build.
    TimelinePlaybackFixtureDeclared,
    /// Sequence/source Export fixture bindings were declared for later exact build validation.
    FrozenExportFixtureDeclared,
    /// One exact audio-device contract was prepared without opening its stream.
    AudioOutputDevicePrepared,
    /// Physical provider/runtime/device/mode discovery was prepared without a Session.
    PhysicalReferenceProviderPrepared,
    /// External-reference signal was observed during preflight, not promised continuously.
    ExternalReferenceSignalPreflight,
    /// Pinned independent verifier identity and execution contract were prepared.
    IndependentExportVerifierPrepared,
    /// Externally approved PSE provider and fixed QC profile are held before phase owners start.
    RegulatoryPseProviderPrepared,
    /// Exact BMX executable, version and complete runtime owners were admitted.
    ApprovedBmxRuntimePrepared,
    /// Declared canonical ANC source is validated and retained before phase creation.
    FrozenAncillaryProgramPrepared,
    /// Product seek recovery contract and target inventory were prepared.
    SeekRecoveryPrepared,
    /// Process-local Surface event-loop recovery owner was prepared.
    SurfaceEventLoopPrepared,
    /// Export cancel/retry contract was prepared.
    ExportCancelRetryPrepared,
    /// Bounded cache pressure/recovery contract was prepared.
    CachePressurePrepared,
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
    missing_capabilities: Vec<EndurancePreStartCapability>,
}

impl EnduranceNotRunAdmission {
    pub(crate) fn missing_bmx(phase_id: String, workload_id: String) -> Self {
        Self::new(
            phase_id,
            workload_id,
            vec![EndurancePreStartCapability::ApprovedBmxRuntimePrepared],
        )
    }
    pub(crate) fn missing_ancillary(phase_id: String, workload_id: String) -> Self {
        Self::new(
            phase_id,
            workload_id,
            vec![EndurancePreStartCapability::FrozenAncillaryProgramPrepared],
        )
    }
    pub(crate) fn missing_regulatory_pse(phase_id: String, workload_id: String) -> Self {
        Self::new(
            phase_id,
            workload_id,
            vec![EndurancePreStartCapability::RegulatoryPseProviderPrepared],
        )
    }

    fn new(
        phase_id: String,
        workload_id: String,
        missing_capabilities: Vec<EndurancePreStartCapability>,
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
    pub fn missing_capabilities(&self) -> &[EndurancePreStartCapability] {
        &self.missing_capabilities
    }
}

/// Exact capability inventory observed before attempting phase owner creation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndurancePreStartCapabilityInventory {
    capabilities: BTreeSet<EndurancePreStartCapability>,
}

impl EndurancePreStartCapabilityInventory {
    pub(crate) fn admit(&mut self, capability: EndurancePreStartCapability) {
        self.capabilities.insert(capability);
    }
    pub(crate) fn revoke(&mut self, capability: EndurancePreStartCapability) {
        self.capabilities.remove(&capability);
    }
    /// Construct an inventory from independently observed product capabilities.
    pub fn new(capabilities: impl IntoIterator<Item = EndurancePreStartCapability>) -> Self {
        Self { capabilities: capabilities.into_iter().collect() }
    }

    fn missing(
        &self,
        required: &[EndurancePreStartCapability],
    ) -> Vec<EndurancePreStartCapability> {
        required
            .iter()
            .copied()
            .filter(|capability| !self.capabilities.contains(capability))
            .collect()
    }
}

/// Runtime-created token binding one admitted pre-start inventory to its exact workload.
///
/// Fields are private so a machine factory can consume this token but cannot
/// fabricate or retarget it to a different phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEndurancePhaseStart {
    phase_id: String,
    workload_id: String,
    kind: EndurancePhaseKind,
}

impl PreparedEndurancePhaseStart {
    /// Exact phase admitted by the runtime.
    pub fn phase_id(&self) -> &str {
        &self.phase_id
    }

    /// Exact checked workload admitted by the runtime.
    pub fn workload_id(&self) -> &str {
        &self.workload_id
    }

    /// Phase family bound to this one-use factory call.
    pub const fn kind(&self) -> EndurancePhaseKind {
        self.kind
    }
}

/// Parsed, hash-bound workload contract safe to hand to a concrete runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEnduranceWorkload {
    phase_id: String,
    workload_id: String,
    kind: EndurancePhaseKind,
    required_capabilities: Vec<EndurancePreStartCapability>,
    recovery_cycle_count: u32,
    program_frame_rate: Option<mondrian_core::Rational>,
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
                        EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared,
                        EndurancePreStartCapability::TimelinePlaybackFixtureDeclared,
                        EndurancePreStartCapability::AudioOutputDevicePrepared,
                        EndurancePreStartCapability::PhysicalReferenceProviderPrepared,
                        EndurancePreStartCapability::ExternalReferenceSignalPreflight,
                    ],
                    recovery_cycle_count: 0,
                    program_frame_rate: Some(contract.program_frame_rate.rate()),
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
                        EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared,
                        EndurancePreStartCapability::FrozenExportFixtureDeclared,
                        EndurancePreStartCapability::IndependentExportVerifierPrepared,
                    ],
                    recovery_cycle_count: 0,
                    program_frame_rate: None,
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
                        EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared,
                        EndurancePreStartCapability::TimelinePlaybackFixtureDeclared,
                        EndurancePreStartCapability::FrozenExportFixtureDeclared,
                        EndurancePreStartCapability::AudioOutputDevicePrepared,
                        EndurancePreStartCapability::PhysicalReferenceProviderPrepared,
                        EndurancePreStartCapability::ExternalReferenceSignalPreflight,
                        EndurancePreStartCapability::IndependentExportVerifierPrepared,
                        EndurancePreStartCapability::SeekRecoveryPrepared,
                        EndurancePreStartCapability::SurfaceEventLoopPrepared,
                        EndurancePreStartCapability::ExportCancelRetryPrepared,
                        EndurancePreStartCapability::CachePressurePrepared,
                    ],
                    recovery_cycle_count: contract.recovery_cycle_count,
                    program_frame_rate: Some(contract.program_frame_rate.rate()),
                })
            }
        }
    }

    /// Exact physical program rate declared by this hash-bound workload.
    pub const fn program_frame_rate(&self) -> Option<mondrian_core::Rational> {
        self.program_frame_rate
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

    /// Prepare a bound factory token only when every declaration/pre-start fact was observed.
    pub fn prepare_start(
        &self,
        inventory: &EndurancePreStartCapabilityInventory,
    ) -> Result<PreparedEndurancePhaseStart, EnduranceNotRunAdmission> {
        let missing = inventory.missing(&self.required_capabilities);
        if missing.is_empty() {
            Ok(PreparedEndurancePhaseStart {
                phase_id: self.phase_id.clone(),
                workload_id: self.workload_id.clone(),
                kind: self.kind,
            })
        } else {
            Err(EnduranceNotRunAdmission::new(
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
        validate_program_frame_counters(
            self.wall_clock_hours,
            self.program_frame_rate.rate(),
            requirement,
        )?;
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
        validate_program_frame_counters(
            self.wall_clock_hours,
            self.program_frame_rate.rate(),
            requirement,
        )?;
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

#[derive(Debug, Clone, Copy, Deserialize)]
enum ProgramFrameRate {
    #[serde(rename = "60/1")]
    Sixty,
    #[serde(rename = "60000/1001")]
    SixtyThousandOver1001,
}
impl ProgramFrameRate {
    fn rate(self) -> mondrian_core::Rational {
        match self {
            Self::Sixty => mondrian_core::Rational::new(60, 1),
            Self::SixtyThousandOver1001 => mondrian_core::Rational::new(60000, 1001),
        }
    }
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
    rate: mondrian_core::Rational,
    requirement: &EndurancePhaseRequirement,
) -> Result<(), EnduranceWorkloadError> {
    let expected_frames = u64::from(wall_clock_hours)
        .checked_mul(3_600)
        .and_then(|seconds| seconds.checked_mul(u64::try_from(rate.num).ok()?))
        .and_then(|numerator| numerator.checked_div(u64::try_from(rate.den).ok()?))
        .ok_or(EnduranceWorkloadError::RequirementMismatch(
            "program frame count overflow",
        ))?;
    if requirement.counters.minimum_playback_presented_frames != expected_frames
        || requirement.counters.minimum_reference_completed_frames != expected_frames
        || requirement.counters.minimum_reference_hardware_timestamps != expected_frames
    {
        return Err(EnduranceWorkloadError::RequirementMismatch(
            "exact rational program counter policy",
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
    fn checked_in_5994_workloads_preserve_exact_rational_counter_policy() {
        let profile: EnduranceQualificationProfile = serde_json::from_slice(
            &fs::read(
                root().join("tests/validation/commercial-endurance-qualification-as11-5994.json"),
            )
            .expect("profile bytes"),
        )
        .expect("profile");
        for mut requirement in profile.phases {
            let stem = match requirement.kind {
                EndurancePhaseKind::PlaybackReference => "playback-reference",
                EndurancePhaseKind::ContinuousExport => "continuous-export",
                EndurancePhaseKind::ConcurrentRecovery => "concurrent-recovery",
            };
            let path = root().join(format!(
                "tests/validation/endurance-workloads/{stem}-as11-5994-v1.json"
            ));
            let workload =
                PreparedEnduranceWorkload::load(&requirement, &path).expect("5994 workload");
            if requirement.kind != EndurancePhaseKind::ContinuousExport {
                assert_eq!(
                    workload.program_frame_rate(),
                    Some(mondrian_core::Rational::new(60000, 1001))
                );
                assert_eq!(
                    requirement.counters.minimum_reference_completed_frames,
                    5_178_821
                );
                requirement.counters.minimum_reference_completed_frames += 1;
                assert!(PreparedEnduranceWorkload::load(&requirement, &path).is_err());
            }
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
        let inventory = EndurancePreStartCapabilityInventory::new([
            EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared,
            EndurancePreStartCapability::TimelinePlaybackFixtureDeclared,
            EndurancePreStartCapability::FrozenExportFixtureDeclared,
        ]);

        let receipt = workload
            .prepare_start(&inventory)
            .expect_err("missing providers must not start");
        assert_eq!(receipt.phase_id(), requirement.phase_id);
        assert_eq!(receipt.workload_id(), workload.workload_id());
        assert_eq!(
            receipt.missing_capabilities(),
            &[
                EndurancePreStartCapability::AudioOutputDevicePrepared,
                EndurancePreStartCapability::PhysicalReferenceProviderPrepared,
                EndurancePreStartCapability::ExternalReferenceSignalPreflight,
                EndurancePreStartCapability::IndependentExportVerifierPrepared,
                EndurancePreStartCapability::SeekRecoveryPrepared,
                EndurancePreStartCapability::SurfaceEventLoopPrepared,
                EndurancePreStartCapability::ExportCancelRetryPrepared,
                EndurancePreStartCapability::CachePressurePrepared,
            ]
        );
    }

    #[test]
    fn complete_pre_start_inventory_produces_an_exact_bound_factory_token() {
        let requirement = profile()
            .phases
            .into_iter()
            .find(|phase| phase.kind == EndurancePhaseKind::ContinuousExport)
            .expect("Export requirement");
        let workload =
            PreparedEnduranceWorkload::load(&requirement, &workload_path(requirement.kind))
                .expect("typed Export workload");
        let inventory = EndurancePreStartCapabilityInventory::new([
            EndurancePreStartCapability::PreloaderMappedImageIdentityPrepared,
            EndurancePreStartCapability::FrozenExportFixtureDeclared,
            EndurancePreStartCapability::IndependentExportVerifierPrepared,
        ]);
        let prepared = workload.prepare_start(&inventory).expect("prepared start");
        assert_eq!(prepared.phase_id(), workload.phase_id());
        assert_eq!(prepared.workload_id(), workload.workload_id());
        assert_eq!(prepared.kind(), EndurancePhaseKind::ContinuousExport);
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
