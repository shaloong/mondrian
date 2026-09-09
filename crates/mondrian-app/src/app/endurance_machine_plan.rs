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

const MACHINE_PLAN_SCHEMA_VERSION: u32 = 2;
const MAXIMUM_MACHINE_PLAN_BYTES: u64 = 256 * 1024;
const MAXIMUM_PATH_BYTES: usize = 4 * 1024;
const MAXIMUM_IDENTITY_BYTES: usize = 128;
const MAXIMUM_SEEK_TARGETS: usize = 128;
const MAXIMUM_FFMPEG_RUNTIME_FILES: usize = 512;
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
    /// Independent physical AJA/DeckLink SDI receiver and validation receipt contract.
    #[serde(default)]
    pub wire_readback: Option<EnduranceMachineWireReadbackPlan>,
}

/// Explicit second-card physical ANC capture configuration. The phase owner
/// generates the marker nonce; fixtures never synthesize readbacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceMachineWireReadbackPlan {
    /// Exact discovered receiver device, distinct from the output card.
    pub device_id: ReferenceOutputDeviceId,
    /// Receiver discovery generation.
    pub device_generation: u64,
    /// Progressive VANC line reserved for the canonical validation marker.
    pub marker_line: u16,
    /// Exact luma-word offset of the marker in that VANC line.
    pub marker_horizontal_offset: u16,
    /// Existing directory for complete output and independent capture words.
    pub receipt_directory: PathBuf,
    /// Bounded bytes per reopened physical session receipt journal.
    pub maximum_receipt_bytes: u64,
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
    /// Explicit externally approved PSE installation; missing approval is never synthesized.
    #[serde(default)]
    pub regulatory_pse: Option<mondrian_export::RegulatoryPseProviderConfig>,
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
    /// Exact approved BMX runtime for AS-11 repeated exports; absence is pre-start NotRun.
    #[serde(default)]
    pub bmx: Option<EnduranceMachineBmxTools>,
    /// Optional approved FFmpeg-free native launcher; absent means no pre-loader capability.
    #[serde(default)]
    pub preloader: Option<EnduranceMachineFileBinding>,
    /// Pinned FFmpeg executable and capability identity.
    pub ffmpeg: EnduranceMachineToolPlan,
    /// Pinned FFprobe executable and capability identity.
    pub ffprobe: EnduranceMachineToolPlan,
    /// Complete ordered packaged Windows DLL closure shared by both tools and
    /// the in-process FFmpeg runtime.
    pub runtime_files: Vec<EnduranceMachineFileBinding>,
}

/// Approved executable identities and complete native DLL namespace for BMX.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceMachineBmxTools {
    /// Exact raw essence wrapper image.
    pub raw2bmx: EnduranceMachineFileBinding,
    /// Exact independent MXF reader image.
    pub mxf2raw: EnduranceMachineFileBinding,
    /// Bounded raw2bmx `-v` stdout followed by stderr SHA-256.
    pub raw2bmx_version_output_sha256: String,
    /// Bounded mxf2raw `-v` stdout followed by stderr SHA-256.
    pub mxf2raw_version_output_sha256: String,
    /// Explicit complete approved DLL set; missing declaration is NotRun.
    pub runtime_files: Option<Vec<EnduranceMachineFileBinding>>,
}

/// Non-renewing execution bounds frozen into the approved machine plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceMachineTimeoutPlan {
    /// One original bound for capability admission and all cold owner preparation.
    #[serde(default = "default_startup_timeout_ms")]
    pub startup_ms: u64,
    /// Maximum latency of one ordinary product pump interval.
    pub interval_ms: u64,
    /// Maximum latency of one complete four-step recovery cycle.
    pub recovery_ms: u64,
    /// Maximum latency of one native Window/Surface/Device reopen operation.
    pub surface_reopen_ms: u64,
    /// Single consuming shutdown deadline for one phase.
    pub shutdown_ms: u64,
}

const fn default_startup_timeout_ms() -> u64 {
    120_000
}

