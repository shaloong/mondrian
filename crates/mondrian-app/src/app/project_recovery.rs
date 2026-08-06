//! Durable Project recovery-point index and lifecycle.
//!
//! This Module owns the complete recovery manifest contract: publication,
//! identity/hash validation, retention, discovery, selection admission, and
//! retirement after a current manual Project save. Callers never interpret or
//! mutate manifest JSON themselves.

use super::project_runtime::{
    create_direct_exclusive_file, ensure_owned_runtime_child_directory, open_direct_read_file,
    project_runtime_parent, validate_owned_runtime_child_directory,
    validate_runtime_child_directory_readonly, validate_runtime_owner_readonly,
    ProjectRuntimeLease,
};
use mondrian_core::ProjectId;
use mondrian_project::{
    read_project_document_from_open_archive, ProjectArchivePublication,
    ProjectArchivePublicationEvidence,
};
use mondrian_storage::{
    write_durable_file_atomically, FilePublicationDurabilityUnconfirmed, FilePublicationFailure,
    FilePublicationNamespaceIndeterminate,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const RECOVERY_MANIFEST_SCHEMA_VERSION: u32 = 1;
const RECOVERY_MANIFEST_FILE: &str = "manifest.json";
const MAX_RECOVERY_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
static RECOVERY_MUTATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// One exact, independently re-verifiable startup recovery choice.
///
/// A candidate is immutable selection evidence, not mutation authority. Opening
/// it still requires the exact runtime lease and a second manifest/archive
/// verification after that lease has been acquired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrashRecoveryCandidate {
    /// Durable identity of the Project represented by the snapshot.
    pub project_id: ProjectId,
    /// Exact paired runtime root containing the canonical recovery manifest.
    pub runtime_root: PathBuf,
    /// Absolute canonical Project publication path recorded by the manifest.
    pub project_file: PathBuf,
    /// Read-only state of the canonical publication target at discovery.
    /// Selection admission verifies this again before taking a live lease.
    pub canonical_target: RecoveryCanonicalTargetEvidence,
    /// Absolute autosave archive path selected within `runtime_root/autosave`.
    pub autosave_file: PathBuf,
    /// Authoring generation captured by the autosave publication.
    pub author_generation: u64,
    /// Asset Library revision captured by the autosave publication.
    pub asset_library_revision: u64,
    /// Durable document revision encoded by the selected archive.
    pub document_revision: u64,
    /// SHA-256 identity of the selected archive bytes.
    pub archive_sha256: String,
    /// Wall-clock publication time used only for startup presentation/sorting.
    pub saved_at_unix_ms: u64,
    /// Number of currently admissible snapshots in this exact manifest.
    pub total_snapshots: usize,
}

/// Canonical publication target evidence attached to a recovery choice.
///
/// This is presentation and stale-selection evidence, not filesystem mutation
/// authority. Recovery admission re-reads the target and requires the same
/// state before opening the autosave into a dirty Authoring Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RecoveryCanonicalTargetEvidence {
    /// The manifest's intended canonical Project path did not exist.
    Missing,
    /// A valid canonical archive for the same Project existed.
    Present {
        /// Durable document revision observed in the canonical archive.
        document_revision: u64,
    },
}

impl RecoveryCanonicalTargetEvidence {
    fn document_revision(self) -> Option<u64> {
        match self {
            Self::Missing => None,
            Self::Present { document_revision } => Some(document_revision),
        }
    }
}

/// Complete immutable facts required to publish one recovery point.
pub(super) struct RecoveryPointPublication<'a> {
    pub project_file: &'a Path,
    pub runtime_lease: &'a ProjectRuntimeLease,
    pub archive_evidence: ProjectArchivePublicationEvidence,
    pub author_generation: u64,
    pub asset_library_revision: u64,
    pub saved_at_unix_ms: u64,
    pub max_recovery_points: usize,
    pub retention_days: u32,
}

#[cfg(test)]
struct RecoveryPointPublicationForTest<'a> {
    project_file: &'a Path,
    runtime_lease: &'a ProjectRuntimeLease,
    autosave_file: &'a Path,
    author_generation: u64,
    asset_library_revision: u64,
    document_revision: u64,
    saved_at_unix_ms: u64,
    max_recovery_points: usize,
    retention_days: u32,
}

struct RecoveryPointManifestFacts<'a> {
    project_file: &'a Path,
    runtime_lease: &'a ProjectRuntimeLease,
    autosave_file: &'a Path,
    author_generation: u64,
    asset_library_revision: u64,
    document_revision: u64,
    saved_at_unix_ms: u64,
    max_recovery_points: usize,
    retention_days: u32,
}

/// Typed Recovery Manifest failure retained until the recovery workflow decides
/// whether old archives may be removed.
#[derive(Debug)]
pub(super) enum RecoveryManifestPublicationFailure {
    BeforeNamespace(anyhow::Error),
    DurabilityUnconfirmed(FilePublicationDurabilityUnconfirmed),
    NamespaceIndeterminate(FilePublicationNamespaceIndeterminate),
}

impl fmt::Display for RecoveryManifestPublicationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeforeNamespace(error) => write!(
                formatter,
                "Recovery Manifest publication failed before namespace commit; prior Recovery Authority remains unchanged: {error:#}"
            ),
            Self::DurabilityUnconfirmed(error) => write!(
                formatter,
                "Recovery Manifest namespace was updated but crash durability is unconfirmed; archive cleanup is suppressed: {error}"
            ),
            Self::NamespaceIndeterminate(error) => write!(
                formatter,
                "Recovery Manifest namespace postcondition is indeterminate; archive cleanup is suppressed: {error}"
            ),
        }
    }
}

impl std::error::Error for RecoveryManifestPublicationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BeforeNamespace(error) => Some(error.as_ref()),
            Self::DurabilityUnconfirmed(error) => Some(error),
            Self::NamespaceIndeterminate(error) => Some(error),
        }
    }
}

impl From<String> for RecoveryManifestPublicationFailure {
    fn from(error: String) -> Self {
        Self::BeforeNamespace(anyhow::Error::msg(error))
    }
}

impl From<FilePublicationFailure> for RecoveryManifestPublicationFailure {
    fn from(error: FilePublicationFailure) -> Self {
        match error {
            FilePublicationFailure::BeforeNamespace(error) => Self::BeforeNamespace(error),
            FilePublicationFailure::DurabilityUnconfirmed(error) => {
                Self::DurabilityUnconfirmed(error)
            }
            FilePublicationFailure::NamespaceIndeterminate(error) => {
                Self::NamespaceIndeterminate(error)
            }
        }
    }
}

/// Result of reconciling recovery authority after one manual save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RecoveryReconciliation {
    pub removed_snapshot_count: usize,
}

/// Best-effort result of cleaning crash-only files under one exact lease.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RecoveryArtifactCleanup {
    /// Abandoned direct-child recovery-open staging files removed.
    pub removed_staging_count: usize,
    /// Autosave archives absent from the valid canonical manifest removed.
    pub removed_unreferenced_snapshot_count: usize,
}

/// Read-only preflight evidence for one exact recovery selection.
///
/// This is not mutation authority. The complete manifest entry is retained so
/// it can be matched again after the caller acquires the Project runtime lease.
#[derive(Debug)]
pub(super) struct RecoverySelectionEvidence {
    /// Stable runtime authority containing the selected canonical manifest.
    pub runtime_root: PathBuf,
    /// Durable Project identity that must be leased before any mutation.
    pub project_id: ProjectId,
    project_file: PathBuf,
    canonical_target: RecoveryCanonicalTargetEvidence,
    exact_entry: RecoverySnapshotEntry,
}

/// Exact verified staging file object admitted for recovery open.
///
/// The file object, not its temporary pathname, is the authority consumed by
/// archive loading. The path exists only for cleanup diagnostics.
#[derive(Debug)]
pub(super) struct VerifiedRecoveryArchive {
    path: PathBuf,
    file: Option<fs::File>,
}

impl VerifiedRecoveryArchive {
    /// Borrow the exact already-verified file object while cleanup remains armed.
    pub(super) fn file_mut(&mut self) -> Result<&mut fs::File, String> {
        self.file
            .as_mut()
            .ok_or_else(|| "verified recovery archive is no longer available".to_owned())
    }
}

