//! Exact machine-local fixture and device plan for commercial endurance runs.

use std::collections::BTreeSet;
use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use mondrian_core::{AudioChannelLayout, SequenceId};
use mondrian_media::RealtimeAudioOutputDeviceId;
use mondrian_platform::{EndurancePhaseKind, EnduranceQualificationProfile};
use mondrian_reference_output::{
    ReferenceOutputAncillaryPolicy, ReferenceOutputDeviceId, ReferenceOutputOpenRequest,
    ReferenceOutputProvider, ReferenceOutputReferencePolicy, ReferenceOutputSignal,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const MACHINE_PLAN_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_MACHINE_PLAN_BYTES: u64 = 256 * 1024;
const MAXIMUM_PATH_BYTES: usize = 4 * 1024;
const MAXIMUM_IDENTITY_BYTES: usize = 128;
const MAXIMUM_SEEK_TARGETS: usize = 128;
const MAXIMUM_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1_000;
const MAXIMUM_ARTIFACT_BYTES: u64 = 1 << 40;

/// One absolute machine-local file and its externally approved byte identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceMachineFileBinding {
    /// Absolute path used by the qualification machine.
    pub path: PathBuf,
    /// Lowercase SHA-256 of the exact file bytes.
    pub sha256: String,
}

/// Exact Project, Sequence, and external-source inventory selected for the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceMachineProjectPlan {
    /// Existing canonical `.mdp` file and its byte identity.
    pub project: EnduranceMachineFileBinding,
    /// Active Sequence used by every qualified phase.
    pub sequence_id: SequenceId,
    /// Canonical external-source inventory report and digest.
    pub external_source_inventory: EnduranceMachineFileBinding,
}

/// Exact realtime Audio Device Clock contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceMachineAudioPlan {
    /// Stable physical output-device identity; never the mutable system default.
    pub device_id: RealtimeAudioOutputDeviceId,
    /// Required hardware sample rate. Commercial endurance requires 48 kHz.
    pub sample_rate_hz: u32,
    /// Exact semantic channel layout admitted by the selected device.
    pub channel_layout: AudioChannelLayout,
}

/// Exact physical Reference Output route and signal request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceMachineReferenceOpenRequest {
    /// Exact signal requested from the selected device.
    pub signal: ReferenceOutputSignal,
    /// External reference-lock policy.
    pub reference_policy: ReferenceOutputReferencePolicy,
    /// Explicit ANC scheduling/readback contract; omission is never accepted.
    pub ancillary_policy: ReferenceOutputAncillaryPolicy,
    /// Minimum video-frame preroll before playback starts.
    pub preroll_frames: u32,
    /// Bounded outstanding scheduled-frame limit.
    pub max_scheduled_frames: u32,
}

impl EnduranceMachineReferenceOpenRequest {
    /// Lower the explicit machine-plan schema into the production open request.
    pub fn production_request(&self) -> ReferenceOutputOpenRequest {
        ReferenceOutputOpenRequest {
            signal: self.signal.clone(),
            reference_policy: self.reference_policy,
            ancillary_policy: self.ancillary_policy,
            preroll_frames: self.preroll_frames,
            max_scheduled_frames: self.max_scheduled_frames,
        }
    }
}

/// Exact physical Reference Output route and signal request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceMachineReferencePlan {
    /// Physical vendor provider family.
    pub provider: ReferenceOutputProvider,
    /// Stable provider-owned device identity.
    pub device_id: ReferenceOutputDeviceId,
    /// Discovery generation that must still be current when the Session opens.
    pub device_generation: u64,
    /// Complete signal, external-reference, ANC, preroll, and queue contract.
    pub open_request: EnduranceMachineReferenceOpenRequest,
    /// First absolute Sequence frame scheduled into the provider.
    pub first_frame_index: u64,
}

/// Create-only publication authority for every repeated Export artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnduranceMachineExportOutputPolicy {
    /// Reject any artifact path that already exists.
    CreateNew,
}

/// Exact Timeline selection encoded without permissive nested enum fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EnduranceMachineExportRange {
    /// Use authored Sequence In/Out points.
    SequenceInOut,
    /// Export the entire root Sequence extent.
    EntireSequence,
    /// Export one explicit half-open root frame range.
    WorkArea {
        /// First included root frame.
        start_frame: i64,
        /// First excluded root frame.
        end_frame_exclusive: i64,
    },
}

