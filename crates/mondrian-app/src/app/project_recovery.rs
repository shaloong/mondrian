//! Durable Project recovery-point index and lifecycle.
//!
//! This Module owns the complete recovery manifest contract: publication,
//! identity/hash validation, retention, discovery, selection admission, and
//! retirement after a current manual Project save. Callers never interpret or
//! mutate manifest JSON themselves.

use mondrian_core::ProjectId;
use mondrian_project::{read_project_document_from_archive, write_durable_file_atomically};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const RECOVERY_MANIFEST_SCHEMA_VERSION: u32 = 1;
const RECOVERY_MANIFEST_FILE: &str = "manifest.json";
static RECOVERY_MUTATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// One verified startup recovery choice.
#[derive(Debug, Clone)]
pub(crate) struct CrashRecoveryCandidate {
    pub(crate) project_file: PathBuf,
    pub(crate) autosave_file: PathBuf,
    pub(crate) saved_at_unix_ms: u64,
    pub(crate) total_snapshots: usize,
}

/// Complete immutable facts required to publish one recovery point.
pub(super) struct RecoveryPointPublication<'a> {
    pub project_id: ProjectId,
    pub project_file: &'a Path,
    pub runtime_root: &'a Path,
    pub autosave_file: &'a Path,
    pub author_generation: u64,
    pub asset_library_revision: u64,
    pub document_revision: u64,
    pub saved_at_unix_ms: u64,
    pub max_recovery_points: usize,
    pub retention_days: u32,
}

/// Result of reconciling recovery authority after one manual save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RecoveryReconciliation {
    pub removed_snapshot_count: usize,
}