impl Drop for VerifiedRecoveryArchive {
    fn drop(&mut self) {
        let Some(file) = self.file.take() else {
            return;
        };
        let remove_path = staging_path_still_names_open_file(&self.path, &file);
        drop(file);
        if !remove_path {
            return;
        }
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    path = %self.path.display(),
                    %error,
                    "failed to remove verified recovery staging archive"
                );
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryManifest {
    schema_version: u32,
    project_id: ProjectId,
    project_file: PathBuf,
    snapshots: Vec<RecoverySnapshotEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoverySnapshotEntry {
    file: PathBuf,
    saved_at_unix_ms: u64,
    author_generation: u64,
    asset_library_revision: u64,
    document_revision: u64,
    archive_sha256: String,
}

impl RecoveryManifest {
    fn new(project_id: ProjectId, project_file: &Path) -> Result<Self, String> {
        let manifest = Self {
            schema_version: RECOVERY_MANIFEST_SCHEMA_VERSION,
            project_id,
            project_file: project_file.to_path_buf(),
            snapshots: Vec::new(),
        };
        manifest.validate_identity(project_id)?;
        Ok(manifest)
    }

    fn validate_identity(&self, expected_project_id: ProjectId) -> Result<(), String> {
        if self.schema_version != RECOVERY_MANIFEST_SCHEMA_VERSION {
            return Err(format!(
                "unsupported recovery manifest schema v{} (expected v{})",
                self.schema_version, RECOVERY_MANIFEST_SCHEMA_VERSION
            ));
        }
        if self.project_id != expected_project_id {
            return Err("recovery manifest Project identity does not match".to_owned());
        }
        if !self.project_file.is_absolute() {
            return Err("recovery manifest Project path is not absolute".to_owned());
        }
        if self.snapshots.iter().any(|entry| !entry.file.is_absolute()) {
            return Err("recovery manifest contains a non-absolute snapshot path".to_owned());
        }
        Ok(())
    }

    fn validate_unique_snapshot_paths(&self) -> Result<(), String> {
        let mut paths = HashSet::with_capacity(self.snapshots.len());
        for entry in &self.snapshots {
            validate_snapshot_entry_identity(entry)?;
            if !paths.insert(entry.file.as_path()) {
                return Err("recovery manifest contains duplicate snapshot paths".to_owned());
            }
        }
        Ok(())
    }

    fn exact_snapshot(&self, file: &Path) -> Result<&RecoverySnapshotEntry, String> {
        self.validate_unique_snapshot_paths()?;
        self.snapshots.iter().find(|entry| entry.file == file).ok_or_else(|| {
            "selected recovery point is not present in the canonical manifest".to_owned()
        })
    }
}

fn candidate_matches_entry(
    candidate: &CrashRecoveryCandidate,
    entry: &RecoverySnapshotEntry,
) -> bool {
    candidate.autosave_file == entry.file
        && candidate.author_generation == entry.author_generation
        && candidate.asset_library_revision == entry.asset_library_revision
        && candidate.document_revision == entry.document_revision
        && candidate.archive_sha256 == entry.archive_sha256
        && candidate.saved_at_unix_ms == entry.saved_at_unix_ms
}

pub(super) fn recovery_manifest_path(runtime_root: &Path) -> PathBuf {
    runtime_root.join("autosave").join(RECOVERY_MANIFEST_FILE)
}

/// Remove only crash artifacts proven to belong to one exact, live runtime.
///
/// Callers must quiesce persistence for this Project before invoking cleanup.
/// Snapshot cleanup requires a structurally valid canonical manifest; a
/// missing or invalid manifest preserves every autosave archive. Unknown
/// entries, directories, symlinks, and files outside the exact leased root are
/// never removed.
pub(super) fn cleanup_recovery_runtime_artifacts(
    runtime_lease: &ProjectRuntimeLease,
) -> Result<RecoveryArtifactCleanup, String> {
    let _mutation = recovery_mutation_guard();
    runtime_lease.validate()?;
    let runtime_root = runtime_lease.runtime_root();
    let mut cleanup = RecoveryArtifactCleanup::default();

    let entries = fs::read_dir(runtime_root).map_err(|error| {
        format!(
            "failed to inspect leased Project runtime {}: {error}",
            runtime_root.display()
        )
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with(".recovery-open-") || !name.ends_with(".staging.mdp") {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_file() || path.parent() != Some(runtime_root) {
            continue;
        }
        if remove_recovery_artifact(&path, "abandoned recovery staging archive") {
            cleanup.removed_staging_count += 1;
        }
    }

    let autosave_dir = recovery_autosave_dir(runtime_root);
    match fs::symlink_metadata(&autosave_dir) {
        Ok(_) => validate_owned_runtime_child_directory(runtime_lease, &autosave_dir)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(cleanup),
        Err(error) => {
            return Err(format!(
                "failed to inspect Project autosave directory {}: {error}",
                autosave_dir.display()
            ));
        }
    }
    let manifest_path = recovery_manifest_path(runtime_root);
    let manifest = match read_manifest(&manifest_path) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::warn!(
                path = %manifest_path.display(),
                %error,
                "preserving recovery archives because the canonical manifest is unavailable"
            );
            return Ok(cleanup);
        }
    };
    if let Err(error) = manifest
        .validate_identity(runtime_lease.project_id())
        .and_then(|()| manifest.validate_unique_snapshot_paths())
    {
        tracing::warn!(
            path = %manifest_path.display(),
            %error,
            "preserving recovery archives because the canonical manifest is invalid"
        );
        return Ok(cleanup);
    }
    let retained = manifest
        .snapshots
        .iter()
        .map(|entry| {
            validate_snapshot_path(&autosave_dir, &entry.file)?;
            Ok(entry.file.clone())
        })
        .collect::<Result<HashSet<_>, String>>();
    let retained = match retained {
        Ok(retained) => retained,
        Err(error) => {
            tracing::warn!(
                path = %manifest_path.display(),
                %error,
                "preserving recovery archives because a manifest path is invalid"
            );
            return Ok(cleanup);
        }
    };

    let entries = fs::read_dir(&autosave_dir).map_err(|error| {
        format!(
            "failed to inspect Project autosave directory {}: {error}",
            autosave_dir.display()
        )
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if retained.contains(&path) || validate_snapshot_path(&autosave_dir, &path).is_err() {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_file() {
            continue;
        }
        if remove_recovery_artifact(&path, "unreferenced recovery snapshot") {
            cleanup.removed_unreferenced_snapshot_count += 1;
        }
    }
    Ok(cleanup)
}

/// Establish the owned autosave directory before any worker-side file create.
pub(super) fn prepare_recovery_archive_target(
    runtime_lease: &ProjectRuntimeLease,
    autosave_file: &Path,
) -> Result<(), String> {
    let runtime_root = runtime_lease.runtime_root();
    let autosave_dir = recovery_autosave_dir(runtime_root);
    ensure_owned_runtime_child_directory(runtime_lease, &autosave_dir)?;
    validate_snapshot_path(&autosave_dir, autosave_file)
}

/// Fail-closed admission for writing one archive under a Project runtime root.
pub(super) fn validate_recovery_archive_target(
    runtime_lease: &ProjectRuntimeLease,
    autosave_file: &Path,
) -> Result<(), String> {
    let runtime_root = runtime_lease.runtime_root();
    let autosave_dir = recovery_autosave_dir(runtime_root);
    validate_owned_runtime_child_directory(runtime_lease, &autosave_dir)?;
    validate_snapshot_path(&autosave_dir, autosave_file)
}

/// Publish a newly written archive into the versioned recovery index.
///
/// The replacement manifest is durably committed before any entry it no longer
/// references is removed. Proven pre-namespace failure leaves the prior
/// manifest unchanged; post-namespace unconfirmed or indeterminate outcomes
/// suppress every archive deletion and remain explicit terminal states.
pub(super) fn publish_recovery_point(
    publication: RecoveryPointPublication<'_>,
) -> Result<(), RecoveryManifestPublicationFailure> {
    if publication.archive_evidence.publication() != ProjectArchivePublication::CreateNew {
        return Err("recovery archive publication must be create-only".to_owned().into());
    }
    let archive_evidence = publication.archive_evidence;
    let facts = RecoveryPointManifestFacts {
        project_file: publication.project_file,
        runtime_lease: publication.runtime_lease,
        autosave_file: archive_evidence.published_path(),
        author_generation: publication.author_generation,
        asset_library_revision: publication.asset_library_revision,
        document_revision: archive_evidence.document_revision(),
        saved_at_unix_ms: publication.saved_at_unix_ms,
        max_recovery_points: publication.max_recovery_points,
        retention_days: publication.retention_days,
    };
    publish_recovery_point_with_entry(facts, |facts, project_id| {
        if archive_evidence.project_id() != project_id {
            return Err(
                "Project archive publication evidence belongs to another Project".to_owned(),
            );
        }
        Ok(RecoverySnapshotEntry {
            file: facts.autosave_file.to_path_buf(),
            saved_at_unix_ms: facts.saved_at_unix_ms,
            author_generation: facts.author_generation,
            asset_library_revision: facts.asset_library_revision,
            document_revision: facts.document_revision,
            archive_sha256: archive_evidence.archive_sha256_hex(),
        })
    })
}

#[cfg(test)]
fn publish_recovery_point_for_test(
    publication: RecoveryPointPublicationForTest<'_>,
) -> Result<(), String> {
    let facts = RecoveryPointManifestFacts {
        project_file: publication.project_file,
        runtime_lease: publication.runtime_lease,
        autosave_file: publication.autosave_file,
        author_generation: publication.author_generation,
        asset_library_revision: publication.asset_library_revision,
        document_revision: publication.document_revision,
        saved_at_unix_ms: publication.saved_at_unix_ms,
        max_recovery_points: publication.max_recovery_points,
        retention_days: publication.retention_days,
    };
    publish_recovery_point_with_entry(facts, |facts, project_id| {
        verified_entry_from_archive(
            facts.autosave_file,
            project_id,
            facts.author_generation,
            facts.asset_library_revision,
            facts.document_revision,
            facts.saved_at_unix_ms,
        )
    })
    .map_err(|error| error.to_string())
}

fn publish_recovery_point_with_entry(
    publication: RecoveryPointManifestFacts<'_>,
    build_entry: impl FnOnce(
        &RecoveryPointManifestFacts<'_>,
        ProjectId,
    ) -> Result<RecoverySnapshotEntry, String>,
) -> Result<(), RecoveryManifestPublicationFailure> {
    validate_positive_publication_values(&publication)?;
    let _mutation = recovery_mutation_guard();
    validate_recovery_archive_target(publication.runtime_lease, publication.autosave_file)?;
    let project_id = publication.runtime_lease.project_id();
    let runtime_root = publication.runtime_lease.runtime_root();
    let autosave_dir = recovery_autosave_dir(runtime_root);
    let new_entry = build_entry(&publication, project_id)?;

    let manifest_path = recovery_manifest_path(runtime_root);
    let mut manifest = if path_entry_exists(&manifest_path)? {
        let manifest = read_manifest(&manifest_path)?;
        // ProjectId is the durable identity. Only manual-save reconciliation
        // may change the canonical path, so a queued autosave cannot flip a
        // completed Save As back to its previous path.
        manifest.validate_identity(project_id)?;
        manifest
    } else {
        RecoveryManifest::new(project_id, publication.project_file)?
    };

    let mut obsolete_files = Vec::new();
    manifest.snapshots =
        retain_well_formed_entries(manifest.snapshots, &autosave_dir, &mut obsolete_files);
    manifest.snapshots.retain(|entry| {
        if entry.file == new_entry.file {
            obsolete_files.push(entry.file.clone());
            false
        } else {
            true
        }
    });
    // Retention applies only to points that were already under manifest
    // authority. The archive being published by this transaction must enter
    // Recovery Authority even when the wall clock moved backwards or all
    // historical slots are exhausted. On the next publication it is an
    // ordinary historical entry and can be retained or expired normally.
    let retention_removed = apply_retention(
        &mut manifest.snapshots,
        publication.max_recovery_points.saturating_sub(1),
        publication.retention_days.max(1),
    );
    obsolete_files.extend(retention_removed);
    manifest.snapshots.push(new_entry);
    sort_and_deduplicate_entries(&mut manifest.snapshots);

    publish_manifest(&manifest_path, &manifest)?;
    remove_unreferenced_snapshot_files(&autosave_dir, obsolete_files, &manifest.snapshots);
    Ok(())
}

/// Inspect one user-selected recovery point before requesting its live lease.
///
/// Callers must subsequently use [`copy_recovery_selection_under_lease`].
pub(super) fn preflight_recovery_selection(
    candidate: &CrashRecoveryCandidate,
) -> Result<RecoverySelectionEvidence, String> {
    let project_file = candidate.project_file.as_path();
    let autosave_file = candidate.autosave_file.as_path();
    if !project_file.is_absolute() {
        return Err("selected recovery Project path is not absolute".to_owned());
    }
    if !autosave_file.is_absolute() {
        return Err("selected recovery point path is not absolute".to_owned());
    }
    let autosave_dir = autosave_file
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "autosave"))
        .ok_or_else(|| "selected recovery point is not inside an autosave directory".to_owned())?;
    let runtime_root = autosave_dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "selected recovery point has no Project runtime authority".to_owned())?;
    if runtime_root != candidate.runtime_root {
        return Err("selected recovery point runtime root does not match its candidate".to_owned());
    }
    let manifest_path = recovery_manifest_path(runtime_root);
    let manifest = read_manifest(&manifest_path)?;
    manifest.validate_identity(candidate.project_id)?;
    if manifest.project_id != candidate.project_id {
        return Err("selected recovery point Project identity changed after discovery".to_owned());
    }
    let autosave_dir = recovery_autosave_dir(runtime_root);
    validate_runtime_child_directory_readonly(runtime_root, &autosave_dir, manifest.project_id)?;
    validate_snapshot_path(&autosave_dir, autosave_file)?;
    if manifest.project_file != project_file {
        return Err("selected recovery point belongs to another Project path".to_owned());
    }
    let entry = manifest.exact_snapshot(autosave_file)?;
    if !candidate_matches_entry(candidate, entry) {
        return Err("selected recovery point evidence changed after discovery".to_owned());
    }
    verify_entry(entry, &autosave_dir, manifest.project_id)?;
    validate_canonical_recovery_target(
        project_file,
        manifest.project_id,
        entry.document_revision,
        candidate.canonical_target,
    )?;
    Ok(RecoverySelectionEvidence {
        runtime_root: runtime_root.to_path_buf(),
        project_id: manifest.project_id,
        project_file: project_file.to_path_buf(),
        canonical_target: candidate.canonical_target,
        exact_entry: entry.clone(),
    })
}

/// Re-admit and copy one exact recovery entry while its Project lease is live.
///
/// The manifest entry, source archive hash/revision, and canonical Project
/// identity are checked again after lease acquisition. The copied bytes are
/// independently checked before they may be opened, and the manifest is
/// matched once more while the process-local recovery mutation guard is held.
pub(super) fn copy_recovery_selection_under_lease(
    evidence: &RecoverySelectionEvidence,
    runtime_lease: &ProjectRuntimeLease,
    staged_archive: &Path,
) -> Result<VerifiedRecoveryArchive, String> {
    let _mutation = recovery_mutation_guard();
    runtime_lease.validate()?;
    if runtime_lease.runtime_root() != evidence.runtime_root {
        return Err("recovery selection belongs to another Project runtime root".to_owned());
    }
    if runtime_lease.project_id() != evidence.project_id {
        return Err("recovery selection belongs to another Project identity".to_owned());
    }

    validate_recovery_evidence_under_lease(evidence, runtime_lease)?;
    if staged_archive.parent() != Some(runtime_lease.runtime_root())
        || staged_archive.file_name().is_none()
    {
        return Err(
            "recovery staging archive is not a direct child of the leased runtime".to_owned(),
        );
    }
    let mut source =
        open_direct_read_file(&evidence.exact_entry.file, "selected recovery archive")?;
    verify_archive_evidence_from_open_file(
        &mut source,
        &evidence.exact_entry,
        evidence.project_id,
    )?;
    source
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("failed to rewind selected recovery archive: {error}"))?;
    let mut staged = create_direct_exclusive_file(staged_archive, "recovery staging archive")?;
    if let Err(error) = std::io::copy(&mut source, &mut staged) {
        drop(staged);
        let _ = fs::remove_file(staged_archive);
        return Err(format!("failed to copy selected recovery archive: {error}"));
    }
    if let Err(error) = staged.sync_all() {
        drop(staged);
        let _ = fs::remove_file(staged_archive);
        return Err(format!("failed to flush copied recovery archive: {error}"));
    }
    if let Err(error) = verify_archive_evidence_from_open_file(
        &mut staged,
        &evidence.exact_entry,
        evidence.project_id,
    ) {
        drop(staged);
        let _ = fs::remove_file(staged_archive);
        return Err(format!(
            "copied recovery archive failed exact verification: {error}"
        ));
    }
    if let Err(error) = validate_recovery_evidence_under_lease(evidence, runtime_lease) {
        drop(staged);
        let _ = fs::remove_file(staged_archive);
        return Err(error);
    }
    if let Err(error) = staged.seek(SeekFrom::Start(0)) {
        drop(staged);
        let _ = fs::remove_file(staged_archive);
        return Err(format!(
            "failed to rewind verified recovery archive: {error}"
        ));
    }
    Ok(VerifiedRecoveryArchive {
        path: staged_archive.to_path_buf(),
        file: Some(staged),
    })
}

