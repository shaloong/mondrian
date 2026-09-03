//! Strict, bounded request admission for the commercial endurance runner.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use mondrian_assets::canonical_native_path;
use mondrian_platform::{
    EndurancePhaseKind, EnduranceQualificationProfile, PreparedEnduranceQualification,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::endurance_campaign::EnduranceCampaignRequest;
use super::endurance_machine_plan::{
    CommercialEnduranceMachinePlanError, EnduranceMachineFileBinding,
    PreparedCommercialEnduranceMachinePlan,
};
use super::endurance_qualification::{
    EnduranceCaptureError, EnduranceRunCapture, EnduranceRunIdentity,
};
use super::endurance_workload::{EnduranceWorkloadError, PreparedEnduranceWorkload};

const RUN_REQUEST_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_RUN_REQUEST_BYTES: u64 = 64 * 1024;
const MAXIMUM_PROFILE_BYTES: u64 = 1024 * 1024;
const MAXIMUM_CAPTURE_AUTHORITY_BYTES: u64 = 1024 * 1024;
const MAXIMUM_WORKLOAD_BYTES: u64 = 16 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnduranceRunRequestFile {
    schema_version: u32,
    profile: EnduranceMachineFileBinding,
    identity: EnduranceRunIdentityFile,
    machine_plan: EnduranceMachineFileBinding,
    capture_authority: EnduranceMachineFileBinding,
    evidence_directory: PathBuf,
    output_manifest_path: PathBuf,
    workloads: Vec<EnduranceRunWorkloadBinding>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnduranceRunWorkloadBinding {
    phase_id: String,
    contract: EnduranceMachineFileBinding,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnduranceRunIdentityFile {
    run_id: String,
    profile_file_sha256: String,
    source_revision: String,
    release_candidate_id: String,
    product_artifact_sha256: String,
    runtime_image_sha256: String,
    build_provenance_sha256: String,
    machine_report_sha256: String,
    platform_cell_sha256: String,
    machine_plan_sha256: String,
    environment_before_sha256: String,
    environment_after_sha256: String,
}

impl From<EnduranceRunIdentityFile> for EnduranceRunIdentity {
    fn from(value: EnduranceRunIdentityFile) -> Self {
        Self {
            run_id: value.run_id,
            profile_file_sha256: value.profile_file_sha256,
            source_revision: value.source_revision,
            release_candidate_id: value.release_candidate_id,
            product_artifact_sha256: value.product_artifact_sha256,
            runtime_image_sha256: value.runtime_image_sha256,
            build_provenance_sha256: value.build_provenance_sha256,
            machine_report_sha256: value.machine_report_sha256,
            platform_cell_sha256: value.platform_cell_sha256,
            machine_plan_sha256: value.machine_plan_sha256,
            environment_before_sha256: value.environment_before_sha256,
            environment_after_sha256: value.environment_after_sha256,
        }
    }
}

/// One fully checked request safe to hand to the serial campaign coordinator.
#[derive(Debug)]
pub struct PreparedEnduranceRunRequest {
    request: EnduranceCampaignRequest,
    request_sha256: String,
}

impl PreparedEnduranceRunRequest {
    /// Load a strict request leaf and validate every pre-issued byte binding.
    pub fn load(path: &Path) -> Result<Self, EnduranceRunRequestError> {
        let (_, bytes, request_sha256) =
            read_bounded_regular_file(path, MAXIMUM_RUN_REQUEST_BYTES, "run_request")?;
        let raw: EnduranceRunRequestFile =
            serde_json::from_slice(&bytes).map_err(EnduranceRunRequestError::Json)?;
        if raw.schema_version != RUN_REQUEST_SCHEMA_VERSION {
            return Err(EnduranceRunRequestError::UnsupportedSchema { actual: raw.schema_version });
        }

        let (_profile_path, profile_bytes) =
            read_binding(&raw.profile, MAXIMUM_PROFILE_BYTES, "profile")?;
        let profile: EnduranceQualificationProfile = serde_json::from_slice(&profile_bytes)
            .map_err(EnduranceRunRequestError::ProfileJson)?;
        PreparedEnduranceQualification::compile(profile.clone())
            .map_err(|error| EnduranceRunRequestError::Profile(error.to_string()))?;

        let identity: EnduranceRunIdentity = raw.identity.into();
        if identity.profile_file_sha256 != raw.profile.sha256
            || identity.machine_plan_sha256 != raw.machine_plan.sha256
        {
            return Err(EnduranceRunRequestError::IdentityBindingMismatch);
        }

        if raw.workloads.len() != profile.phases.len() {
            return Err(EnduranceRunRequestError::WorkloadClosure);
        }
        let mut workload_contracts = BTreeMap::new();
        let mut recovery_cycle_count = None;
        for (binding, requirement) in raw.workloads.iter().zip(&profile.phases) {
            if binding.phase_id != requirement.phase_id
                || binding.contract.sha256 != requirement.workload_sha256
                || workload_contracts.contains_key(&binding.phase_id)
            {
                return Err(EnduranceRunRequestError::WorkloadClosure);
            }
            let (path, _) = read_binding(&binding.contract, MAXIMUM_WORKLOAD_BYTES, "workload")?;
            let workload = PreparedEnduranceWorkload::load(requirement, &path)?;
            if requirement.kind == EndurancePhaseKind::ConcurrentRecovery {
                recovery_cycle_count = Some(workload.recovery_cycle_count());
            }
            workload_contracts.insert(binding.phase_id.clone(), path);
        }
        let recovery_cycle_count = recovery_cycle_count
            .filter(|count| *count > 0)
            .ok_or(EnduranceRunRequestError::WorkloadClosure)?;

        let (machine_plan_path, _) =
            read_binding(&raw.machine_plan, MAXIMUM_PROFILE_BYTES, "machine_plan")?;
        let machine_plan = PreparedCommercialEnduranceMachinePlan::load(
            &machine_plan_path,
            &profile,
            recovery_cycle_count,
        )?;
        if machine_plan.sha256() != raw.machine_plan.sha256 {
            return Err(EnduranceRunRequestError::IdentityBindingMismatch);
        }

        let (capture_authority_path, _) = read_binding(
            &raw.capture_authority,
            MAXIMUM_CAPTURE_AUTHORITY_BYTES,
            "capture_authority",
        )?;
        EnduranceRunCapture::new(
            profile.clone(),
            identity.clone(),
            &capture_authority_path,
            &raw.capture_authority.sha256,
        )?;

        let evidence_directory = existing_empty_directory(&raw.evidence_directory)?;
        let output_manifest_path = absent_output_path(&raw.output_manifest_path)?;
        if output_manifest_path.starts_with(&evidence_directory) {
            return Err(EnduranceRunRequestError::OutputInsideEvidenceDirectory);
        }
        let request = EnduranceCampaignRequest {
            profile,
            identity,
            machine_plan,
            capture_authority_manifest_path: capture_authority_path,
            capture_authority_sha256: raw.capture_authority.sha256,
            evidence_directory,
            output_manifest_path,
            workload_contracts,
        };
        Ok(Self { request, request_sha256 })
    }

    /// SHA-256 of the exact strict request JSON bytes.
    pub fn request_sha256(&self) -> &str {
        &self.request_sha256
    }

    /// Borrow the exact typed machine plan admitted from the request binding.
    pub const fn machine_plan(&self) -> &PreparedCommercialEnduranceMachinePlan {
        &self.request.machine_plan
    }

    /// Borrow the prepared campaign request.
    #[cfg(test)]
    pub(crate) const fn request(&self) -> &EnduranceCampaignRequest {
        &self.request
    }

    /// Transfer the checked request into the serial campaign coordinator.
    pub(crate) fn into_campaign_request(self) -> EnduranceCampaignRequest {
        self.request
    }
}

fn read_binding(
    binding: &EnduranceMachineFileBinding,
    maximum_bytes: u64,
    field: &'static str,
) -> Result<(PathBuf, Vec<u8>), EnduranceRunRequestError> {
    validate_absolute_normalized_path(&binding.path, field)?;
    let (path, bytes, actual_sha256) =
        read_bounded_regular_file(&binding.path, maximum_bytes, field)?;
    if actual_sha256 != binding.sha256 {
        return Err(EnduranceRunRequestError::DigestMismatch { field });
    }
    Ok((path, bytes))
}

fn read_bounded_regular_file(
    path: &Path,
    maximum_bytes: u64,
    field: &'static str,
) -> Result<(PathBuf, Vec<u8>, String), EnduranceRunRequestError> {
    let link = fs::symlink_metadata(path)
        .map_err(|error| EnduranceRunRequestError::Read { field, detail: error.to_string() })?;
    if link.file_type().is_symlink() || !link.file_type().is_file() {
        return Err(EnduranceRunRequestError::InvalidFile { field });
    }
    let canonical = canonical_native_path(path)
        .map_err(|error| EnduranceRunRequestError::Read { field, detail: error.to_string() })?;
    let mut file = File::open(&canonical)
        .map_err(|error| EnduranceRunRequestError::Read { field, detail: error.to_string() })?;
    let metadata = file
        .metadata()
        .map_err(|error| EnduranceRunRequestError::Read { field, detail: error.to_string() })?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum_bytes {
        return Err(EnduranceRunRequestError::InvalidSize {
            field,
            actual: metadata.len(),
            maximum: maximum_bytes,
        });
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).map_err(|_| {
        EnduranceRunRequestError::InvalidSize {
            field,
            actual: metadata.len(),
            maximum: maximum_bytes,
        }
    })?);
    Read::by_ref(&mut file)
        .take(maximum_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| EnduranceRunRequestError::Read { field, detail: error.to_string() })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != metadata.len() {
        return Err(EnduranceRunRequestError::ChangedWhileReading { field });
    }
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    Ok((canonical, bytes, sha256))
}

fn existing_empty_directory(path: &Path) -> Result<PathBuf, EnduranceRunRequestError> {
    validate_absolute_normalized_path(path, "evidence_directory")?;
    let link = fs::symlink_metadata(path).map_err(|error| EnduranceRunRequestError::Read {
        field: "evidence_directory",
        detail: error.to_string(),
    })?;
    if link.file_type().is_symlink() || !link.file_type().is_dir() {
        return Err(EnduranceRunRequestError::InvalidDirectory { field: "evidence_directory" });
    }
    let canonical =
        canonical_native_path(path).map_err(|error| EnduranceRunRequestError::Read {
            field: "evidence_directory",
            detail: error.to_string(),
        })?;
    let mut entries = fs::read_dir(&canonical).map_err(|error| EnduranceRunRequestError::Read {
        field: "evidence_directory",
        detail: error.to_string(),
    })?;
    if entries
        .next()
        .transpose()
        .map_err(|error| EnduranceRunRequestError::Read {
            field: "evidence_directory",
            detail: error.to_string(),
        })?
        .is_some()
    {
        return Err(EnduranceRunRequestError::EvidenceDirectoryNotEmpty);
    }
    Ok(canonical)
}

fn absent_output_path(path: &Path) -> Result<PathBuf, EnduranceRunRequestError> {
    validate_absolute_normalized_path(path, "output_manifest_path")?;
    match fs::symlink_metadata(path) {
        Ok(_) => return Err(EnduranceRunRequestError::OutputAlreadyExists),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.kind() == std::io::ErrorKind::NotADirectory => {}
        Err(error) => {
            return Err(EnduranceRunRequestError::Read {
                field: "output_manifest_path",
                detail: error.to_string(),
            });
        }
    }
    let file_name = path
        .file_name()
        .ok_or(EnduranceRunRequestError::InvalidPath { field: "output_manifest_path" })?;
    let parent = path
        .parent()
        .ok_or(EnduranceRunRequestError::InvalidPath { field: "output_manifest_path" })?;
    let canonical_parent =
        canonical_native_path(parent).map_err(|error| EnduranceRunRequestError::Read {
            field: "output_manifest_path",
            detail: error.to_string(),
        })?;
    Ok(canonical_parent.join(file_name))
}

fn validate_absolute_normalized_path(
    path: &Path,
    field: &'static str,
) -> Result<(), EnduranceRunRequestError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        || path.components().any(|component| match component {
            Component::Normal(part) => !is_portable_ordinary_path_component(part),
            _ => false,
        })
    {
        return Err(EnduranceRunRequestError::InvalidPath { field });
    }
    Ok(())
}

pub(super) fn is_portable_ordinary_path_component(component: &std::ffi::OsStr) -> bool {
    let Some(component) = component.to_str() else {
        return false;
    };
    if component.is_empty()
        || component.ends_with([' ', '.'])
        || component
            .chars()
            .any(|character| character.is_control() || "<>:\"/\\|?*".contains(character))
    {
        return false;
    }
    let device_stem = component
        .split_once('.')
        .map_or(component, |(stem, _extension)| stem)
        .to_ascii_uppercase();
    !matches!(device_stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !matches!(
            device_stem.as_str(),
            "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
}

/// Strict request-admission failure before any product owner is created.
#[derive(Debug, Error)]
pub enum EnduranceRunRequestError {
    /// A request or bound input could not be read.
    #[error("could not read endurance {field}: {detail}")]
    Read { field: &'static str, detail: String },
    /// A request input was not a direct regular non-link file.
    #[error("endurance {field} must be a regular non-link file")]
    InvalidFile { field: &'static str },
    /// A bounded input was empty, too large, or changed length.
    #[error("endurance {field} size {actual} exceeds its bound {maximum}")]
    InvalidSize {
        field: &'static str,
        actual: u64,
        maximum: u64,
    },
    /// A file changed while its bounded bytes were read.
    #[error("endurance {field} changed while it was read")]
    ChangedWhileReading { field: &'static str },
    /// Request JSON did not match the exact schema.
    #[error("invalid endurance run-request JSON: {0}")]
    Json(serde_json::Error),
    /// Profile JSON did not match its exact schema.
    #[error("invalid endurance profile JSON: {0}")]
    ProfileJson(serde_json::Error),
    /// The request schema version is not supported.
    #[error("unsupported endurance run-request schema {actual}; expected 1")]
    UnsupportedSchema { actual: u32 },
    /// A bound file's actual bytes differed from its approved digest.
    #[error("endurance {field} differs from its request-file digest")]
    DigestMismatch { field: &'static str },
    /// Raw profile/machine-plan bindings disagreed with the run identity.
    #[error("endurance run identity differs from its request-file bindings")]
    IdentityBindingMismatch,
    /// Profile policy was invalid.
    #[error("invalid endurance profile: {0}")]
    Profile(String),
    /// Workload entries did not exactly close over the ordered profile phases.
    #[error("endurance workload bindings do not close over the profile")]
    WorkloadClosure,
    /// A runtime directory was absent or was not a direct real directory.
    #[error("endurance {field} must be an existing real directory")]
    InvalidDirectory { field: &'static str },
    /// Evidence publication requires a fresh empty directory.
    #[error("endurance evidence directory must be empty before capture")]
    EvidenceDirectoryNotEmpty,
    /// Final manifest publication is create-only.
    #[error("endurance output manifest already exists")]
    OutputAlreadyExists,
    /// The final manifest must remain outside the external-evidence namespace.
    #[error("endurance output manifest must be outside the evidence directory")]
    OutputInsideEvidenceDirectory,
    /// A runtime route was not absolute and normalized.
    #[error("endurance path '{field}' is not absolute and normalized")]
    InvalidPath { field: &'static str },
    /// The exact machine plan was invalid.
    #[error(transparent)]
    MachinePlan(#[from] CommercialEnduranceMachinePlanError),
    /// A checked-in workload was invalid.
    #[error(transparent)]
    Workload(#[from] EnduranceWorkloadError),
    /// Capture authority or run identity was invalid.
    #[error(transparent)]
    Capture(#[from] EnduranceCaptureError),
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn root() -> PathBuf {
        fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .expect("canonical workspace root")
    }

    fn write_fixture() -> (tempfile::TempDir, PathBuf) {
        let temporary = tempfile::tempdir().expect("temporary runner request");
        let profile_path = root().join("tests/validation/commercial-endurance-qualification.json");
        let profile_bytes = fs::read(&profile_path).expect("read profile");
        let profile_sha256 = format!("{:x}", Sha256::digest(&profile_bytes));
        let profile: EnduranceQualificationProfile =
            serde_json::from_slice(&profile_bytes).expect("profile schema");
        let machine_plan_path = temporary.path().join("machine-plan.json");
        let machine_plan_sha256 = super::super::endurance_machine_plan::write_test_machine_plan(
            &machine_plan_path,
            temporary.path(),
            &profile,
            24,
        );
        let authority_path = temporary.path().join("capture-authority.json");
        let authority_bytes = serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 2,
            "authority_id": "external-commercial-endurance-authority-v2",
            "run_id": "runner-request-test",
            "profile_file_sha256": profile_sha256,
            "source_revision": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "release_candidate_id": "mondrian-test-rc",
            "product_artifact_sha256": SHA,
            "runtime_image_sha256": SHA,
            "build_provenance_sha256": SHA,
            "machine_report_sha256": SHA,
            "platform_cell_sha256": SHA,
            "machine_plan_sha256": machine_plan_sha256,
            "single_use_challenge": "runner-request-challenge",
            "phases": profile.phases.iter().map(|phase| serde_json::json!({
                "phase_id": phase.phase_id,
                "workload_sha256": phase.workload_sha256,
                "producer_owner": phase.producer_owner,
                "producer_verifier_id": phase.producer_verifier_id,
            })).collect::<Vec<_>>(),
        }))
        .expect("serialize authority");
        fs::write(&authority_path, &authority_bytes).expect("write authority");
        let authority_sha256 = format!("{:x}", Sha256::digest(&authority_bytes));
        let evidence_directory = temporary.path().join("evidence");
        fs::create_dir(&evidence_directory).expect("create evidence directory");
        let workloads = profile
            .phases
            .iter()
            .map(|phase| {
                let file_name = match phase.kind {
                    EndurancePhaseKind::PlaybackReference => "playback-reference-v1.json",
                    EndurancePhaseKind::ContinuousExport => "continuous-export-v1.json",
                    EndurancePhaseKind::ConcurrentRecovery => "concurrent-recovery-v1.json",
                };
                serde_json::json!({
                    "phase_id": phase.phase_id,
                    "contract": {
                        "path": root().join("tests/validation/endurance-workloads").join(file_name),
                        "sha256": phase.workload_sha256,
                    }
                })
            })
            .collect::<Vec<_>>();
        let request = serde_json::json!({
            "schema_version": 1,
            "profile": { "path": profile_path, "sha256": profile_sha256 },
            "identity": {
                "run_id": "runner-request-test",
                "profile_file_sha256": profile_sha256,
                "source_revision": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "release_candidate_id": "mondrian-test-rc",
                "product_artifact_sha256": SHA,
                "runtime_image_sha256": SHA,
                "build_provenance_sha256": SHA,
                "machine_report_sha256": SHA,
                "platform_cell_sha256": SHA,
                "machine_plan_sha256": machine_plan_sha256,
                "environment_before_sha256": SHA,
                "environment_after_sha256": SHA,
            },
            "machine_plan": { "path": machine_plan_path, "sha256": machine_plan_sha256 },
            "capture_authority": { "path": authority_path, "sha256": authority_sha256 },
            "evidence_directory": evidence_directory,
            "output_manifest_path": temporary.path().join("run.json"),
            "workloads": workloads,
        });
        let request_path = temporary.path().join("request.json");
        fs::write(
            &request_path,
            serde_json::to_vec_pretty(&request).expect("serialize request"),
        )
        .expect("write request");
        (temporary, request_path)
    }

    #[test]
    fn request_loader_closes_profile_plan_authority_and_workloads() {
        let (temporary, request_path) = write_fixture();
        let prepared = PreparedEnduranceRunRequest::load(&request_path)
            .expect("prepare exact endurance request");

        assert_eq!(prepared.request_sha256().len(), 64);
        assert_eq!(prepared.request().profile.phases.len(), 3);
        assert_eq!(prepared.request().workload_contracts.len(), 3);
        assert_eq!(prepared.request().identity.machine_plan_sha256.len(), 64);
        assert_eq!(prepared.request().capture_authority_sha256.len(), 64);

        let admitted_sha256 = prepared.machine_plan().sha256().to_owned();
        fs::write(temporary.path().join("machine-plan.json"), b"{}")
            .expect("replace plan path after admission");
        assert_eq!(prepared.machine_plan().sha256(), admitted_sha256);
        assert_eq!(prepared.request().machine_plan.sha256(), admitted_sha256);
    }

    #[test]
    fn request_loader_rejects_unknown_fields_digest_drift_and_dirty_output_routes() {
        let (temporary, request_path) = write_fixture();
        let original = fs::read(&request_path).expect("read request");
        let mut value: serde_json::Value =
            serde_json::from_slice(&original).expect("parse request value");
        value["unknown"] = serde_json::json!(true);
        fs::write(
            &request_path,
            serde_json::to_vec_pretty(&value).expect("serialize unknown request"),
        )
        .expect("write unknown request");
        assert!(matches!(
            PreparedEnduranceRunRequest::load(&request_path),
            Err(EnduranceRunRequestError::Json(_))
        ));

        fs::write(&request_path, &original).expect("restore request");
        let authority_path = temporary.path().join("capture-authority.json");
        let mut authority = fs::read(&authority_path).expect("read authority");
        authority.push(b' ');
        fs::write(&authority_path, authority).expect("drift authority bytes");
        assert!(matches!(
            PreparedEnduranceRunRequest::load(&request_path),
            Err(EnduranceRunRequestError::DigestMismatch { field: "capture_authority" })
        ));

        let (_temporary, request_path) = write_fixture();
        let request: serde_json::Value =
            serde_json::from_slice(&fs::read(&request_path).expect("read request"))
                .expect("parse request");
        fs::write(
            request["output_manifest_path"].as_str().expect("output path"),
            b"occupied",
        )
        .expect("occupy output route");
        assert!(matches!(
            PreparedEnduranceRunRequest::load(&request_path),
            Err(EnduranceRunRequestError::OutputAlreadyExists)
        ));

        let (temporary, request_path) = write_fixture();
        let mut request: serde_json::Value =
            serde_json::from_slice(&fs::read(&request_path).expect("read request"))
                .expect("parse request");
        request["output_manifest_path"] =
            serde_json::json!(temporary.path().join("evidence/run.json"));
        fs::write(
            &request_path,
            serde_json::to_vec_pretty(&request).expect("serialize nested output route"),
        )
        .expect("write nested output route");
        assert!(matches!(
            PreparedEnduranceRunRequest::load(&request_path),
            Err(EnduranceRunRequestError::OutputInsideEvidenceDirectory)
        ));

        let (_temporary, request_path) = write_fixture();
        let mut request: serde_json::Value =
            serde_json::from_slice(&fs::read(&request_path).expect("read request"))
                .expect("parse request");
        request["output_manifest_path"] = serde_json::json!(request_path
            .parent()
            .expect("request parent")
            .join("evidence:run.json"));
        fs::write(
            &request_path,
            serde_json::to_vec_pretty(&request).expect("serialize ADS output route"),
        )
        .expect("write ADS output route");
        assert!(matches!(
            PreparedEnduranceRunRequest::load(&request_path),
            Err(EnduranceRunRequestError::InvalidPath { field: "output_manifest_path" })
        ));

        let (_temporary, request_path) = write_fixture();
        let mut request: serde_json::Value =
            serde_json::from_slice(&fs::read(&request_path).expect("read request"))
                .expect("parse request");
        request["profile"]["path"] = serde_json::json!("relative-profile.json");
        fs::write(
            &request_path,
            serde_json::to_vec_pretty(&request).expect("serialize relative binding"),
        )
        .expect("write relative binding");
        assert!(matches!(
            PreparedEnduranceRunRequest::load(&request_path),
            Err(EnduranceRunRequestError::InvalidPath { field: "profile" })
        ));
    }
}