/// Bounded machine-local plan whose exact bytes are a qualification trust anchor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommercialEnduranceMachinePlan {
    /// Machine-plan schema. Version 2 is required.
    pub schema_version: u32,
    /// Stable operator-assigned plan identity.
    pub plan_id: String,
    /// Exact Project/Sequence/source inventory.
    pub project: EnduranceMachineProjectPlan,
    /// Exact physical realtime audio contract.
    pub audio: EnduranceMachineAudioPlan,
    /// Exact physical Reference Output contract.
    pub reference_output: EnduranceMachineReferencePlan,
    /// One exact canonical ANC program shared by physical output and every repeated export.
    #[serde(default)]
    pub ancillary_program: Option<EnduranceMachineFileBinding>,
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
    phase_requirements: Vec<mondrian_platform::EndurancePhaseRequirement>,
    ancillary: super::endurance_ancillary::EnduranceAncillaryAdmission,
}

impl PreparedCommercialEnduranceMachinePlan {
    /// Load one bounded regular JSON file and validate it against the exact profile/workload.
    pub fn load(
        path: &Path,
        profile: &EnduranceQualificationProfile,
        recovery_cycle_count: u32,
    ) -> Result<Self, CommercialEnduranceMachinePlanError> {
        mondrian_platform::PreparedEnduranceQualification::compile(profile.clone())
            .map_err(|error| CommercialEnduranceMachinePlanError::Profile(error.to_string()))?;
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
        let ancillary = super::endurance_ancillary::EnduranceAncillaryAdmission::prepare(
            plan.ancillary_program.as_ref(),
        )
        .map_err(CommercialEnduranceMachinePlanError::Read)?;
        if let Some(program) = ancillary.program() {
            program
                .validate_rate(plan.reference_output.open_request.signal.frame_rate)
                .map_err(CommercialEnduranceMachinePlanError::Read)?;
        }
        Ok(Self {
            ancillary,
            plan,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            phase_requirements: profile.phases.clone(),
        })
    }

    /// Same parsed source and native lease for every phase consumer.
    pub fn ancillary_program(
        &self,
    ) -> Option<&std::sync::Arc<super::endurance_ancillary::PreparedEnduranceAncillaryProgram>>
    {
        self.ancillary.program()
    }
    pub(crate) fn ancillary_program_missing(&self) -> bool {
        self.ancillary.missing()
    }

    /// Validated strongly typed machine plan.
    pub const fn plan(&self) -> &CommercialEnduranceMachinePlan {
        &self.plan
    }