fn validate_recovery_evidence_under_lease(
    evidence: &RecoverySelectionEvidence,
    runtime_lease: &ProjectRuntimeLease,
) -> Result<(), String> {
    let runtime_root = runtime_lease.runtime_root();
    let autosave_dir = recovery_autosave_dir(runtime_root);
    validate_owned_runtime_child_directory(runtime_lease, &autosave_dir)?;
    let manifest = read_manifest(&recovery_manifest_path(runtime_root))?;
    manifest.validate_identity(evidence.project_id)?;
    if manifest.project_file != evidence.project_file {
        return Err("canonical recovery Project path changed after selection".to_owned());
    }
    manifest.validate_unique_snapshot_paths()?;
    let current = manifest
        .snapshots
        .iter()
        .find(|entry| entry.file == evidence.exact_entry.file)
        .ok_or_else(|| "selected recovery point was retired before lease admission".to_owned())?;
    if current != &evidence.exact_entry {
        return Err("selected recovery point evidence changed before lease admission".to_owned());
    }
    verify_entry(current, &autosave_dir, evidence.project_id)?;
    validate_canonical_recovery_target(
        &evidence.project_file,
        evidence.project_id,
        current.document_revision,
        evidence.canonical_target,
    )?;
    Ok(())
}

/// Discover only recovery points whose manifest, path, hash, archive, and
/// Project identity all validate.
pub(crate) fn discover_crash_recovery_candidates() -> Vec<CrashRecoveryCandidate> {
    match project_runtime_parent() {
        Ok(runtime_parent) => discover_crash_recovery_candidates_under(&runtime_parent),
        Err(error) => {
            tracing::warn!(%error, "stable Project recovery state root is unavailable");
            Vec::new()
        }
    }
}

fn discover_crash_recovery_candidates_under(root: &Path) -> Vec<CrashRecoveryCandidate> {
    let mut candidates = Vec::new();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return candidates,
    };

    for entry in entries.flatten() {
        let runtime_root = entry.path();
        let manifest_path = recovery_manifest_path(&runtime_root);
        let Ok(manifest) = read_manifest(&manifest_path) else {
            continue;
        };
        let autosave_dir = recovery_autosave_dir(&runtime_root);
        if manifest.validate_identity(manifest.project_id).is_err()
            || manifest.validate_unique_snapshot_paths().is_err()
            || validate_runtime_owner_readonly(&runtime_root, manifest.project_id).is_err()
            || validate_runtime_child_directory_readonly(
                &runtime_root,
                &autosave_dir,
                manifest.project_id,
            )
            .is_err()
        {
            continue;
        }
        let canonical_target =
            match canonical_recovery_target(&manifest.project_file, manifest.project_id) {
                Ok(target) => target,
                Err(_) => continue,
            };
        let verified = manifest
            .snapshots
            .iter()
            .filter(|snapshot| {
                canonical_target
                    .document_revision()
                    .is_none_or(|canonical| snapshot.document_revision >= canonical)
                    && verify_entry(snapshot, &autosave_dir, manifest.project_id).is_ok()
            })
            .collect::<Vec<_>>();
        let total_snapshots = verified.len();
        for snapshot in verified {
            candidates.push(CrashRecoveryCandidate {
                project_id: manifest.project_id,
                runtime_root: runtime_root.clone(),
                project_file: manifest.project_file.clone(),
                canonical_target,
                autosave_file: snapshot.file.clone(),
                author_generation: snapshot.author_generation,
                asset_library_revision: snapshot.asset_library_revision,
                document_revision: snapshot.document_revision,
                archive_sha256: snapshot.archive_sha256.clone(),
                saved_at_unix_ms: snapshot.saved_at_unix_ms,
                total_snapshots,
            });
        }
    }
    candidates.sort_by(|left, right| {
        right
            .saved_at_unix_ms
            .cmp(&left.saved_at_unix_ms)
            .then_with(|| left.project_id.cmp(&right.project_id))
            .then_with(|| left.runtime_root.cmp(&right.runtime_root))
            .then_with(|| left.autosave_file.cmp(&right.autosave_file))
    });
    candidates
}

