//! UI-independent durable Project persistence service.
//!
//! The service consumes immutable authoring snapshots, creates one consistent
//! SQLite backup, and atomically publishes an `.mdp` archive on a dedicated
//! worker. Window and command Adapters only submit intent and poll completions.

use super::project_recovery::{publish_recovery_point, RecoveryPointPublication};
use mondrian_core::ProjectMeta;
use mondrian_editor_state::{AuthorGeneration, AuthoringSessionId, AuthoringSnapshot};
use mondrian_project::save_project_archive;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

const PERSISTENCE_QUEUE_CAPACITY: usize = 4;
const MAX_COMPLETIONS_PER_POLL: usize = 8;

/// Stable identity of one persistence request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProjectPersistenceRequestId(u64);

impl ProjectPersistenceRequestId {
    /// Numeric request identity for diagnostics.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Why one archive is being published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectPersistencePurpose {
    /// User-requested durable save. `update_project_path` implements Save As.
    Manual { update_project_path: bool },
    /// Recovery point that never makes the authoring session clean.
    Autosave {
        original_project_file: PathBuf,
        runtime_root: PathBuf,
        max_recovery_points: usize,
        retention_days: u32,
        saved_at_unix_ms: u64,
    },
}

/// Metadata durably published by one successful request.
#[derive(Debug, Clone)]
pub struct PersistedProjectState {
    /// Save-only document revision embedded in the archive.
    pub document_revision: u64,
    /// Exact SQLite revision embedded in the archive.
    pub asset_library_revision: u64,
    /// Persistence metadata embedded in the archive.
    pub meta: ProjectMeta,
}

/// Terminal persistence result delivered to the App composition root.
#[derive(Debug, Clone)]
pub struct ProjectPersistenceCompletion {
    /// Request identity.
    pub request_id: ProjectPersistenceRequestId,
    /// Exact open authoring lifetime that submitted the request.
    pub session_id: AuthoringSessionId,
    /// Exact author generation represented by the archive.
    pub generation: AuthorGeneration,
    /// Published target path.
    pub target_file: PathBuf,
    /// Original request purpose.
    pub purpose: ProjectPersistencePurpose,
    /// Durable result. Error strings retain the full worker-side cause chain.
    pub result: Result<PersistedProjectState, String>,
}

struct ProjectPersistenceRequest {
    id: ProjectPersistenceRequestId,
    snapshot: AuthoringSnapshot,
    target_file: PathBuf,
    purpose: ProjectPersistencePurpose,
}

/// Bounded, single-writer durable persistence Module.
pub struct ProjectPersistenceService {
    request_tx: SyncSender<ProjectPersistenceRequest>,
    completion_rx: Receiver<ProjectPersistenceCompletion>,
    pending: Arc<AtomicUsize>,
    next_request_id: u64,
    startup_error: Option<String>,
}

impl ProjectPersistenceService {
    /// Start the lazy-independent persistence worker.
    pub fn new() -> Self {
        let (request_tx, request_rx) = mpsc::sync_channel(PERSISTENCE_QUEUE_CAPACITY);
        let (completion_tx, completion_rx) = mpsc::channel();
        let pending = Arc::new(AtomicUsize::new(0));
        let worker_pending = Arc::clone(&pending);
        let startup_error = std::thread::Builder::new()
            .name("mondrian-project-persistence".to_owned())
            .spawn(move || persistence_worker(request_rx, completion_tx, worker_pending))
            .err()
            .map(|error| format!("project persistence worker failed to start: {error}"));
        Self {
            request_tx,
            completion_rx,
            pending,
            next_request_id: 1,
            startup_error,
        }
    }