/// Admission evidence for one exact recovery selection.
pub(super) struct ValidatedRecoverySelection {
    /// Stable runtime authority containing the selected canonical manifest.
    pub runtime_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryManifest {
    schema_version: u32,
    project_id: ProjectId,
    project_file: PathBuf,
    snapshots: Vec<RecoverySnapshotEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    fn new(project_id: ProjectId, project_file: &Path) -> Self {
        Self {
            schema_version: RECOVERY_MANIFEST_SCHEMA_VERSION,
            project_id,
            project_file: project_file.to_path_buf(),
            snapshots: Vec::new(),
        }
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
        Ok(())
    }
}

pub(super) fn recovery_manifest_path(runtime_root: &Path) -> PathBuf {
    runtime_root.join("autosave").join(RECOVERY_MANIFEST_FILE)
}

/// Publish a newly written archive into the versioned recovery index.
///
/// The replacement manifest is durably committed before any entry it no longer
/// references is removed. A failed manifest publication therefore leaves the
/// previous manifest and all of its referenced recovery archives intact.
pub(super) fn publish_recovery_point(
    publication: RecoveryPointPublication<'_>,
) -> Result<(), String> {
    validate_positive_publication_values(&publication)?;
    let autosave_dir = recovery_autosave_dir(publication.runtime_root);
    validate_snapshot_path(&autosave_dir, publication.autosave_file)?;
    let new_entry = verified_entry_from_archive(
        publication.autosave_file,
        publication.project_id,
        publication.author_generation,
        publication.asset_library_revision,
        publication.document_revision,
        publication.saved_at_unix_ms,
    )?;
    let _mutation = recovery_mutation_guard();

    let manifest_path = recovery_manifest_path(publication.runtime_root);
    let mut manifest = if manifest_path.exists() {
        let manifest = read_manifest(&manifest_path)?;
        // ProjectId is the durable identity. Only manual-save reconciliation
        // may change the canonical path, so a queued autosave cannot flip a
        // completed Save As back to its previous path.
        manifest.validate_identity(publication.project_id)?;
        manifest
    } else {
        RecoveryManifest::new(publication.project_id, publication.project_file)
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
    manifest.snapshots.push(new_entry);
    let retention_removed = apply_retention(
        &mut manifest.snapshots,
        publication.max_recovery_points.max(1),
        publication.retention_days.max(1),
    );
    obsolete_files.extend(retention_removed);
    sort_and_deduplicate_entries(&mut manifest.snapshots);

    publish_manifest(&manifest_path, &manifest)?;
    remove_unreferenced_snapshot_files(&autosave_dir, obsolete_files, &manifest.snapshots);
    Ok(())
}

/// Admit one user-selected recovery point against the canonical manifest.
pub(super) fn validate_recovery_selection(
    project_file: &Path,
    autosave_file: &Path,
) -> Result<ValidatedRecoverySelection, String> {
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
    let manifest_path = recovery_manifest_path(runtime_root);
    let manifest = read_manifest(&manifest_path)?;
    if manifest.schema_version != RECOVERY_MANIFEST_SCHEMA_VERSION {
        return Err(format!(
            "unsupported recovery manifest schema v{}",
            manifest.schema_version
        ));
    }
    if manifest.project_file != project_file {
        return Err("selected recovery point belongs to another Project path".to_owned());
    }
    let autosave_dir = recovery_autosave_dir(runtime_root);
    let entry =
        manifest
            .snapshots
            .iter()
            .find(|entry| entry.file == autosave_file)
            .ok_or_else(|| {
                "selected recovery point is not present in the canonical manifest".to_owned()
            })?;
    verify_entry(entry, &autosave_dir, manifest.project_id)?;

    if project_file.exists() {
        let canonical = read_project_document_from_archive(project_file)
            .map_err(|error| format!("canonical Project archive is invalid: {error:#}"))?;
        if canonical.project_id != manifest.project_id {
            return Err("recovery manifest does not belong to the canonical Project".to_owned());
        }
    }
    Ok(ValidatedRecoverySelection { runtime_root: runtime_root.to_path_buf() })
}

/// Discover only recovery points whose manifest, path, hash, archive, and
/// Project identity all validate.
pub(crate) fn discover_crash_recovery_candidates() -> Vec<CrashRecoveryCandidate> {
    discover_crash_recovery_candidates_under(&std::env::temp_dir().join("mondrian-runtime"))
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
        if manifest.schema_version != RECOVERY_MANIFEST_SCHEMA_VERSION
            || manifest.project_file.as_os_str().is_empty()
            || !canonical_project_identity_matches(&manifest.project_file, manifest.project_id)
        {
            continue;
        }
        let autosave_dir = recovery_autosave_dir(&runtime_root);
        let verified = manifest
            .snapshots
            .iter()
            .filter(|snapshot| verify_entry(snapshot, &autosave_dir, manifest.project_id).is_ok())
            .collect::<Vec<_>>();
        let total_snapshots = verified.len();
        for snapshot in verified {
            candidates.push(CrashRecoveryCandidate {
                project_file: manifest.project_file.clone(),
                autosave_file: snapshot.file.clone(),
                saved_at_unix_ms: snapshot.saved_at_unix_ms,
                total_snapshots,
            });
        }
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.saved_at_unix_ms));
    candidates
}

fn canonical_project_identity_matches(project_file: &Path, project_id: ProjectId) -> bool {
    if !project_file.exists() {
        return true;
    }
    read_project_document_from_archive(project_file)
        .is_ok_and(|document| document.project_id == project_id)
}