fn canonical_project_document(
    project_file: &Path,
    project_id: ProjectId,
) -> Result<Option<mondrian_project::ProjectDocument>, String> {
    if !project_file.is_absolute() {
        return Err("canonical Project path is not absolute".to_owned());
    }
    match fs::symlink_metadata(project_file) {
        Ok(_) => {
            let document = read_direct_project_document(project_file, "canonical Project archive")?;
            if document.project_id != project_id {
                return Err("recovery manifest does not belong to the canonical Project".to_owned());
            }
            Ok(Some(document))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "failed to inspect canonical Project archive: {error}"
        )),
    }
}

fn canonical_recovery_target(
    project_file: &Path,
    project_id: ProjectId,
) -> Result<RecoveryCanonicalTargetEvidence, String> {
    Ok(
        match canonical_project_document(project_file, project_id)? {
            Some(document) => RecoveryCanonicalTargetEvidence::Present {
                document_revision: document.document_revision,
            },
            None => RecoveryCanonicalTargetEvidence::Missing,
        },
    )
}

fn validate_canonical_recovery_target(
    project_file: &Path,
    project_id: ProjectId,
    snapshot_document_revision: u64,
    expected: RecoveryCanonicalTargetEvidence,
) -> Result<(), String> {
    let current = canonical_recovery_target(project_file, project_id)?;
    if current != expected {
        return Err(format!(
            "canonical Project target changed after recovery discovery (expected {expected:?}, observed {current:?})"
        ));
    }
    if let Some(canonical_revision) = current.document_revision()
        && snapshot_document_revision < canonical_revision
    {
        return Err(format!(
            "selected recovery point document revision {} is older than canonical revision {}",
            snapshot_document_revision, canonical_revision
        ));
    }
    Ok(())
}

fn read_direct_project_document(
    project_file: &Path,
    description: &str,
) -> Result<mondrian_project::ProjectDocument, String> {
    let mut file = open_direct_read_file(project_file, description)?;
    read_project_document_from_open_archive(&mut file)
        .map_err(|error| format!("{description} is invalid: {error:#}"))
}

/// Reconcile recovery authority after a successful manual Project publication.
///
/// A clean save publishes an empty manifest before deleting covered archives.
/// A stale Save As completion only rebinds retained authority to the new
/// canonical path, because newer author state must remain recoverable.
pub(super) fn reconcile_recovery_after_manual_save(
    runtime_lease: &ProjectRuntimeLease,
    current_project_file: &Path,
    retire_all: bool,
) -> Result<RecoveryReconciliation, RecoveryManifestPublicationFailure> {
    if !current_project_file.is_absolute() {
        return Err("recovery reconciliation Project path is not absolute".to_owned().into());
    }
    let _mutation = recovery_mutation_guard();
    runtime_lease.validate()?;
    let runtime_root = runtime_lease.runtime_root();
    let project_id = runtime_lease.project_id();
    let manifest_path = recovery_manifest_path(runtime_root);
    if !path_entry_exists(&manifest_path)? {
        return Ok(RecoveryReconciliation { removed_snapshot_count: 0 });
    }
    let mut manifest = read_manifest(&manifest_path)?;
    manifest.validate_identity(project_id)?;
    let autosave_dir = recovery_autosave_dir(runtime_root);
    validate_owned_runtime_child_directory(runtime_lease, &autosave_dir)?;
    let mut obsolete_files = Vec::new();
    manifest.snapshots =
        retain_well_formed_entries(manifest.snapshots, &autosave_dir, &mut obsolete_files);
    sort_and_deduplicate_entries(&mut manifest.snapshots);
    manifest.project_file = current_project_file.to_path_buf();

    if retire_all {
        obsolete_files.extend(manifest.snapshots.drain(..).map(|entry| entry.file));
    }
    let removed_snapshot_count = if retire_all {
        obsolete_files.iter().collect::<HashSet<_>>().len()
    } else {
        0
    };
    publish_manifest(&manifest_path, &manifest)?;
    remove_unreferenced_snapshot_files(&autosave_dir, obsolete_files, &manifest.snapshots);
    Ok(RecoveryReconciliation { removed_snapshot_count })
}

fn recovery_mutation_guard() -> MutexGuard<'static, ()> {
    match RECOVERY_MUTATION_LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                "Project recovery mutation lock was poisoned; recovering stateless file authority"
            );
            poisoned.into_inner()
        }
    }
}

fn validate_positive_publication_values(
    publication: &RecoveryPointManifestFacts<'_>,
) -> Result<(), String> {
    if !publication.project_file.is_absolute() {
        return Err("recovery source Project path is not absolute".to_owned());
    }
    if publication.author_generation == 0 {
        return Err("recovery author generation must be positive".to_owned());
    }
    if publication.document_revision == 0 {
        return Err("recovery document revision must be positive".to_owned());
    }
    Ok(())
}

fn recovery_autosave_dir(runtime_root: &Path) -> PathBuf {
    runtime_root.join("autosave")
}

fn path_entry_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "failed to inspect recovery namespace entry {}: {error}",
            path.display()
        )),
    }
}

fn validate_snapshot_path(autosave_dir: &Path, snapshot: &Path) -> Result<(), String> {
    if !autosave_dir.is_absolute() || !snapshot.is_absolute() {
        return Err("recovery snapshot path is not absolute".to_owned());
    }
    if snapshot.parent() != Some(autosave_dir) {
        return Err(format!(
            "recovery snapshot is outside the Project autosave directory: {}",
            snapshot.display()
        ));
    }
    let valid_name = snapshot
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".autosave.mdp"));
    if !valid_name {
        return Err(format!(
            "invalid recovery snapshot file name: {}",
            snapshot.display()
        ));
    }
    Ok(())
}

fn read_manifest(path: &Path) -> Result<RecoveryManifest, String> {
    if !path.is_absolute() {
        return Err("Recovery Manifest path is not absolute".to_owned());
    }
    let file = open_direct_read_file(path, "Recovery Manifest")?;
    let length = file
        .metadata()
        .map_err(|error| format!("failed to inspect Recovery Manifest: {error}"))?
        .len();
    if length > MAX_RECOVERY_MANIFEST_BYTES {
        return Err(format!(
            "Recovery Manifest exceeds the {} byte limit",
            MAX_RECOVERY_MANIFEST_BYTES
        ));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(MAX_RECOVERY_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read Recovery Manifest: {error}"))?;
    if bytes.len() as u64 > MAX_RECOVERY_MANIFEST_BYTES {
        return Err(format!(
            "Recovery Manifest exceeds the {} byte limit",
            MAX_RECOVERY_MANIFEST_BYTES
        ));
    }
    let manifest: RecoveryManifest =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    manifest.validate_identity(manifest.project_id)?;
    manifest.validate_unique_snapshot_paths()?;
    Ok(manifest)
}

fn publish_manifest(
    path: &Path,
    manifest: &RecoveryManifest,
) -> Result<(), RecoveryManifestPublicationFailure> {
    if !path.is_absolute() {
        return Err(RecoveryManifestPublicationFailure::BeforeNamespace(
            anyhow::anyhow!("Recovery Manifest publication path is not absolute"),
        ));
    }
    manifest.validate_identity(manifest.project_id).map_err(|error| {
        RecoveryManifestPublicationFailure::BeforeNamespace(anyhow::anyhow!(error))
    })?;
    manifest.validate_unique_snapshot_paths().map_err(|error| {
        RecoveryManifestPublicationFailure::BeforeNamespace(anyhow::anyhow!(error))
    })?;
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|error| {
        RecoveryManifestPublicationFailure::BeforeNamespace(anyhow::Error::new(error))
    })?;
    write_durable_file_atomically(path, &bytes)
        .map(|_| ())
        .map_err(RecoveryManifestPublicationFailure::from)
}

#[cfg(test)]
fn verified_entry_from_archive(
    file: &Path,
    project_id: ProjectId,
    author_generation: u64,
    asset_library_revision: u64,
    document_revision: u64,
    saved_at_unix_ms: u64,
) -> Result<RecoverySnapshotEntry, String> {
    let mut archive = open_direct_read_file(file, "recovery archive")?;
    let archive_sha256 = sha256_open_file(&mut archive)?;
    let document = read_project_document_from_open_archive(&mut archive)
        .map_err(|error| format!("recovery archive validation failed: {error:#}"))?;
    if document.project_id != project_id {
        return Err("recovery archive Project identity does not match".to_owned());
    }
    if document.document_revision != document_revision {
        return Err("recovery archive document revision does not match".to_owned());
    }
    Ok(RecoverySnapshotEntry {
        file: file.to_path_buf(),
        saved_at_unix_ms,
        author_generation,
        asset_library_revision,
        document_revision,
        archive_sha256,
    })
}

fn verify_entry(
    entry: &RecoverySnapshotEntry,
    autosave_dir: &Path,
    project_id: ProjectId,
) -> Result<(), String> {
    validate_snapshot_path(autosave_dir, &entry.file)?;
    validate_snapshot_entry_identity(entry)?;
    verify_archive_evidence(&entry.file, entry, project_id)
}

fn validate_snapshot_entry_identity(entry: &RecoverySnapshotEntry) -> Result<(), String> {
    if entry.author_generation == 0 || entry.document_revision == 0 {
        return Err("recovery snapshot has an invalid zero revision".to_owned());
    }
    if entry.archive_sha256.len() != 64
        || !entry.archive_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("recovery snapshot has an invalid SHA-256 identity".to_owned());
    }
    Ok(())
}

fn verify_archive_evidence(
    archive_file: &Path,
    entry: &RecoverySnapshotEntry,
    project_id: ProjectId,
) -> Result<(), String> {
    let mut file = open_direct_read_file(archive_file, "recovery archive")?;
    verify_archive_evidence_from_open_file(&mut file, entry, project_id)
}