    /// Lowercase SHA-256 of the exact approved JSON bytes.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Complete ordered profile topology used to validate this machine plan.
    ///
    /// Downstream fixture admission consumes this sealed list instead of a
    /// caller-supplied phase slice that could omit required work.
    pub fn phase_requirements(&self) -> &[mondrian_platform::EndurancePhaseRequirement] {
        &self.phase_requirements
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
    if let Some(binding) = &plan.ancillary_program {
        validate_file_binding(binding, "ancillary_program")?;
        if !plan.reference_output.open_request.ancillary_policy.requires_readback()
            || plan.reference_output.wire_readback.is_none()
        {
            return Err(CommercialEnduranceMachinePlanError::InvalidReferenceContract);
        }
    }
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
    if let Some(preloader) = &plan.verifier_tools.preloader {
        validate_file_binding(preloader, "verifier_tools.preloader")?;
    }
    if let Some(wire) = &plan.reference_output.wire_readback {
        validate_absolute_path(
            &wire.receipt_directory,
            "reference_output.wire_readback.receipt_directory",
        )?;
        if !matches!(
            plan.reference_output.provider,
            ReferenceOutputProvider::AjaNtv2 | ReferenceOutputProvider::DeckLink
        ) || wire.device_id == plan.reference_output.device_id
            || wire.device_generation == 0
            || wire.marker_line == 0
            || wire.marker_line > 2047
            || u32::from(wire.marker_horizontal_offset) + 39
                > plan.reference_output.open_request.signal.width
            || wire.maximum_receipt_bytes < 4096
            || wire.maximum_receipt_bytes > (1u64 << 40)
        {
            return Err(CommercialEnduranceMachinePlanError::InvalidReferenceContract);
        }
    }
    validate_tool(&plan.verifier_tools.ffmpeg, "ffmpeg")?;
    validate_tool(&plan.verifier_tools.ffprobe, "ffprobe")?;
    validate_verifier_runtime_files(&plan.verifier_tools)?;
    if let Some(bmx) = &plan.verifier_tools.bmx {
        validate_file_binding(&bmx.raw2bmx, "verifier_tools.bmx.raw2bmx")?;
        validate_file_binding(&bmx.mxf2raw, "verifier_tools.bmx.mxf2raw")?;
        validate_sha256(
            &bmx.raw2bmx_version_output_sha256,
            "verifier_tools.bmx.raw2bmx_version_output_sha256",
        )?;
        validate_sha256(
            &bmx.mxf2raw_version_output_sha256,
            "verifier_tools.bmx.mxf2raw_version_output_sha256",
        )?;
        if let Some(files) = &bmx.runtime_files {
            if files.len() > 256 {
                return Err(CommercialEnduranceMachinePlanError::InvalidVerifierRuntimeClosure);
            }
            for file in files {
                validate_file_binding(file, "verifier_tools.bmx.runtime_files")?;
            }
        }
    }
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

fn validate_verifier_runtime_files(
    tools: &EnduranceMachineVerifierTools,
) -> Result<(), CommercialEnduranceMachinePlanError> {
    if tools.runtime_files.is_empty()
        || tools.runtime_files.len() > MAXIMUM_FFMPEG_RUNTIME_FILES
        || tools.ffmpeg.executable.path.parent() != tools.ffprobe.executable.path.parent()
    {
        return Err(CommercialEnduranceMachinePlanError::InvalidVerifierRuntimeClosure);
    }
    let executable_directory = tools
        .ffmpeg
        .executable
        .path
        .parent()
        .ok_or(CommercialEnduranceMachinePlanError::InvalidVerifierRuntimeClosure)?;
    let mut previous: Option<&Path> = None;
    let mut names = BTreeSet::new();
    for binding in &tools.runtime_files {
        validate_file_binding(binding, "verifier_tools.runtime_files")?;
        if binding.path.parent() != Some(executable_directory)
            || binding
                .path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_none_or(|extension| !extension.eq_ignore_ascii_case("dll"))
            || previous.is_some_and(|path| path >= binding.path.as_path())
            || !names.insert(binding.path.file_name())
        {
            return Err(CommercialEnduranceMachinePlanError::InvalidVerifierRuntimeClosure);
        }
        previous = Some(binding.path.as_path());
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
        timeouts.startup_ms,
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
    /// Supplied profile was not a complete compiled commercial qualification.
    #[error("commercial endurance profile is invalid: {0}")]
    Profile(String),
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
    #[error("unsupported commercial endurance machine-plan schema {actual}; expected 2")]
    UnsupportedSchema { actual: u32 },
    /// Identity token was empty, placeholder, oversized, or unsafe.
    #[error("invalid commercial endurance machine-plan identity '{field}'")]
    InvalidIdentity { field: &'static str },
    /// An approved file digest was malformed.
    #[error("commercial endurance machine-plan field '{field}' must be lowercase SHA-256")]
    InvalidSha256 { field: &'static str },
    /// Pinned FFmpeg runtime files were missing, unordered, duplicated, or did
    /// not share the executable directory.
    #[error("invalid FFmpeg verifier runtime-file closure")]
    InvalidVerifierRuntimeClosure,
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
            preset: {
                let path = fixture_root.join(format!("{}-preset.json", phase.phase_id));
                let bytes =
                    serde_json::to_vec(&mondrian_export::ExportPreset::h264_aac_sdr_1080p())
                        .expect("test preset JSON");
                std::fs::write(&path, &bytes).expect("write exact test preset");
                EnduranceMachineFileBinding {
                    path: mondrian_assets::canonical_native_path(&path)
                        .expect("canonical test preset"),
                    sha256: format!("{:x}", sha2::Sha256::digest(&bytes)),
                }
            },
            sequence_id,
            range: EnduranceMachineExportRange::EntireSequence,
            output_directory: fixture_root.join(format!("{}-output", phase.phase_id)),
            artifact_prefix: phase.phase_id.clone(),
            broadcast_qc: None,
            regulatory_pse: None,
            output_policy: EnduranceMachineExportOutputPolicy::CreateNew,
            maximum_artifact_bytes: 1 << 30,
            decode_timeout_ms: 60_000,
        })
        .collect();
    let plan = CommercialEnduranceMachinePlan {
        ancillary_program: None,
        schema_version: 2,
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
            wire_readback: None,
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
            bmx: None,
            preloader: None,
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
            runtime_files: vec![binding("avcodec.dll")],
        },
        timeouts: EnduranceMachineTimeoutPlan {
            startup_ms: default_startup_timeout_ms(),
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

    #[cfg(windows)]
    #[test]
    fn bmx_prerequisites_are_rejected_before_any_phase_native_owner() {
        use crate::app::endurance_source_inventory::{
            prepare_bmx_prerequisite, EnduranceBmxAdmission,
        };
        let temporary = tempfile::tempdir().expect("temporary plan");
        let path = temporary.path().join("machine-plan.json");
        let profile = profile();
        write_test_machine_plan(&path, temporary.path(), &profile, 24);
        let mut plan: CommercialEnduranceMachinePlan =
            serde_json::from_slice(&fs::read(&path).expect("read plan")).expect("plan");
        let phase_id = plan.exports[0].phase_id.clone();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let cancel = mondrian_core::ExecutionCancellationToken::new();
        let prepared = PreparedCommercialEnduranceMachinePlan::load(&path, &profile, 24)
            .expect("ordinary plan");
        let ordinary = prepare_bmx_prerequisite(&prepared, &phase_id, deadline, deadline, &cancel);
        assert!(
            matches!(ordinary, Ok(EnduranceBmxAdmission::NotRequired)),
            "unexpected ordinary admission error: {:?}",
            ordinary.err()
        );
        let bytes = serde_json::to_vec(&mondrian_export::ExportPreset::as11_x9_naba_hd_720p5994())
            .expect("AS11 preset");
        fs::write(&plan.exports[0].preset.path, &bytes).expect("replace test preset");
        plan.exports[0].preset.sha256 = format!("{:x}", Sha256::digest(bytes));
        let missing = EnduranceMachineFileBinding {
            path: temporary.path().join("missing-bmx.exe"),
            sha256: "a".repeat(64),
        };
        for bmx in [
            None,
            Some(EnduranceMachineBmxTools {
                raw2bmx: missing.clone(),
                mxf2raw: missing.clone(),
                raw2bmx_version_output_sha256: "b".repeat(64),
                mxf2raw_version_output_sha256: "c".repeat(64),
                runtime_files: None,
            }),
            Some(EnduranceMachineBmxTools {
                raw2bmx: missing.clone(),
                mxf2raw: missing,
                raw2bmx_version_output_sha256: "b".repeat(64),
                mxf2raw_version_output_sha256: "c".repeat(64),
                runtime_files: Some(Vec::new()),
            }),
        ] {
            plan.verifier_tools.bmx = bmx;
            fs::write(&path, serde_json::to_vec(&plan).expect("bound plan")).expect("write plan");
            let prepared = PreparedCommercialEnduranceMachinePlan::load(&path, &profile, 24)
                .expect("admitted plan shape");
            assert!(matches!(
                prepare_bmx_prerequisite(&prepared, &phase_id, deadline, deadline, &cancel),
                Ok(EnduranceBmxAdmission::NotRun)
            ));
        }
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
    fn startup_timeout_is_independent_defaulted_and_bounded() {
        let json = serde_json::json!({"interval_ms":1000,"recovery_ms":7,"surface_reopen_ms":1000,"shutdown_ms":1000});
        let mut timeouts: EnduranceMachineTimeoutPlan =
            serde_json::from_value(json).expect("legacy plan uses startup default");
        assert_eq!(timeouts.startup_ms, 120_000);
        assert!(validate_timeouts(timeouts).is_ok());
        for invalid in [0, MAXIMUM_TIMEOUT_MS + 1] {
            timeouts.startup_ms = invalid;
            assert!(validate_timeouts(timeouts).is_err());
        }
        timeouts.startup_ms = MAXIMUM_TIMEOUT_MS;
        assert!(validate_timeouts(timeouts).is_ok());
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
        value["verifier_tools"]["runtime_files"] = serde_json::json!([]);
        fs::write(
            &path,
            serde_json::to_vec_pretty(&value).expect("serialize empty runtime closure"),
        )
        .expect("write empty runtime closure");
        assert!(matches!(
            PreparedCommercialEnduranceMachinePlan::load(&path, &profile(), 24),
            Err(CommercialEnduranceMachinePlanError::InvalidVerifierRuntimeClosure)
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