impl EnduranceMachineExportRange {
    /// Lower the sealed machine-plan range into the production Export contract.
    pub const fn timeline_range(self) -> mondrian_export::TimelineExportRange {
        match self {
            Self::SequenceInOut => mondrian_export::TimelineExportRange::SequenceInOut,
            Self::EntireSequence => mondrian_export::TimelineExportRange::EntireSequence,
            Self::WorkArea { start_frame, end_frame_exclusive } => {
                mondrian_export::TimelineExportRange::WorkArea { start_frame, end_frame_exclusive }
            }
        }
    }

    fn is_structurally_valid(self) -> bool {
        match self {
            Self::SequenceInOut | Self::EntireSequence => true,
            Self::WorkArea { start_frame, end_frame_exclusive } => {
                start_frame >= 0 && end_frame_exclusive > start_frame
            }
        }
    }
}

/// Phase-specific frozen Export and independent verification plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceMachineExportPlan {
    /// Exact profile phase receiving this Export plan.
    pub phase_id: String,
    /// Exact serialized `ExportPreset` approved for this phase.
    pub preset: EnduranceMachineFileBinding,
    /// Exact Sequence frozen into every Export attempt.
    pub sequence_id: SequenceId,
    /// Exact Timeline selection resolved by the Export snapshot.
    pub range: EnduranceMachineExportRange,
    /// Existing canonical directory receiving only create-new artifacts.
    pub output_directory: PathBuf,
    /// Link-free prefix combined with the monotonically increasing ordinal.
    pub artifact_prefix: String,
    /// Optional exact Broadcast QC profile.
    pub broadcast_qc: Option<EnduranceMachineFileBinding>,
    /// Explicit publication policy; commercial evidence is always create-only.
    pub output_policy: EnduranceMachineExportOutputPolicy,
    /// Largest artifact admitted for hashing and full decode.
    pub maximum_artifact_bytes: u64,
    /// Non-renewing wall-clock bound for independent full decode.
    pub decode_timeout_ms: u64,
}

/// Pinned executable plus independently captured version/capability identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceMachineToolPlan {
    /// Exact executable image.
    pub executable: EnduranceMachineFileBinding,
    /// SHA-256 of the bounded raw `-version` output approved for this run.
    pub version_output_sha256: String,
    /// SHA-256 of the bounded capability report used by preflight.
    pub capability_report_sha256: String,
}

/// Exact external verifier toolchain used by Export and media probes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceMachineVerifierTools {
    /// Pinned FFmpeg executable and capability identity.
    pub ffmpeg: EnduranceMachineToolPlan,
    /// Pinned FFprobe executable and capability identity.
    pub ffprobe: EnduranceMachineToolPlan,
}

/// Non-renewing execution bounds frozen into the approved machine plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceMachineTimeoutPlan {
    /// Maximum latency of one ordinary product pump interval.
    pub interval_ms: u64,
    /// Maximum latency of one complete four-step recovery cycle.
    pub recovery_ms: u64,
    /// Maximum latency of one native Window/Surface/Device reopen operation.
    pub surface_reopen_ms: u64,
    /// Single consuming shutdown deadline for one phase.
    pub shutdown_ms: u64,
}

/// Bounded machine-local plan whose exact bytes are a qualification trust anchor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommercialEnduranceMachinePlan {
    /// Machine-plan schema. Version 1 is required.
    pub schema_version: u32,
    /// Stable operator-assigned plan identity.
    pub plan_id: String,
    /// Exact Project/Sequence/source inventory.
    pub project: EnduranceMachineProjectPlan,
    /// Exact physical realtime audio contract.
    pub audio: EnduranceMachineAudioPlan,
    /// Exact physical Reference Output contract.
    pub reference_output: EnduranceMachineReferencePlan,
    /// One phase-specific plan for Continuous Export and Concurrent Recovery.
    pub exports: Vec<EnduranceMachineExportPlan>,
    /// Ordered, distinct absolute frame targets for every recovery cycle.
    pub recovery_seek_targets: Vec<i64>,
    /// Pinned independent FFmpeg/FFprobe toolchain.
    pub verifier_tools: EnduranceMachineVerifierTools,
    /// Exact non-renewing runtime bounds.
    pub timeouts: EnduranceMachineTimeoutPlan,
}

/// Validated plan together with the SHA-256 of its original JSON bytes.
#[derive(Debug, Clone)]
pub struct PreparedCommercialEnduranceMachinePlan {
    plan: CommercialEnduranceMachinePlan,
    sha256: String,
}