fn verify_archive_evidence_from_open_file(
    archive_file: &mut fs::File,
    entry: &RecoverySnapshotEntry,
    project_id: ProjectId,
) -> Result<(), String> {
    let actual_sha256 = sha256_open_file(archive_file)?;
    if actual_sha256 != entry.archive_sha256 {
        return Err("recovery snapshot content hash does not match".to_owned());
    }
    let document = read_project_document_from_open_archive(archive_file)
        .map_err(|error| format!("recovery archive validation failed: {error:#}"))?;
    if document.project_id != project_id {
        return Err("recovery snapshot belongs to another Project".to_owned());
    }
    if document.document_revision != entry.document_revision {
        return Err("recovery snapshot document revision does not match".to_owned());
    }
    Ok(())
}

fn retain_well_formed_entries(
    entries: Vec<RecoverySnapshotEntry>,
    autosave_dir: &Path,
    obsolete_files: &mut Vec<PathBuf>,
) -> Vec<RecoverySnapshotEntry> {
    entries
        .into_iter()
        .filter_map(|entry| {
            let safe_path = validate_snapshot_path(autosave_dir, &entry.file).is_ok();
            let valid_identity = entry.author_generation > 0
                && entry.document_revision > 0
                && entry.archive_sha256.len() == 64
                && entry.archive_sha256.bytes().all(|byte| byte.is_ascii_hexdigit());
            if safe_path
                && valid_identity
                && open_direct_read_file(&entry.file, "recovery archive").is_ok()
            {
                Some(entry)
            } else {
                if safe_path {
                    obsolete_files.push(entry.file);
                }
                None
            }
        })
        .collect()
}

fn apply_retention(
    entries: &mut Vec<RecoverySnapshotEntry>,
    max_recovery_points: usize,
    retention_days: u32,
) -> Vec<PathBuf> {
    sort_and_deduplicate_entries(entries);
    let retention_ms = u64::from(retention_days)
        .saturating_mul(24)
        .saturating_mul(60)
        .saturating_mul(60)
        .saturating_mul(1000);
    let cutoff_ms = unix_now_ms().saturating_sub(retention_ms);
    let mut removed = Vec::new();
    entries.retain(|entry| {
        if entry.saved_at_unix_ms < cutoff_ms {
            removed.push(entry.file.clone());
            false
        } else {
            true
        }
    });
    if entries.len() > max_recovery_points {
        removed.extend(entries.drain(max_recovery_points..).map(|entry| entry.file));
    }
    removed
}

fn sort_and_deduplicate_entries(entries: &mut Vec<RecoverySnapshotEntry>) {
    entries.sort_by(|left, right| {
        right
            .saved_at_unix_ms
            .cmp(&left.saved_at_unix_ms)
            .then_with(|| right.author_generation.cmp(&left.author_generation))
            .then_with(|| left.file.cmp(&right.file))
    });
    let mut retained_files = HashSet::new();
    entries.retain(|entry| retained_files.insert(entry.file.clone()));
}

fn remove_unreferenced_snapshot_files(
    autosave_dir: &Path,
    files: impl IntoIterator<Item = PathBuf>,
    retained: &[RecoverySnapshotEntry],
) {
    let retained = retained.iter().map(|entry| entry.file.clone()).collect::<HashSet<_>>();
    for file in files {
        if retained.contains(&file) || validate_snapshot_path(autosave_dir, &file).is_err() {
            continue;
        }
        match fs::remove_file(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(path = %file.display(), %error, "failed to remove retired recovery snapshot");
            }
        }
    }
}

fn remove_recovery_artifact(path: &Path, description: &str) -> bool {
    match fs::remove_file(path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "failed to remove {description}"
            );
            false
        }
    }
}

#[cfg(windows)]
fn staging_path_still_names_open_file(_path: &Path, _file: &fs::File) -> bool {
    // `create_direct_exclusive_file` opens the staging leaf without
    // FILE_SHARE_DELETE. Windows therefore prevents unlink/rename replacement
    // until this exact handle is closed.
    true
}