    /// Submit an immutable save request without waiting for filesystem I/O.
    pub fn submit(
        &mut self,
        snapshot: AuthoringSnapshot,
        target_file: PathBuf,
        purpose: ProjectPersistencePurpose,
    ) -> Result<ProjectPersistenceRequestId, String> {
        if let Some(error) = &self.startup_error {
            return Err(error.clone());
        }
        if target_file.as_os_str().is_empty() {
            return Err("project persistence target is empty".to_owned());
        }
        let id = ProjectPersistenceRequestId(self.next_request_id);
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| "project persistence request identity exhausted".to_owned())?;
        let request = ProjectPersistenceRequest { id, snapshot, target_file, purpose };
        // Publish admission before the worker can observe the request. Doing
        // this after `try_send` permits a fast worker to decrement zero.
        self.pending.fetch_add(1, Ordering::AcqRel);
        match self.request_tx.try_send(request) {
            Ok(()) => Ok(id),
            Err(TrySendError::Full(_)) => {
                self.pending.fetch_sub(1, Ordering::AcqRel);
                Err("project persistence queue is full".to_owned())
            }
            Err(TrySendError::Disconnected(_)) => {
                self.pending.fetch_sub(1, Ordering::AcqRel);
                Err("project persistence worker is unavailable".to_owned())
            }
        }
    }

    /// Drain a bounded number of terminal completions.
    pub fn poll_completions(&self) -> Vec<ProjectPersistenceCompletion> {
        self.completion_rx.try_iter().take(MAX_COMPLETIONS_PER_POLL).collect()
    }

    /// Number of admitted requests not yet completed.
    #[cfg(test)]
    pub fn pending_requests(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }
}

impl Default for ProjectPersistenceService {
    fn default() -> Self {
        Self::new()
    }
}

fn persistence_worker(
    request_rx: Receiver<ProjectPersistenceRequest>,
    completion_tx: mpsc::Sender<ProjectPersistenceCompletion>,
    pending: Arc<AtomicUsize>,
) {
    while let Ok(request) = request_rx.recv() {
        let completion = execute_persistence_request(request);
        pending.fetch_sub(1, Ordering::AcqRel);
        if completion_tx.send(completion).is_err() {
            break;
        }
    }
}

fn execute_persistence_request(request: ProjectPersistenceRequest) -> ProjectPersistenceCompletion {
    let session_id = request.snapshot.session_id;
    let generation = request.snapshot.generation;
    let asset_library_revision = request.snapshot.asset_library_revision;
    let database_snapshot = database_snapshot_path(&request.target_file, request.id);
    let result = (|| {
        let mut document = request.snapshot.document;
        document.document_revision = document.document_revision.saturating_add(1).max(1);
        document.meta.touch();
        request
            .snapshot
            .asset_library
            .snapshot_database(request.snapshot.asset_library_revision, &database_snapshot)
            .map_err(|error| error.to_string())?;
        save_project_archive(&document, &database_snapshot, &request.target_file)
            .map_err(|error| format!("{error:#}"))?;
        if let ProjectPersistencePurpose::Autosave {
            original_project_file,
            runtime_root,
            max_recovery_points,
            retention_days,
            saved_at_unix_ms,
        } = &request.purpose
        {
            publish_recovery_point(RecoveryPointPublication {
                project_id: document.project_id,
                project_file: original_project_file,
                runtime_root,
                autosave_file: &request.target_file,
                author_generation: generation.get(),
                asset_library_revision,
                document_revision: document.document_revision,
                saved_at_unix_ms: *saved_at_unix_ms,
                max_recovery_points: *max_recovery_points,
                retention_days: *retention_days,
            })?;
        }
        Ok(PersistedProjectState {
            document_revision: document.document_revision,
            asset_library_revision,
            meta: document.meta,
        })
    })();
    if let Err(error) = std::fs::remove_file(&database_snapshot) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                path = %database_snapshot.display(),
                %error,
                "failed to remove persistence SQLite snapshot"
            );
        }
    }
    ProjectPersistenceCompletion {
        request_id: request.id,
        session_id,
        generation,
        target_file: request.target_file,
        purpose: request.purpose,
        result,
    }
}