impl PreparedCommercialEnduranceMachinePlan {
    /// Load one bounded regular JSON file and validate it against the exact profile/workload.
    pub fn load(
        path: &Path,
        profile: &EnduranceQualificationProfile,
        recovery_cycle_count: u32,
    ) -> Result<Self, CommercialEnduranceMachinePlanError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| CommercialEnduranceMachinePlanError::Read(error.to_string()))?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(CommercialEnduranceMachinePlanError::NotRegularFile);
        }
        if metadata.len() == 0 || metadata.len() > MAXIMUM_MACHINE_PLAN_BYTES {
            return Err(CommercialEnduranceMachinePlanError::InvalidSize {
                actual: metadata.len(),
            });
        }
        let mut file = File::open(path)
            .map_err(|error| CommercialEnduranceMachinePlanError::Read(error.to_string()))?;
        let opened_metadata = file
            .metadata()
            .map_err(|error| CommercialEnduranceMachinePlanError::Read(error.to_string()))?;
        if !opened_metadata.is_file()
            || opened_metadata.len() == 0
            || opened_metadata.len() > MAXIMUM_MACHINE_PLAN_BYTES
        {
            return Err(CommercialEnduranceMachinePlanError::InvalidSize {
                actual: opened_metadata.len(),
            });
        }
        let mut bytes =
            Vec::with_capacity(usize::try_from(opened_metadata.len()).map_err(|_| {
                CommercialEnduranceMachinePlanError::InvalidSize { actual: opened_metadata.len() }
            })?);
        Read::by_ref(&mut file)
            .take(MAXIMUM_MACHINE_PLAN_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| CommercialEnduranceMachinePlanError::Read(error.to_string()))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != opened_metadata.len() {
            return Err(CommercialEnduranceMachinePlanError::InvalidSize {
                actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            });
        }
        let plan: CommercialEnduranceMachinePlan =
            serde_json::from_slice(&bytes).map_err(CommercialEnduranceMachinePlanError::Json)?;
        validate_plan(&plan, profile, recovery_cycle_count)?;
        Ok(Self {
            plan,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        })
    }

    /// Validated strongly typed machine plan.
    pub const fn plan(&self) -> &CommercialEnduranceMachinePlan {
        &self.plan
    }

    /// Lowercase SHA-256 of the exact approved JSON bytes.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