#[cfg(unix)]
fn staging_path_still_names_open_file(path: &Path, file: &fs::File) -> bool {
    let Ok(path_metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if !path_metadata.file_type().is_file() {
        return false;
    }
    let Ok(open_metadata) = file.metadata() else {
        return false;
    };
    use std::os::unix::fs::MetadataExt;
    path_metadata.dev() == open_metadata.dev() && path_metadata.ino() == open_metadata.ino()
}

#[cfg(not(any(unix, windows)))]
fn staging_path_still_names_open_file(_path: &Path, _file: &fs::File) -> bool {
    false
}

fn sha256_open_file(file: &mut fs::File) -> Result<String, String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("failed to rewind recovery archive: {error}"))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::super::project_runtime::claim_project_runtime_lease_for_test;
    use super::*;
    use mondrian_assets::AssetLibrary;
    use mondrian_core::{ProjectColorEnvironment, ProjectSettings};
    use mondrian_project::{
        save_project_archive, save_project_archive_with_publication, ProjectDocument,
    };
    use mondrian_timeline::{Sequence, SequenceCollection, SequenceSettings};
    use std::io::Write;
    use std::sync::Arc;

    fn fixture(
        runtime_parent: &Path,
        project_file: &Path,
    ) -> (ProjectDocument, Arc<ProjectRuntimeLease>, PathBuf) {
        let sequence =
            Sequence::with_settings("Recovery", SequenceSettings::default()).expect("sequence");
        let mut document = ProjectDocument::new(
            "Recovery",
            SequenceCollection::new(sequence),
            ProjectColorEnvironment::default(),
            SequenceSettings::default(),
            ProjectSettings::default(),
        );
        document.document_revision = 2;
        let runtime_lease =
            claim_project_runtime_lease_for_test(runtime_parent, project_file, document.project_id)
                .expect("claim runtime owner");
        let runtime_root = runtime_lease.runtime_root();
        fs::create_dir_all(runtime_root.join("library")).expect("create library root");
        let library = AssetLibrary::open(runtime_root.join("library")).expect("open library");
        let autosave = runtime_root.join("autosave").join("fixture.autosave.mdp");
        fs::create_dir_all(autosave.parent().expect("autosave parent")).expect("autosave root");
        save_project_archive(&document, &library.database_path(), &autosave)
            .expect("save recovery archive");
        (document, runtime_lease, autosave)
    }

    fn fixture_for_document(
        runtime_parent: &Path,
        project_file: &Path,
        document: &ProjectDocument,
        autosave_name: &str,
    ) -> (Arc<ProjectRuntimeLease>, PathBuf) {
        let runtime_lease =
            claim_project_runtime_lease_for_test(runtime_parent, project_file, document.project_id)
                .expect("claim runtime owner");
        let runtime_root = runtime_lease.runtime_root();
        fs::create_dir_all(runtime_root.join("library")).expect("create library root");
        let library = AssetLibrary::open(runtime_root.join("library")).expect("open library");
        let autosave = runtime_root.join("autosave").join(autosave_name);
        fs::create_dir_all(autosave.parent().expect("autosave parent")).expect("autosave root");
        save_project_archive(document, &library.database_path(), &autosave)
            .expect("save recovery archive");
        (runtime_lease, autosave)
    }

    fn publication_evidence_fixture(
        runtime_parent: &Path,
        project_file: &Path,
        publication: ProjectArchivePublication,
        autosave_name: &str,
    ) -> (
        ProjectDocument,
        Arc<ProjectRuntimeLease>,
        ProjectArchivePublicationEvidence,
    ) {
        let sequence =
            Sequence::with_settings("Recovery", SequenceSettings::default()).expect("sequence");
        let mut document = ProjectDocument::new(
            "Recovery",
            SequenceCollection::new(sequence),
            ProjectColorEnvironment::default(),
            SequenceSettings::default(),
            ProjectSettings::default(),
        );
        document.document_revision = 2;
        let runtime_lease =
            claim_project_runtime_lease_for_test(runtime_parent, project_file, document.project_id)
                .expect("claim runtime owner");
        let runtime_root = runtime_lease.runtime_root();
        fs::create_dir_all(runtime_root.join("library")).expect("create library root");
        let library = AssetLibrary::open(runtime_root.join("library")).expect("open library");
        let autosave = runtime_root.join("autosave").join(autosave_name);
        fs::create_dir_all(autosave.parent().expect("autosave parent")).expect("autosave root");
        let evidence = save_project_archive_with_publication(
            &document,
            &library.database_path(),
            &autosave,
            publication,
        )
        .expect("publish recovery archive");
        (document, runtime_lease, evidence)
    }

    fn publication<'a>(
        document: &ProjectDocument,
        project_file: &'a Path,
        runtime_lease: &'a ProjectRuntimeLease,
        autosave_file: &'a Path,
        saved_at_unix_ms: u64,
    ) -> RecoveryPointPublicationForTest<'a> {
        RecoveryPointPublicationForTest {
            project_file,
            runtime_lease,
            autosave_file,
            author_generation: 2,
            asset_library_revision: 0,
            document_revision: document.document_revision,
            saved_at_unix_ms,
            max_recovery_points: 10,
            retention_days: 365,
        }
    }

    fn publish_recovery_point(
        publication: RecoveryPointPublicationForTest<'_>,
    ) -> Result<(), String> {
        super::publish_recovery_point_for_test(publication)
    }

    fn candidate_for(
        runtime_lease: &ProjectRuntimeLease,
        autosave_file: &Path,
    ) -> CrashRecoveryCandidate {
        let manifest =
            read_manifest(&recovery_manifest_path(runtime_lease.runtime_root())).expect("manifest");
        let entry = manifest.exact_snapshot(autosave_file).expect("manifest entry").clone();
        CrashRecoveryCandidate {
            project_id: manifest.project_id,
            runtime_root: runtime_lease.runtime_root().to_path_buf(),
            canonical_target: canonical_recovery_target(
                &manifest.project_file,
                manifest.project_id,
            )
            .expect("canonical target evidence"),
            project_file: manifest.project_file,
            autosave_file: entry.file,
            author_generation: entry.author_generation,
            asset_library_revision: entry.asset_library_revision,
            document_revision: entry.document_revision,
            archive_sha256: entry.archive_sha256,
            saved_at_unix_ms: entry.saved_at_unix_ms,
            total_snapshots: manifest.snapshots.len(),
        }
    }

    fn unique_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mondrian_recovery_{name}_{}_{}",
            std::process::id(),
            unix_now_ms()
        ))
    }

    #[test]
    fn manifest_validation_failure_retains_before_namespace_phase() {
        let project_id = ProjectId::new();
        let absolute_project = unique_root("manifest-phase").join("project.mdp");
        let manifest =
            RecoveryManifest::new(project_id, &absolute_project).expect("valid manifest");

        let error = publish_manifest(Path::new("relative-manifest.json"), &manifest)
            .expect_err("relative target must fail before publication");

        assert!(matches!(
            &error,
            RecoveryManifestPublicationFailure::BeforeNamespace(_)
        ));
    }

    #[test]
    fn production_publication_derives_manifest_identity_from_create_new_evidence() {
        let scan_root = unique_root("typed-publication");
        let project_file = scan_root.join("project.mdp");
        let (document, runtime_lease, evidence) = publication_evidence_fixture(
            &scan_root,
            &project_file,
            ProjectArchivePublication::CreateNew,
            "typed.autosave.mdp",
        );
        let autosave_file = evidence.published_path().to_path_buf();
        let archive_sha256 = evidence.archive_sha256_hex();
        let saved_at_unix_ms = unix_now_ms();

        super::publish_recovery_point(RecoveryPointPublication {
            project_file: &project_file,
            runtime_lease: &runtime_lease,
            archive_evidence: evidence,
            author_generation: 7,
            asset_library_revision: 3,
            saved_at_unix_ms,
            max_recovery_points: 10,
            retention_days: 365,
        })
        .expect("publish typed recovery point");

        let manifest =
            read_manifest(&recovery_manifest_path(runtime_lease.runtime_root())).expect("manifest");
        assert_eq!(manifest.project_id, document.project_id);
        assert_eq!(manifest.project_file, project_file);
        assert_eq!(manifest.snapshots.len(), 1);
        let entry = &manifest.snapshots[0];
        assert_eq!(entry.file, autosave_file);
        assert_eq!(entry.document_revision, document.document_revision);
        assert_eq!(entry.author_generation, 7);
        assert_eq!(entry.asset_library_revision, 3);
        assert_eq!(entry.saved_at_unix_ms, saved_at_unix_ms);
        assert_eq!(entry.archive_sha256, archive_sha256);

        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn production_publication_rejects_replace_existing_evidence() {
        let scan_root = unique_root("replacement-evidence");
        let project_file = scan_root.join("project.mdp");
        let (_document, runtime_lease, evidence) = publication_evidence_fixture(
            &scan_root,
            &project_file,
            ProjectArchivePublication::ReplaceExisting,
            "replacement.autosave.mdp",
        );

        let error = super::publish_recovery_point(RecoveryPointPublication {
            project_file: &project_file,
            runtime_lease: &runtime_lease,
            archive_evidence: evidence,
            author_generation: 7,
            asset_library_revision: 3,
            saved_at_unix_ms: unix_now_ms(),
            max_recovery_points: 10,
            retention_days: 365,
        })
        .expect_err("replacement evidence must not enter recovery authority");

        assert!(error.to_string().contains("create-only"), "{error}");
        assert!(
            !recovery_manifest_path(runtime_lease.runtime_root()).exists(),
            "rejected replacement evidence must not publish a manifest"
        );
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn publication_discovery_and_retirement_validate_exact_identity() {
        let scan_root = unique_root("lifecycle");
        let project_file = scan_root.join("project.mdp");
        let (document, runtime_lease, autosave) = fixture(&scan_root, &project_file);
        let runtime_root = runtime_lease.runtime_root();
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &autosave,
            unix_now_ms(),
        ))
        .expect("publish recovery point");

        let candidates = discover_crash_recovery_candidates_under(&scan_root);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].project_file, project_file);
        assert_eq!(candidates[0].autosave_file, autosave);
        let candidate = candidate_for(&runtime_lease, &autosave);
        let selection = preflight_recovery_selection(&candidate).expect("inspect selection");
        assert_eq!(selection.runtime_root.as_path(), runtime_root);
        assert_eq!(selection.project_id, document.project_id);

        let retirement = reconcile_recovery_after_manual_save(&runtime_lease, &project_file, true)
            .expect("retire recovery points");
        assert_eq!(retirement.removed_snapshot_count, 1);
        assert!(!autosave.exists());
        assert!(discover_crash_recovery_candidates_under(&scan_root).is_empty());
        let manifest = read_manifest(&recovery_manifest_path(runtime_root)).expect("manifest");
        assert!(manifest.snapshots.is_empty());
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn discovery_orders_same_project_multi_root_candidates_deterministically() {
        let scan_root = unique_root("multi-root-order");
        let first_project_file = scan_root.join("first.mdp");
        let second_project_file = scan_root.join("second.mdp");
        let (document, first_lease, first_autosave) = fixture(&scan_root, &first_project_file);
        let first_runtime_root = first_lease.runtime_root().to_path_buf();
        let shared_saved_at = unix_now_ms();
        publish_recovery_point(publication(
            &document,
            &first_project_file,
            &first_lease,
            &first_autosave,
            shared_saved_at,
        ))
        .expect("publish first root");
        drop(first_lease);

        let (second_lease, second_autosave) = fixture_for_document(
            &scan_root,
            &second_project_file,
            &document,
            "second.autosave.mdp",
        );
        let second_runtime_root = second_lease.runtime_root().to_path_buf();
        publish_recovery_point(publication(
            &document,
            &second_project_file,
            &second_lease,
            &second_autosave,
            shared_saved_at,
        ))
        .expect("publish second root");
        drop(second_lease);

        let candidates = discover_crash_recovery_candidates_under(&scan_root);
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|candidate| candidate.project_id == document.project_id));
        let mut expected_roots = vec![first_runtime_root.clone(), second_runtime_root.clone()];
        expected_roots.sort();
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.runtime_root.clone())
                .collect::<Vec<_>>(),
            expected_roots,
            "equal-time candidates require a stable Project/root/path tie-break"
        );

        let mut wrong_root = candidates
            .iter()
            .find(|candidate| candidate.runtime_root == first_runtime_root)
            .expect("first candidate")
            .clone();
        wrong_root.runtime_root = second_runtime_root;
        let error = preflight_recovery_selection(&wrong_root)
            .expect_err("a path-only match must not cross paired runtime roots");
        assert!(error.contains("runtime root does not match"));

        let mut wrong_revision = candidates[0].clone();
        wrong_revision.document_revision += 1;
        let error = preflight_recovery_selection(&wrong_revision)
            .expect_err("candidate evidence must match the exact manifest entry");
        assert!(error.contains("evidence changed"));

        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn discovery_hides_only_autosaves_older_than_the_same_project_canonical_revision() {
        let scan_root = unique_root("canonical-revision");
        let project_file = scan_root.join("project.mdp");
        let (mut document, runtime_lease, autosave) = fixture(&scan_root, &project_file);
        let saved_at = unix_now_ms();
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &autosave,
            saved_at,
        ))
        .expect("publish recovery point");
        let missing_candidates = discover_crash_recovery_candidates_under(&scan_root);
        assert_eq!(missing_candidates.len(), 1);
        assert_eq!(
            missing_candidates[0].canonical_target,
            RecoveryCanonicalTargetEvidence::Missing,
            "a missing canonical archive must remain explicit recovery evidence"
        );
        let missing_target_candidate = missing_candidates[0].clone();

        let library =
            AssetLibrary::open(runtime_lease.runtime_root().join("library")).expect("library");
        save_project_archive(&document, &library.database_path(), &project_file)
            .expect("publish equal canonical revision");
        let equal_candidates = discover_crash_recovery_candidates_under(&scan_root);
        assert_eq!(equal_candidates.len(), 1);
        assert_eq!(
            equal_candidates[0].canonical_target,
            RecoveryCanonicalTargetEvidence::Present {
                document_revision: document.document_revision,
            },
            "an equal canonical revision remains an explicit admissible target"
        );
        let error = preflight_recovery_selection(&missing_target_candidate)
            .expect_err("a target created after discovery requires fresh user confirmation");
        assert!(error.contains("target changed after recovery discovery"));
        let equal_target_candidate = equal_candidates[0].clone();
        preflight_recovery_selection(&equal_target_candidate)
            .expect("fresh equal-revision evidence remains selectable");

        document.document_revision += 1;
        save_project_archive(&document, &library.database_path(), &project_file)
            .expect("publish newer canonical revision");
        assert!(
            discover_crash_recovery_candidates_under(&scan_root).is_empty(),
            "an autosave older than the same Project canonical revision is stale"
        );
        let error = preflight_recovery_selection(&equal_target_candidate)
            .expect_err("a canonical target changed after confirmation must fail admission");
        assert!(error.contains("target changed after recovery discovery"));
        let mut stale_candidate = equal_target_candidate;
        stale_candidate.canonical_target = RecoveryCanonicalTargetEvidence::Present {
            document_revision: document.document_revision,
        };
        let error = preflight_recovery_selection(&stale_candidate)
            .expect_err("a recovery point older than a freshly observed target must fail");
        assert!(error.contains("older than canonical revision"));

        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn leased_cleanup_removes_only_proven_staging_and_unreferenced_snapshots() {
        let scan_root = unique_root("artifact-cleanup");
        let project_file = scan_root.join("project.mdp");
        let (document, runtime_lease, autosave) = fixture(&scan_root, &project_file);
        let saved_at = unix_now_ms();
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &autosave,
            saved_at,
        ))
        .expect("publish recovery point");
        let runtime_root = runtime_lease.runtime_root();
        let orphan = runtime_root.join("autosave").join("orphan.autosave.mdp");
        fs::copy(&autosave, &orphan).expect("create unreferenced snapshot");
        let staging = runtime_root.join(".recovery-open-abandoned.staging.mdp");
        fs::write(&staging, b"abandoned staging").expect("create staging");
        let unknown = runtime_root.join("user-owned-note.txt");
        fs::write(&unknown, b"preserve").expect("create unknown entry");
        let sibling_root = scan_root.join("unleased-sibling");
        fs::create_dir(&sibling_root).expect("create sibling root");
        let sibling_staging = sibling_root.join(".recovery-open-sibling.staging.mdp");
        fs::write(&sibling_staging, b"preserve sibling").expect("create sibling staging");
        let snapshot_named_directory = runtime_root.join("autosave").join("directory.autosave.mdp");
        fs::create_dir(&snapshot_named_directory).expect("create snapshot-named directory");

        let cleanup =
            cleanup_recovery_runtime_artifacts(&runtime_lease).expect("clean exact leased root");
        assert_eq!(cleanup.removed_staging_count, 1);
        assert_eq!(cleanup.removed_unreferenced_snapshot_count, 1);
        assert!(
            autosave.is_file(),
            "manifest-referenced snapshot must survive"
        );
        assert!(unknown.is_file(), "unknown runtime entries must survive");
        assert!(
            sibling_staging.is_file(),
            "cleanup must never enumerate or mutate a sibling runtime root"
        );
        assert!(
            snapshot_named_directory.is_dir(),
            "cleanup must not remove directories or follow link-like entries"
        );

        let preserved_without_manifest =
            runtime_root.join("autosave").join("manifestless.autosave.mdp");
        fs::copy(&autosave, &preserved_without_manifest).expect("create manifestless snapshot");
        fs::remove_file(recovery_manifest_path(runtime_root)).expect("remove manifest");
        let cleanup = cleanup_recovery_runtime_artifacts(&runtime_lease)
            .expect("missing manifest is a conservative cleanup no-op");
        assert_eq!(cleanup.removed_unreferenced_snapshot_count, 0);
        assert!(
            preserved_without_manifest.is_file(),
            "missing manifest must preserve every autosave archive"
        );

        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn recovery_manifest_rejects_non_absolute_project_authority() {
        let relative = Path::new("relative/project.mdp");
        let error = RecoveryManifest::new(ProjectId::new(), relative)
            .expect_err("relative Project authority must not be created");
        assert!(error.contains("not absolute"));
    }

    #[test]
    fn lease_admission_rejects_an_entry_retired_after_preflight() {
        let scan_root = unique_root("selection-retired-after-preflight");
        let project_file = scan_root.join("project.mdp");
        let (document, runtime_lease, autosave) = fixture(&scan_root, &project_file);
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &autosave,
            unix_now_ms(),
        ))
        .expect("publish recovery point");
        let candidate = candidate_for(&runtime_lease, &autosave);
        let evidence = preflight_recovery_selection(&candidate).expect("preflight evidence");

        let manifest_path = recovery_manifest_path(runtime_lease.runtime_root());
        let mut manifest = read_manifest(&manifest_path).expect("manifest");
        manifest.snapshots.clear();
        publish_manifest(&manifest_path, &manifest).expect("retire selected entry");
        assert!(
            autosave.is_file(),
            "model cleanup failure retaining retired archive"
        );

        let staged = runtime_lease.runtime_root().join(".retired-selection-copy.staging.mdp");
        let error = copy_recovery_selection_under_lease(&evidence, &runtime_lease, &staged)
            .expect_err("retired evidence must not be copied after lease admission");
        assert!(error.contains("retired before lease admission"));
        assert!(!staged.exists());
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn lease_admission_rejects_source_bytes_changed_after_preflight() {
        let scan_root = unique_root("selection-source-changed-after-preflight");
        let project_file = scan_root.join("project.mdp");
        let (document, runtime_lease, autosave) = fixture(&scan_root, &project_file);
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &autosave,
            unix_now_ms(),
        ))
        .expect("publish recovery point");
        let candidate = candidate_for(&runtime_lease, &autosave);
        let evidence = preflight_recovery_selection(&candidate).expect("preflight evidence");
        fs::OpenOptions::new()
            .append(true)
            .open(&autosave)
            .expect("open selected source")
            .write_all(b"changed after selection")
            .expect("change selected source");

        let staged = runtime_lease.runtime_root().join(".changed-selection-copy.staging.mdp");
        let error = copy_recovery_selection_under_lease(&evidence, &runtime_lease, &staged)
            .expect_err("changed source must not be copied after lease admission");
        assert!(error.contains("content hash does not match"));
        assert!(!staged.exists());
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn lease_admission_rejects_ambiguous_duplicate_manifest_entries() {
        let scan_root = unique_root("selection-duplicate-after-preflight");
        let project_file = scan_root.join("project.mdp");
        let (document, runtime_lease, autosave) = fixture(&scan_root, &project_file);
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &autosave,
            unix_now_ms(),
        ))
        .expect("publish recovery point");
        let candidate = candidate_for(&runtime_lease, &autosave);
        let evidence = preflight_recovery_selection(&candidate).expect("preflight evidence");

        let manifest_path = recovery_manifest_path(runtime_lease.runtime_root());
        let mut manifest = read_manifest(&manifest_path).expect("manifest");
        let duplicate = manifest.snapshots[0].clone();
        manifest.snapshots.push(duplicate);
        let bytes = serde_json::to_vec_pretty(&manifest).expect("encode ambiguous manifest");
        write_durable_file_atomically(&manifest_path, &bytes)
            .expect("publish intentionally ambiguous manifest");

        let staged = runtime_lease.runtime_root().join(".ambiguous-selection-copy.staging.mdp");
        let error = copy_recovery_selection_under_lease(&evidence, &runtime_lease, &staged)
            .expect_err("duplicate path entries must not select an arbitrary authority");
        assert!(error.contains("duplicate snapshot paths"));
        assert!(!staged.exists());
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn lease_admission_copies_and_reverifies_the_exact_selected_bytes() {
        let scan_root = unique_root("selection-exact-copy");
        let project_file = scan_root.join("project.mdp");
        let (document, runtime_lease, autosave) = fixture(&scan_root, &project_file);
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &autosave,
            unix_now_ms(),
        ))
        .expect("publish recovery point");
        let candidate = candidate_for(&runtime_lease, &autosave);
        let evidence = preflight_recovery_selection(&candidate).expect("preflight evidence");
        let staged = runtime_lease.runtime_root().join(".verified-selection-copy.staging.mdp");

        let mut verified = copy_recovery_selection_under_lease(&evidence, &runtime_lease, &staged)
            .expect("lease-bound exact copy");
        assert_eq!(
            sha256_open_file(verified.file_mut().expect("verified file")).expect("staged hash"),
            evidence.exact_entry.archive_sha256
        );
        let staged_document =
            read_project_document_from_open_archive(verified.file_mut().expect("verified file"))
                .expect("staged recovery archive");
        assert_eq!(staged_document.project_id, evidence.project_id);
        assert_eq!(
            staged_document.document_revision,
            evidence.exact_entry.document_revision
        );
        drop(verified);
        assert!(!staged.exists(), "RAII must remove the staging archive");
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[cfg(unix)]
    #[test]
    fn verified_recovery_handle_survives_staging_namespace_replacement() {
        let scan_root = unique_root("verified-handle-unix");
        let project_file = scan_root.join("project.mdp");
        let (document, runtime_lease, autosave) = fixture(&scan_root, &project_file);
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &autosave,
            unix_now_ms(),
        ))
        .expect("publish recovery point");
        let candidate = candidate_for(&runtime_lease, &autosave);
        let evidence = preflight_recovery_selection(&candidate).expect("preflight evidence");
        let staged = runtime_lease.runtime_root().join(".verified-handle-unix.staging.mdp");
        let mut verified = copy_recovery_selection_under_lease(&evidence, &runtime_lease, &staged)
            .expect("lease-bound exact copy");

        fs::remove_file(&staged).expect("unlink staging namespace entry");
        fs::write(&staged, b"replacement pathname bytes").expect("replace staging pathname");
        let loaded =
            read_project_document_from_open_archive(verified.file_mut().expect("verified file"))
                .expect("retained handle still names the verified object");
        assert_eq!(loaded.project_id, evidence.project_id);
        assert_eq!(
            loaded.document_revision,
            evidence.exact_entry.document_revision
        );
        drop(verified);
        assert!(
            staged.exists(),
            "RAII must not delete a replacement namespace object"
        );

        let _ = fs::remove_file(&staged);
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[cfg(windows)]
    #[test]
    fn verified_recovery_handle_denies_staging_namespace_replacement() {
        let scan_root = unique_root("verified-handle-windows");
        let project_file = scan_root.join("project.mdp");
        let (document, runtime_lease, autosave) = fixture(&scan_root, &project_file);
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &autosave,
            unix_now_ms(),
        ))
        .expect("publish recovery point");
        let candidate = candidate_for(&runtime_lease, &autosave);
        let evidence = preflight_recovery_selection(&candidate).expect("preflight evidence");
        let staged = runtime_lease.runtime_root().join(".verified-handle-windows.staging.mdp");
        let mut verified = copy_recovery_selection_under_lease(&evidence, &runtime_lease, &staged)
            .expect("lease-bound exact copy");

        fs::remove_file(&staged)
            .expect_err("the verified no-share handle must deny path replacement");
        let loaded =
            read_project_document_from_open_archive(verified.file_mut().expect("verified file"))
                .expect("retained handle remains readable");
        assert_eq!(loaded.project_id, evidence.project_id);

        drop(verified);
        assert!(!staged.exists(), "RAII must remove the staging archive");
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn discovery_and_selection_reject_tampered_archive() {
        let scan_root = unique_root("tamper");
        let project_file = scan_root.join("project.mdp");
        let (document, runtime_lease, autosave) = fixture(&scan_root, &project_file);
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &autosave,
            unix_now_ms(),
        ))
        .expect("publish recovery point");
        let candidate = candidate_for(&runtime_lease, &autosave);
        fs::OpenOptions::new()
            .append(true)
            .open(&autosave)
            .expect("open autosave")
            .write_all(b"tampered")
            .expect("tamper autosave");
        assert!(discover_crash_recovery_candidates_under(&scan_root).is_empty());
        assert!(
            preflight_recovery_selection(&candidate).is_err(),
            "tampered recovery selection must fail"
        );
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[cfg(unix)]
    #[test]
    fn discovery_and_selection_reject_a_symlinked_snapshot_leaf() {
        use std::os::unix::fs::symlink;

        let scan_root = unique_root("symlinked-snapshot");
        let project_file = scan_root.join("project.mdp");
        let (document, runtime_lease, autosave) = fixture(&scan_root, &project_file);
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &autosave,
            unix_now_ms(),
        ))
        .expect("publish recovery point");
        let candidate = candidate_for(&runtime_lease, &autosave);

        let backing = runtime_lease.runtime_root().join("autosave").join("snapshot-backing.mdp");
        fs::rename(&autosave, &backing).expect("retain exact snapshot bytes under another entry");
        symlink(&backing, &autosave).expect("replace selected entry with a symlink");

        assert!(
            discover_crash_recovery_candidates_under(&scan_root).is_empty(),
            "recovery discovery must not follow a snapshot symlink"
        );
        let error = preflight_recovery_selection(&candidate)
            .expect_err("selection must reject a symlinked snapshot leaf");
        assert!(
            error.contains("direct recovery archive")
                || error.contains("Too many levels of symbolic links"),
            "unexpected symlink rejection: {error}"
        );

        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn discovery_and_selection_reject_canonical_project_identity_mismatch() {
        let scan_root = unique_root("cross-project");
        let project_file = scan_root.join("project.mdp");
        let (document, runtime_lease, autosave) = fixture(&scan_root, &project_file);
        let runtime_root = runtime_lease.runtime_root();
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &autosave,
            unix_now_ms(),
        ))
        .expect("publish recovery point");
        let candidate = candidate_for(&runtime_lease, &autosave);

        let sequence = Sequence::with_settings("Other Project", SequenceSettings::default())
            .expect("sequence");
        let other = ProjectDocument::new(
            "Other Project",
            SequenceCollection::new(sequence),
            ProjectColorEnvironment::default(),
            SequenceSettings::default(),
            ProjectSettings::default(),
        );
        let library = AssetLibrary::open(runtime_root.join("library")).expect("open library");
        save_project_archive(&other, &library.database_path(), &project_file)
            .expect("save canonical other Project");

        assert!(discover_crash_recovery_candidates_under(&scan_root).is_empty());
        assert!(
            preflight_recovery_selection(&candidate).is_err(),
            "cross-Project recovery selection must fail"
        );
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn queued_autosave_cannot_reverse_a_manual_path_rebind() {
        let scan_root = unique_root("queued-after-save-as");
        let previous_project_file = scan_root.join("previous.mdp");
        let current_project_file = scan_root.join("current.mdp");
        let (mut document, runtime_lease, first) = fixture(&scan_root, &previous_project_file);
        let runtime_root = runtime_lease.runtime_root();
        publish_recovery_point(publication(
            &document,
            &previous_project_file,
            &runtime_lease,
            &first,
            unix_now_ms().saturating_sub(1),
        ))
        .expect("publish first");
        reconcile_recovery_after_manual_save(&runtime_lease, &current_project_file, false)
            .expect("rebind path");

        document.document_revision += 1;
        let queued = runtime_root.join("autosave").join("queued.autosave.mdp");
        let library = AssetLibrary::open(runtime_root.join("library")).expect("open library");
        save_project_archive(&document, &library.database_path(), &queued).expect("save queued");
        publish_recovery_point(publication(
            &document,
            &previous_project_file,
            &runtime_lease,
            &queued,
            unix_now_ms(),
        ))
        .expect("publish queued autosave");

        let manifest = read_manifest(&recovery_manifest_path(runtime_root)).expect("manifest");
        assert_eq!(manifest.project_file, current_project_file);
        assert_eq!(manifest.snapshots.len(), 2);
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn retention_publishes_new_manifest_before_deleting_old_archive() {
        let scan_root = unique_root("retention");
        let project_file = scan_root.join("project.mdp");
        let (mut document, runtime_lease, first) = fixture(&scan_root, &project_file);
        let runtime_root = runtime_lease.runtime_root();
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &first,
            unix_now_ms().saturating_sub(1),
        ))
        .expect("publish first");

        document.document_revision += 1;
        let second = runtime_root.join("autosave").join("second.autosave.mdp");
        let library = AssetLibrary::open(runtime_root.join("library")).expect("open library");
        save_project_archive(&document, &library.database_path(), &second).expect("save second");
        let mut second_publication = publication(
            &document,
            &project_file,
            &runtime_lease,
            &second,
            unix_now_ms(),
        );
        second_publication.max_recovery_points = 1;
        publish_recovery_point(second_publication).expect("publish second");

        let manifest = read_manifest(&recovery_manifest_path(runtime_root)).expect("manifest");
        assert_eq!(manifest.snapshots.len(), 1);
        assert_eq!(manifest.snapshots[0].file, second);
        assert!(second.exists());
        assert!(!first.exists());
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn retention_protects_new_publication_from_future_dated_history() {
        let scan_root = unique_root("retention-protects-new");
        let project_file = scan_root.join("project.mdp");
        let (mut document, runtime_lease, future_dated) = fixture(&scan_root, &project_file);
        let runtime_root = runtime_lease.runtime_root();
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &future_dated,
            u64::MAX,
        ))
        .expect("publish future-dated history");

        document.document_revision += 1;
        let newly_published = runtime_root.join("autosave").join("newly-published.autosave.mdp");
        let library = AssetLibrary::open(runtime_root.join("library")).expect("open library");
        save_project_archive(&document, &library.database_path(), &newly_published)
            .expect("save newly published archive");
        let mut new_publication = publication(
            &document,
            &project_file,
            &runtime_lease,
            &newly_published,
            0,
        );
        new_publication.max_recovery_points = 1;
        new_publication.retention_days = 1;
        publish_recovery_point(new_publication).expect("publish protected recovery point");

        let manifest = read_manifest(&recovery_manifest_path(runtime_root)).expect("manifest");
        assert_eq!(manifest.snapshots.len(), 1);
        assert_eq!(manifest.snapshots[0].file, newly_published);
        assert_eq!(manifest.snapshots[0].saved_at_unix_ms, 0);
        assert!(newly_published.exists());
        assert!(
            !future_dated.exists(),
            "future-dated history must not displace the current publication"
        );
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn protected_publication_becomes_expirable_history_on_next_publication() {
        let scan_root = unique_root("retention-protection-is-transactional");
        let project_file = scan_root.join("project.mdp");
        let (mut document, runtime_lease, previously_protected) =
            fixture(&scan_root, &project_file);
        let runtime_root = runtime_lease.runtime_root();
        let mut first_publication = publication(
            &document,
            &project_file,
            &runtime_lease,
            &previously_protected,
            0,
        );
        first_publication.max_recovery_points = 1;
        first_publication.retention_days = 1;
        publish_recovery_point(first_publication).expect("publish initially protected point");

        let first_manifest =
            read_manifest(&recovery_manifest_path(runtime_root)).expect("first manifest");
        assert_eq!(first_manifest.snapshots.len(), 1);
        assert_eq!(first_manifest.snapshots[0].file, previously_protected);

        document.document_revision += 1;
        let current = runtime_root.join("autosave").join("current.autosave.mdp");
        let library = AssetLibrary::open(runtime_root.join("library")).expect("open library");
        save_project_archive(&document, &library.database_path(), &current)
            .expect("save current archive");
        let mut current_publication = publication(
            &document,
            &project_file,
            &runtime_lease,
            &current,
            unix_now_ms(),
        );
        current_publication.max_recovery_points = 2;
        current_publication.retention_days = 1;
        publish_recovery_point(current_publication).expect("publish next recovery point");

        let manifest = read_manifest(&recovery_manifest_path(runtime_root)).expect("manifest");
        assert_eq!(manifest.snapshots.len(), 1);
        assert_eq!(manifest.snapshots[0].file, current);
        assert!(current.exists());
        assert!(
            !previously_protected.exists(),
            "a prior transaction's protected point must become ordinary expirable history"
        );
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn owner_mismatch_fails_before_recovery_retirement_deletes_any_archive() {
        let scan_root = unique_root("owner-mismatch-retirement");
        let project_file = scan_root.join("project.mdp");
        let (document, runtime_lease, autosave) = fixture(&scan_root, &project_file);
        let runtime_root = runtime_lease.runtime_root();
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_lease,
            &autosave,
            unix_now_ms(),
        ))
        .expect("publish recovery point");

        let owner_path = runtime_root.join("owner.manifest.json");
        let mut owner: serde_json::Value =
            serde_json::from_slice(&fs::read(&owner_path).expect("read owner"))
                .expect("parse owner");
        owner["project_id"] = serde_json::Value::String(ProjectId::new().to_string());
        fs::write(
            &owner_path,
            serde_json::to_vec_pretty(&owner).expect("encode owner"),
        )
        .expect("tamper owner identity");

        let error = reconcile_recovery_after_manual_save(&runtime_lease, &project_file, true)
            .expect_err("mismatched owner must fail closed");
        assert!(matches!(
            &error,
            RecoveryManifestPublicationFailure::BeforeNamespace(_)
        ));
        assert!(error.to_string().contains("another Project"));
        assert!(
            autosave.is_file(),
            "owner mismatch must not delete autosave"
        );
        let manifest = read_manifest(&recovery_manifest_path(runtime_root))
            .expect("manifest remains readable");
        assert_eq!(manifest.snapshots.len(), 1);
        drop(runtime_lease);
        let _ = fs::remove_dir_all(scan_root);
    }
}