fn database_snapshot_path(target_file: &Path, request_id: ProjectPersistenceRequestId) -> PathBuf {
    let parent = target_file.parent().unwrap_or_else(|| Path::new("."));
    let stem = target_file.file_name().and_then(|name| name.to_str()).unwrap_or("project.mdp");
    parent.join(format!(
        ".{stem}.{}.{}.library-snapshot.db",
        std::process::id(),
        request_id.get()
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_assets::AssetLibrary;
    use mondrian_core::ProjectSettings;
    use mondrian_editor_state::AuthoringSession;
    use mondrian_project::{load_project_archive, ProjectDocument};
    use mondrian_timeline::{Sequence, SequenceCollection, SequenceSettings};
    use std::time::{Duration, Instant};

    fn unique_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mondrian-persistence-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ))
    }

    fn session(root: &Path) -> AuthoringSession {
        let library = AssetLibrary::open(root.join("library")).expect("asset library");
        let document = ProjectDocument::new(
            "Persistence Test",
            SequenceCollection::new(Sequence::new("Sequence")),
            mondrian_core::ProjectColorEnvironment::default(),
            SequenceSettings::default(),
            ProjectSettings::default(),
        );
        AuthoringSession::new_unsaved(
            document,
            root.join("project.mdp"),
            root.join("runtime"),
            library,
        )
        .expect("authoring session")
    }

    fn wait_for(
        service: &ProjectPersistenceService,
        request_id: ProjectPersistenceRequestId,
    ) -> ProjectPersistenceCompletion {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(completion) = service
                .poll_completions()
                .into_iter()
                .find(|completion| completion.request_id == request_id)
            {
                return completion;
            }
            assert!(Instant::now() < deadline, "persistence request timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn background_save_embeds_the_exact_sqlite_snapshot() {
        let root = unique_root("sqlite-snapshot");
        let authoring = session(&root);
        let asset_id = authoring
            .asset_library()
            .create_solid_color_asset(Some("Snapshot Asset"))
            .expect("create asset");
        let snapshot = authoring.snapshot().expect("authoring snapshot");
        let target = root.join("saved.mdp");
        let mut service = ProjectPersistenceService::new();
        let request_id = service
            .submit(
                snapshot.clone(),
                target.clone(),
                ProjectPersistencePurpose::Manual { update_project_path: false },
            )
            .expect("submit save");

        let completion = wait_for(&service, request_id);
        let persisted = completion.result.expect("save completion");
        assert_eq!(
            persisted.asset_library_revision,
            snapshot.asset_library_revision
        );
        assert_eq!(service.pending_requests(), 0);

        let extracted = root.join("extracted-library");
        let loaded = load_project_archive(&target, &extracted).expect("load saved archive");
        assert_eq!(
            loaded.document.document_revision,
            persisted.document_revision
        );
        let reopened = AssetLibrary::open(extracted).expect("open embedded library");
        assert_eq!(
            reopened
                .get_asset(asset_id)
                .expect("query embedded asset")
                .expect("embedded asset")
                .name,
            "Snapshot Asset"
        );
    }

    #[test]
    fn autosave_archive_precedes_repeatable_manifest_publication() {
        let root = unique_root("autosave-manifest");
        let authoring = session(&root);
        let original = root.join("project.mdp");
        let runtime_root = root.join("runtime");
        let mut service = ProjectPersistenceService::new();
        let saved_at_base = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_millis() as u64;

        for index in 1..=2u64 {
            let autosave =
                runtime_root.join("autosave").join(format!("project-{index}.autosave.mdp"));
            let request_id = service
                .submit(
                    authoring.snapshot().expect("authoring snapshot"),
                    autosave.clone(),
                    ProjectPersistencePurpose::Autosave {
                        original_project_file: original.clone(),
                        runtime_root: runtime_root.clone(),
                        max_recovery_points: 2,
                        retention_days: 7,
                        saved_at_unix_ms: saved_at_base + index,
                    },
                )
                .expect("submit autosave");
            wait_for(&service, request_id).result.expect("autosave completion");
            assert!(
                autosave.is_file(),
                "manifest must never name an unpublished archive"
            );
        }

        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(runtime_root.join("autosave").join("manifest.json"))
                .expect("autosave manifest"),
        )
        .expect("valid autosave manifest");
        assert_eq!(manifest["schema_version"], 1);
        assert_eq!(
            manifest["project_file"],
            original.to_string_lossy().as_ref()
        );
        let snapshots = manifest["snapshots"].as_array().expect("snapshot array");
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0]["saved_at_unix_ms"], saved_at_base + 2);
        assert_eq!(snapshots[0]["author_generation"], 1);
        assert_eq!(snapshots[0]["asset_library_revision"], 0);
        assert_eq!(
            snapshots[0]["archive_sha256"].as_str().map(str::len),
            Some(64)
        );
    }
}