fn validate_plan(
    plan: &CommercialEnduranceMachinePlan,
    profile: &EnduranceQualificationProfile,
    recovery_cycle_count: u32,
) -> Result<(), CommercialEnduranceMachinePlanError> {
    if plan.schema_version != MACHINE_PLAN_SCHEMA_VERSION {
        return Err(CommercialEnduranceMachinePlanError::UnsupportedSchema {
            actual: plan.schema_version,
        });
    }
    validate_identity(&plan.plan_id, "plan_id")?;
    validate_file_binding(&plan.project.project, "project")?;
    validate_file_binding(
        &plan.project.external_source_inventory,
        "external_source_inventory",
    )?;
    if plan.audio.sample_rate_hz != 48_000 || plan.audio.channel_layout.channel_count() > 16 {
        return Err(CommercialEnduranceMachinePlanError::InvalidAudioContract);
    }
    if plan.reference_output.provider == ReferenceOutputProvider::Simulated
        || plan.reference_output.device_generation == 0
        || plan.reference_output.open_request.reference_policy
            != ReferenceOutputReferencePolicy::RequireExternalLock
        || plan.reference_output.open_request.production_request().validate().is_err()
    {
        return Err(CommercialEnduranceMachinePlanError::InvalidReferenceContract);
    }
    validate_tool(&plan.verifier_tools.ffmpeg, "ffmpeg")?;
    validate_tool(&plan.verifier_tools.ffprobe, "ffprobe")?;
    validate_timeouts(plan.timeouts)?;

    let export_phase_ids = profile
        .phases
        .iter()
        .filter(|phase| {
            matches!(
                phase.kind,
                EndurancePhaseKind::ContinuousExport | EndurancePhaseKind::ConcurrentRecovery
            )
        })
        .map(|phase| phase.phase_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut observed_export_phase_ids = BTreeSet::new();
    for export in &plan.exports {
        validate_identity(&export.phase_id, "export.phase_id")?;
        if !export_phase_ids.contains(export.phase_id.as_str())
            || !observed_export_phase_ids.insert(export.phase_id.as_str())
            || export.sequence_id != plan.project.sequence_id
        {
            return Err(CommercialEnduranceMachinePlanError::ExportPhaseClosure);
        }
        validate_file_binding(&export.preset, "export.preset")?;
        if let Some(qc) = &export.broadcast_qc {
            validate_file_binding(qc, "export.broadcast_qc")?;
        }
        validate_absolute_path(&export.output_directory, "export.output_directory")?;
        if export.artifact_prefix.is_empty()
            || export.artifact_prefix.len() > 64
            || matches!(export.artifact_prefix.as_str(), "." | "..")
            || !export
                .artifact_prefix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            || export.maximum_artifact_bytes == 0
            || export.maximum_artifact_bytes > MAXIMUM_ARTIFACT_BYTES
            || export.decode_timeout_ms == 0
            || export.decode_timeout_ms > MAXIMUM_TIMEOUT_MS
            || !export.range.is_structurally_valid()
        {
            return Err(CommercialEnduranceMachinePlanError::InvalidExportContract {
                phase_id: export.phase_id.clone(),
            });
        }
    }
    if observed_export_phase_ids != export_phase_ids {
        return Err(CommercialEnduranceMachinePlanError::ExportPhaseClosure);
    }

    if recovery_cycle_count == 0
        || usize::try_from(recovery_cycle_count).ok() != Some(plan.recovery_seek_targets.len())
        || plan.recovery_seek_targets.len() > MAXIMUM_SEEK_TARGETS
        || plan.recovery_seek_targets.iter().any(|target| *target < 0)
        || plan.recovery_seek_targets.iter().copied().collect::<BTreeSet<_>>().len()
            != plan.recovery_seek_targets.len()
    {
        return Err(CommercialEnduranceMachinePlanError::InvalidSeekTargets {
            expected: recovery_cycle_count,
            actual: plan.recovery_seek_targets.len(),
        });
    }
    Ok(())
}

fn validate_file_binding(
    binding: &EnduranceMachineFileBinding,
    field: &'static str,
) -> Result<(), CommercialEnduranceMachinePlanError> {
    validate_absolute_path(&binding.path, field)?;
    validate_sha256(&binding.sha256, field)
}

fn validate_tool(
    tool: &EnduranceMachineToolPlan,
    field: &'static str,
) -> Result<(), CommercialEnduranceMachinePlanError> {
    validate_file_binding(&tool.executable, field)?;
    validate_sha256(&tool.version_output_sha256, field)?;
    validate_sha256(&tool.capability_report_sha256, field)
}

fn validate_timeouts(
    timeouts: EnduranceMachineTimeoutPlan,
) -> Result<(), CommercialEnduranceMachinePlanError> {
    if [
        timeouts.interval_ms,
        timeouts.recovery_ms,
        timeouts.surface_reopen_ms,
        timeouts.shutdown_ms,
    ]
    .into_iter()
    .any(|value| value == 0 || value > MAXIMUM_TIMEOUT_MS)
    {
        return Err(CommercialEnduranceMachinePlanError::InvalidTimeouts);
    }
    Ok(())
}

fn validate_absolute_path(
    path: &Path,
    field: &'static str,
) -> Result<(), CommercialEnduranceMachinePlanError> {
    let Some(text) = path.to_str() else {
        return Err(CommercialEnduranceMachinePlanError::InvalidPath { field });
    };
    if !path.is_absolute()
        || text.is_empty()
        || text.len() > MAXIMUM_PATH_BYTES
        || text.chars().any(char::is_control)
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(CommercialEnduranceMachinePlanError::InvalidPath { field });
    }
    Ok(())
}

fn validate_identity(
    value: &str,
    field: &'static str,
) -> Result<(), CommercialEnduranceMachinePlanError> {
    if value.is_empty()
        || value.len() > MAXIMUM_IDENTITY_BYTES
        || value.contains("placeholder")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
    {
        return Err(CommercialEnduranceMachinePlanError::InvalidIdentity { field });
    }
    Ok(())
}

fn validate_sha256(
    value: &str,
    field: &'static str,
) -> Result<(), CommercialEnduranceMachinePlanError> {
    if value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(CommercialEnduranceMachinePlanError::InvalidSha256 { field });
    }
    Ok(())
}