/// Reconcile recovery authority after a successful manual Project publication.
///
/// A clean save publishes an empty manifest before deleting covered archives.
/// A stale Save As completion only rebinds retained authority to the new
/// canonical path, because newer author state must remain recoverable.
pub(super) fn reconcile_recovery_after_manual_save(
    runtime_root: &Path,
    project_id: ProjectId,
    current_project_file: &Path,
    retire_all: bool,
) -> Result<RecoveryReconciliation, String> {
    let _mutation = recovery_mutation_guard();
    let manifest_path = recovery_manifest_path(runtime_root);
    if !manifest_path.exists() {
        return Ok(RecoveryReconciliation { removed_snapshot_count: 0 });
    }
    let mut manifest = read_manifest(&manifest_path)?;
    manifest.validate_identity(project_id)?;
    let autosave_dir = recovery_autosave_dir(runtime_root);
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
    publication: &RecoveryPointPublication<'_>,
) -> Result<(), String> {
    if publication.project_file.as_os_str().is_empty() {
        return Err("recovery source Project path is empty".to_owned());
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

fn validate_snapshot_path(autosave_dir: &Path, snapshot: &Path) -> Result<(), String> {
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
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

fn publish_manifest(path: &Path, manifest: &RecoveryManifest) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|error| error.to_string())?;
    write_durable_file_atomically(path, &bytes).map_err(|error| error.to_string())
}

fn verified_entry_from_archive(
    file: &Path,
    project_id: ProjectId,
    author_generation: u64,
    asset_library_revision: u64,
    document_revision: u64,
    saved_at_unix_ms: u64,
) -> Result<RecoverySnapshotEntry, String> {
    let archive_sha256 = sha256_file(file)?;
    let document = read_project_document_from_archive(file)
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
    if entry.author_generation == 0 || entry.document_revision == 0 {
        return Err("recovery snapshot has an invalid zero revision".to_owned());
    }
    if entry.archive_sha256.len() != 64
        || !entry.archive_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("recovery snapshot has an invalid SHA-256 identity".to_owned());
    }
    let actual_sha256 = sha256_file(&entry.file)?;
    if actual_sha256 != entry.archive_sha256 {
        return Err("recovery snapshot content hash does not match".to_owned());
    }
    let document = read_project_document_from_archive(&entry.file)
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
            if safe_path && valid_identity && entry.file.is_file() {
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

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
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
    use super::*;
    use mondrian_assets::AssetLibrary;
    use mondrian_core::{ProjectColorEnvironment, ProjectSettings};
    use mondrian_project::{save_project_archive, ProjectDocument};
    use mondrian_timeline::{Sequence, SequenceCollection, SequenceSettings};
    use std::io::Write;

    fn fixture(root: &Path) -> (ProjectDocument, PathBuf) {
        fs::create_dir_all(root.join("library")).expect("create library root");
        let library = AssetLibrary::open(root.join("library")).expect("open library");
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
        let autosave = root.join("autosave").join("fixture.autosave.mdp");
        fs::create_dir_all(autosave.parent().expect("autosave parent")).expect("autosave root");
        save_project_archive(&document, &library.database_path(), &autosave)
            .expect("save recovery archive");
        (document, autosave)
    }

    fn publication<'a>(
        document: &ProjectDocument,
        project_file: &'a Path,
        runtime_root: &'a Path,
        autosave_file: &'a Path,
        saved_at_unix_ms: u64,
    ) -> RecoveryPointPublication<'a> {
        RecoveryPointPublication {
            project_id: document.project_id,
            project_file,
            runtime_root,
            autosave_file,
            author_generation: 2,
            asset_library_revision: 0,
            document_revision: document.document_revision,
            saved_at_unix_ms,
            max_recovery_points: 10,
            retention_days: 365,
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
    fn publication_discovery_and_retirement_validate_exact_identity() {
        let scan_root = unique_root("lifecycle");
        let runtime_root = scan_root.join("runtime");
        let project_file = scan_root.join("project.mdp");
        let (document, autosave) = fixture(&runtime_root);
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_root,
            &autosave,
            unix_now_ms(),
        ))
        .expect("publish recovery point");

        let candidates = discover_crash_recovery_candidates_under(&scan_root);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].project_file, project_file);
        assert_eq!(candidates[0].autosave_file, autosave);
        let selection =
            validate_recovery_selection(&project_file, &autosave).expect("admit selection");
        assert_eq!(selection.runtime_root, runtime_root);

        let retirement = reconcile_recovery_after_manual_save(
            &runtime_root,
            document.project_id,
            &project_file,
            true,
        )
        .expect("retire recovery points");
        assert_eq!(retirement.removed_snapshot_count, 1);
        assert!(!autosave.exists());
        assert!(discover_crash_recovery_candidates_under(&scan_root).is_empty());
        let manifest = read_manifest(&recovery_manifest_path(&runtime_root)).expect("manifest");
        assert!(manifest.snapshots.is_empty());
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn discovery_and_selection_reject_tampered_archive() {
        let scan_root = unique_root("tamper");
        let runtime_root = scan_root.join("runtime");
        let project_file = scan_root.join("project.mdp");
        let (document, autosave) = fixture(&runtime_root);
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_root,
            &autosave,
            unix_now_ms(),
        ))
        .expect("publish recovery point");
        fs::OpenOptions::new()
            .append(true)
            .open(&autosave)
            .expect("open autosave")
            .write_all(b"tampered")
            .expect("tamper autosave");
        assert!(discover_crash_recovery_candidates_under(&scan_root).is_empty());
        assert!(
            validate_recovery_selection(&project_file, &autosave).is_err(),
            "tampered recovery selection must fail"
        );
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn discovery_and_selection_reject_canonical_project_identity_mismatch() {
        let scan_root = unique_root("cross-project");
        let runtime_root = scan_root.join("runtime");
        let project_file = scan_root.join("project.mdp");
        let (document, autosave) = fixture(&runtime_root);
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_root,
            &autosave,
            unix_now_ms(),
        ))
        .expect("publish recovery point");

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
            validate_recovery_selection(&project_file, &autosave).is_err(),
            "cross-Project recovery selection must fail"
        );
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn queued_autosave_cannot_reverse_a_manual_path_rebind() {
        let scan_root = unique_root("queued-after-save-as");
        let runtime_root = scan_root.join("runtime");
        let previous_project_file = scan_root.join("previous.mdp");
        let current_project_file = scan_root.join("current.mdp");
        let (mut document, first) = fixture(&runtime_root);
        publish_recovery_point(publication(
            &document,
            &previous_project_file,
            &runtime_root,
            &first,
            unix_now_ms().saturating_sub(1),
        ))
        .expect("publish first");
        reconcile_recovery_after_manual_save(
            &runtime_root,
            document.project_id,
            &current_project_file,
            false,
        )
        .expect("rebind path");

        document.document_revision += 1;
        let queued = runtime_root.join("autosave").join("queued.autosave.mdp");
        let library = AssetLibrary::open(runtime_root.join("library")).expect("open library");
        save_project_archive(&document, &library.database_path(), &queued).expect("save queued");
        publish_recovery_point(publication(
            &document,
            &previous_project_file,
            &runtime_root,
            &queued,
            unix_now_ms(),
        ))
        .expect("publish queued autosave");

        let manifest = read_manifest(&recovery_manifest_path(&runtime_root)).expect("manifest");
        assert_eq!(manifest.project_file, current_project_file);
        assert_eq!(manifest.snapshots.len(), 2);
        let _ = fs::remove_dir_all(scan_root);
    }

    #[test]
    fn retention_publishes_new_manifest_before_deleting_old_archive() {
        let scan_root = unique_root("retention");
        let runtime_root = scan_root.join("runtime");
        let project_file = scan_root.join("project.mdp");
        let (mut document, first) = fixture(&runtime_root);
        publish_recovery_point(publication(
            &document,
            &project_file,
            &runtime_root,
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
            &runtime_root,
            &second,
            unix_now_ms(),
        );
        second_publication.max_recovery_points = 1;
        publish_recovery_point(second_publication).expect("publish second");

        let manifest = read_manifest(&recovery_manifest_path(&runtime_root)).expect("manifest");
        assert_eq!(manifest.snapshots.len(), 1);
        assert_eq!(manifest.snapshots[0].file, second);
        assert!(second.exists());
        assert!(!first.exists());
        let _ = fs::remove_dir_all(scan_root);
    }
}