/// Structural or profile-binding failure in an approved machine plan.
#[derive(Debug, Error)]
pub enum CommercialEnduranceMachinePlanError {
    /// Machine-plan path could not be inspected or read.
    #[error("could not read commercial endurance machine plan: {0}")]
    Read(String),
    /// Plan path did not identify a direct regular file.
    #[error("commercial endurance machine plan must be a regular non-link file")]
    NotRegularFile,
    /// Plan JSON was empty or exceeded the fixed parser bound.
    #[error("commercial endurance machine plan size {actual} is outside the supported bound")]
    InvalidSize { actual: u64 },
    /// Strict JSON parsing failed.
    #[error("invalid commercial endurance machine-plan JSON: {0}")]
    Json(serde_json::Error),
    /// Machine-plan schema is unsupported.
    #[error("unsupported commercial endurance machine-plan schema {actual}; expected 1")]
    UnsupportedSchema { actual: u32 },
    /// Identity token was empty, placeholder, oversized, or unsafe.
    #[error("invalid commercial endurance machine-plan identity '{field}'")]
    InvalidIdentity { field: &'static str },
    /// An approved file digest was malformed.
    #[error("commercial endurance machine-plan field '{field}' must be lowercase SHA-256")]
    InvalidSha256 { field: &'static str },
    /// A plan path was not a bounded absolute normalized path.
    #[error("commercial endurance machine-plan path '{field}' is invalid")]
    InvalidPath { field: &'static str },
    /// Audio was not an exact supported 48 kHz semantic layout.
    #[error("commercial endurance machine plan requires an exact 48 kHz audio contract")]
    InvalidAudioContract,
    /// Reference Output was simulated, stale, invalid, or lacked required external lock.
    #[error("commercial endurance machine plan lacks an exact physical Reference Output contract")]
    InvalidReferenceContract,
    /// Export plans did not exactly cover the Export-bearing profile phases.
    #[error("commercial endurance machine-plan Export phases do not close over the profile")]
    ExportPhaseClosure,
    /// One phase-specific Export plan violated publication or verifier bounds.
    #[error("invalid commercial endurance Export plan for phase '{phase_id}'")]
    InvalidExportContract { phase_id: String },
    /// Recovery seek target count/order did not match the workload.
    #[error(
        "commercial endurance recovery requires {expected} distinct seek targets, got {actual}"
    )]
    InvalidSeekTargets { expected: u32, actual: usize },
    /// Runtime bounds were zero or exceeded one day per operation.
    #[error("commercial endurance machine-plan timeouts are invalid")]
    InvalidTimeouts,
}

#[cfg(test)]
pub(crate) fn write_test_machine_plan(
    path: &Path,
    fixture_root: &Path,
    profile: &EnduranceQualificationProfile,
    recovery_cycle_count: u32,
) -> String {
    use mondrian_core::{ColorSpace, Rational};
    use mondrian_reference_output::{
        ReferenceOutputAncillaryPolicy, ReferenceOutputPixelFormat, ReferenceOutputRange,
        ReferenceOutputScan, ReferenceOutputSignal,
    };

    let sequence_id = SequenceId::new();
    let binding = |name: &str| EnduranceMachineFileBinding {
        path: fixture_root.join(name),
        sha256: "a".repeat(64),
    };
    let exports = profile
        .phases
        .iter()
        .filter(|phase| {
            matches!(
                phase.kind,
                EndurancePhaseKind::ContinuousExport | EndurancePhaseKind::ConcurrentRecovery
            )
        })
        .map(|phase| EnduranceMachineExportPlan {
            phase_id: phase.phase_id.clone(),
            preset: binding(&format!("{}-preset.json", phase.phase_id)),
            sequence_id,
            range: EnduranceMachineExportRange::EntireSequence,
            output_directory: fixture_root.join(format!("{}-output", phase.phase_id)),
            artifact_prefix: phase.phase_id.clone(),
            broadcast_qc: None,
            output_policy: EnduranceMachineExportOutputPolicy::CreateNew,
            maximum_artifact_bytes: 1 << 30,
            decode_timeout_ms: 60_000,
        })
        .collect();
    let plan = CommercialEnduranceMachinePlan {
        schema_version: 1,
        plan_id: "test-machine-plan".to_owned(),
        project: EnduranceMachineProjectPlan {
            project: binding("project.mdp"),
            sequence_id,
            external_source_inventory: binding("external-sources.json"),
        },
        audio: EnduranceMachineAudioPlan {
            device_id: RealtimeAudioOutputDeviceId::new("test:physical-device")
                .expect("test audio device"),
            sample_rate_hz: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
        },
        reference_output: EnduranceMachineReferencePlan {
            provider: ReferenceOutputProvider::DeckLink,
            device_id: ReferenceOutputDeviceId::new("decklink:test-device")
                .expect("test Reference device"),
            device_generation: 1,
            open_request: EnduranceMachineReferenceOpenRequest {
                signal: ReferenceOutputSignal {
                    width: 1920,
                    height: 1080,
                    frame_rate: Rational::FPS_25,
                    scan: ReferenceOutputScan::Progressive,
                    pixel_format: ReferenceOutputPixelFormat::Yuv422TenV210,
                    color_space: ColorSpace::Rec709,
                    range: ReferenceOutputRange::Legal,
                    hdr: None,
                    audio_layout: AudioChannelLayout::Stereo,
                },
                reference_policy: ReferenceOutputReferencePolicy::RequireExternalLock,
                ancillary_policy: ReferenceOutputAncillaryPolicy::Disabled,
                preroll_frames: 3,
                max_scheduled_frames: 5,
            },
            first_frame_index: 0,
        },
        exports,
        recovery_seek_targets: (0..recovery_cycle_count).map(i64::from).collect(),
        verifier_tools: EnduranceMachineVerifierTools {
            ffmpeg: EnduranceMachineToolPlan {
                executable: binding("ffmpeg.exe"),
                version_output_sha256: "b".repeat(64),
                capability_report_sha256: "c".repeat(64),
            },
            ffprobe: EnduranceMachineToolPlan {
                executable: binding("ffprobe.exe"),
                version_output_sha256: "d".repeat(64),
                capability_report_sha256: "e".repeat(64),
            },
        },
        timeouts: EnduranceMachineTimeoutPlan {
            interval_ms: 1_000,
            recovery_ms: 60_000,
            surface_reopen_ms: 30_000,
            shutdown_ms: 60_000,
        },
    };
    let bytes = serde_json::to_vec_pretty(&plan).expect("serialize test machine plan");
    fs::write(path, &bytes).expect("write test machine plan");
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> EnduranceQualificationProfile {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        serde_json::from_slice(
            &fs::read(root.join("tests/validation/commercial-endurance-qualification.json"))
                .expect("read profile"),
        )
        .expect("parse profile")
    }

    #[test]
    fn bounded_machine_plan_binds_exact_bytes_and_recovery_targets() {
        let temporary = tempfile::tempdir().expect("temporary machine plan");
        let path = temporary.path().join("machine-plan.json");
        let expected = write_test_machine_plan(&path, temporary.path(), &profile(), 24);
        let prepared = PreparedCommercialEnduranceMachinePlan::load(&path, &profile(), 24)
            .expect("load exact machine plan");
        assert_eq!(prepared.sha256(), expected);
        assert_eq!(prepared.plan().recovery_seek_targets.len(), 24);

        assert!(matches!(
            PreparedCommercialEnduranceMachinePlan::load(&path, &profile(), 23),
            Err(CommercialEnduranceMachinePlanError::InvalidSeekTargets {
                expected: 23,
                actual: 24
            })
        ));
    }

    #[test]
    fn unknown_plan_fields_and_simulated_reference_are_rejected() {
        let temporary = tempfile::tempdir().expect("temporary machine plan");
        let path = temporary.path().join("machine-plan.json");
        write_test_machine_plan(&path, temporary.path(), &profile(), 24);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read plan")).expect("parse plan");
        value["unknown"] = serde_json::json!(true);
        fs::write(
            &path,
            serde_json::to_vec_pretty(&value).expect("serialize unknown plan"),
        )
        .expect("write unknown plan");
        assert!(matches!(
            PreparedCommercialEnduranceMachinePlan::load(&path, &profile(), 24),
            Err(CommercialEnduranceMachinePlanError::Json(_))
        ));

        write_test_machine_plan(&path, temporary.path(), &profile(), 24);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read plan")).expect("parse plan");
        value["reference_output"]["provider"] = serde_json::json!("Simulated");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&value).expect("serialize simulated plan"),
        )
        .expect("write simulated plan");
        assert!(matches!(
            PreparedCommercialEnduranceMachinePlan::load(&path, &profile(), 24),
            Err(CommercialEnduranceMachinePlanError::InvalidReferenceContract)
        ));

        write_test_machine_plan(&path, temporary.path(), &profile(), 24);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read plan")).expect("parse plan");
        value["reference_output"]["open_request"]
            .as_object_mut()
            .expect("Reference request object")
            .remove("ancillary_policy");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&value).expect("serialize plan without ANC policy"),
        )
        .expect("write plan without ANC policy");
        assert!(matches!(
            PreparedCommercialEnduranceMachinePlan::load(&path, &profile(), 24),
            Err(CommercialEnduranceMachinePlanError::Json(_))
        ));
    }
}
