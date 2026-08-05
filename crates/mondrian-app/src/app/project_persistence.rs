//! UI-independent durable Project persistence service.
//!
//! The service consumes immutable authoring snapshots, creates one consistent
//! SQLite backup, and atomically publishes an `.mdp` archive on a dedicated
//! worker. Window and command Adapters only submit intent and poll completions.

use super::project_recovery::{
    prepare_recovery_archive_target, publish_recovery_point, RecoveryManifestPublicationFailure,
    RecoveryPointPublication,
};
use super::project_runtime::{ProjectRuntimeLease, ProjectRuntimeLeaseId};
use mondrian_core::ProjectMeta;
use mondrian_editor_state::{AuthorGeneration, AuthoringSessionId, AuthoringSnapshot};
use mondrian_project::{
    save_project_archive_from_open_library_with_publication, ProjectArchivePublication,
    ProjectArchivePublicationFailure,
};
use mondrian_storage::{ensure_durable_directory_chain, DirectoryPublicationFailure};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) const PERSISTENCE_QUEUE_CAPACITY: usize = 4;
const MAX_COMPLETIONS_PER_POLL: usize = 8;
const PERSISTENCE_QUIESCENCE_TIMEOUT: Duration = Duration::from_secs(300);

static NEXT_PERSISTENCE_SERVICE_ID: AtomicU64 = AtomicU64::new(1);

/// Stable identity of one persistence request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProjectPersistenceRequestId(u64);

impl ProjectPersistenceRequestId {
    /// Numeric request identity for diagnostics.
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectPersistenceServiceId(u64);

fn next_persistence_service_id() -> Result<ProjectPersistenceServiceId, String> {
    NEXT_PERSISTENCE_SERVICE_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            next.checked_add(1)
        })
        .map(ProjectPersistenceServiceId)
        .map_err(|_| "Project persistence service identity exhausted".to_owned())
}

/// Monotonic submit-admission lifetime within one Authoring Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ProjectPersistenceGeneration(u64);

impl ProjectPersistenceGeneration {
    const INITIAL: Self = Self(1);

    fn next(self) -> Result<Self, String> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| "Project persistence admission generation exhausted".to_owned())
    }
}

/// Exact authority to resume or retire one quiesced persistence admission generation.
///
/// Tokens are service-, Session-, and generation-scoped. Reusing a token after
/// resume or presenting it to another service fails without changing admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProjectPersistencePauseToken {
    service_id: ProjectPersistenceServiceId,
    session_id: AuthoringSessionId,
    admission_generation: ProjectPersistenceGeneration,
}

/// Non-blocking FIFO quiescence request for one Authoring Session.
///
/// The ticket owns the sole acknowledgement receiver. While it exists, the
/// exact Session generation is `Pausing`: new persistence work is rejected,
/// but the UI thread may continue pumping events while the worker finishes all
/// earlier admitted requests. A caller must drive the ticket to a terminal
/// result; dropping it would intentionally leave admission closed.
pub(super) struct ProjectPersistencePauseTicket {
    token: ProjectPersistencePauseToken,
    acknowledgement_rx: Receiver<ProjectPersistenceBarrierAcknowledgement>,
    started_at: Instant,
    timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionAdmissionPhase {
    Open,
    Pausing,
    Paused,
    Retired,
    Poisoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionAdmission {
    generation: ProjectPersistenceGeneration,
    phase: SessionAdmissionPhase,
    last_reserved_manual_document_revision: Option<u64>,
}

impl SessionAdmission {
    const fn open() -> Self {
        Self {
            generation: ProjectPersistenceGeneration::INITIAL,
            phase: SessionAdmissionPhase::Open,
            last_reserved_manual_document_revision: None,
        }
    }
}

/// Session-scoped identity of the intended canonical manual-save destination.
///
/// Save As advances this binding only after queue admission. Later ordinary
/// Save requests reuse the admitted binding even while its first publication
/// is still in flight, so they cannot accidentally target the previous
/// canonical path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualProjectFileDestination {
    session_id: AuthoringSessionId,
    revision: u64,
    project_file: PathBuf,
    publication: ProjectArchivePublication,
}

impl ManualProjectFileDestination {
    /// Build the first destination binding for one open Authoring Session.
    pub(crate) fn initial(
        session_id: AuthoringSessionId,
        project_file: PathBuf,
    ) -> Result<Self, String> {
        let project_file = absolute_destination(project_file, "manual Project destination")?;
        Ok(Self {
            session_id,
            revision: 1,
            project_file,
            publication: ProjectArchivePublication::ReplaceExisting,
        })
    }

    /// Build the first destination binding for a newly created Project.
    ///
    /// The target must still be absent at the final atomic publication step;
    /// an earlier existence check is only an early diagnostic.
    pub(crate) fn initial_create(
        session_id: AuthoringSessionId,
        project_file: PathBuf,
    ) -> Result<Self, String> {
        let project_file = absolute_destination(project_file, "new Project destination")?;
        Ok(Self {
            session_id,
            revision: 1,
            project_file,
            publication: ProjectArchivePublication::CreateNew,
        })
    }

    /// Retarget this Session to a new intended canonical path.
    ///
    /// Repeating the same path retains the same binding identity, allowing
    /// every admitted snapshot for that destination to share one completion
    /// policy. A different path receives a checked successor revision.
    pub(crate) fn retarget(
        &self,
        project_file: PathBuf,
        publication: ProjectArchivePublication,
    ) -> Result<Self, String> {
        let project_file = absolute_destination(project_file, "manual Project destination")?;
        if self.project_file == project_file && self.publication == publication {
            return Ok(self.clone());
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| "manual Project destination identity exhausted".to_owned())?;
        Ok(Self {
            session_id: self.session_id,
            revision,
            project_file,
            publication,
        })
    }

    /// Open Authoring Session that owns this binding.
    pub const fn session_id(&self) -> AuthoringSessionId {
        self.session_id
    }

    /// Exact file that a matching completion may make canonical.
    pub fn project_file(&self) -> &Path {
        &self.project_file
    }

    fn publication(&self) -> ProjectArchivePublication {
        self.publication
    }

    fn lineage_key(&self) -> (AuthoringSessionId, u64) {
        (self.session_id, self.revision)
    }

    pub(super) fn replacement_binding(&self) -> Self {
        let mut binding = self.clone();
        binding.publication = ProjectArchivePublication::ReplaceExisting;
        binding
    }
}

/// Exact recovery-archive destination captured for one autosave snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutosaveArchiveDestination {
    archive_file: PathBuf,
    canonical_project_file: PathBuf,
}

impl AutosaveArchiveDestination {
    /// Bind an autosave to its immutable archive and canonical locator.
    pub(crate) fn new(
        archive_file: PathBuf,
        canonical_project_file: PathBuf,
    ) -> Result<Self, String> {
        let archive_file = absolute_destination(archive_file, "autosave archive destination")?;
        let canonical_project_file =
            absolute_destination(canonical_project_file, "autosave canonical Project path")?;
        Ok(Self { archive_file, canonical_project_file })
    }

    /// Exact recovery archive published by this request.
    pub fn archive_file(&self) -> &Path {
        &self.archive_file
    }

    /// Canonical Project locator captured with this recovery point.
    pub fn canonical_project_file(&self) -> &Path {
        &self.canonical_project_file
    }
}

/// Why and where one archive is being published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectPersistencePurpose {
    /// User-requested durable save to one explicit canonical destination.
    Manual {
        /// Destination identity admitted by the App persistence coordinator.
        destination: ManualProjectFileDestination,
    },
    /// Recovery point that never makes the authoring session clean.
    Autosave {
        /// Immutable recovery archive and captured canonical locator.
        destination: AutosaveArchiveDestination,
        /// Maximum number of recovery points retained for this Project.
        max_recovery_points: usize,
        /// Maximum recovery-point age in whole days.
        retention_days: u32,
        /// Wall-clock timestamp recorded as recovery metadata.
        saved_at_unix_ms: u64,
    },
}

impl ProjectPersistencePurpose {
    fn target_file(&self) -> &Path {
        match self {
            Self::Manual { destination } => destination.project_file(),
            Self::Autosave { destination, .. } => destination.archive_file(),
        }
    }
}

fn absolute_destination(path: PathBuf, description: &str) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err(format!("{description} is empty"));
    }
    std::path::absolute(path)
        .map_err(|error| format!("failed to make {description} absolute: {error}"))
}

fn ensure_durable_publication_parent(target_file: &Path) -> Result<(), String> {
    let parent = target_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "Project publication target has no parent directory".to_owned())?;
    match std::fs::symlink_metadata(parent) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(format!(
                    "Project publication parent is not a direct directory: {}",
                    parent.display()
                ));
            }
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "failed to inspect Project publication parent {}: {error}",
                parent.display()
            ));
        }
    }

    let anchor = nearest_existing_publication_ancestor(parent)?;
    let evidence = ensure_durable_directory_chain(&anchor, parent).map_err(|error| match error {
        DirectoryPublicationFailure::BeforeNamespace(error) => format!(
            "failed to establish Project publication parent before namespace commit: {error:#}"
        ),
        DirectoryPublicationFailure::DurabilityUnconfirmed { path, source } => format!(
            "Project publication parent is visible at {}, but crash durability is unconfirmed: {source}",
            path.display()
        ),
        DirectoryPublicationFailure::NamespaceIndeterminate {
            intended_path,
            retained_staging_path,
            source,
        } => {
            let retained = retained_staging_path
                .as_deref()
                .map(|path| format!("; verified staging remains at {}", path.display()))
                .unwrap_or_default();
            format!(
                "Project publication parent at {} has an indeterminate namespace postcondition{retained}: {source}",
                intended_path.display()
            )
        }
    })?;
    if evidence.path() != parent {
        return Err("Project publication parent evidence names a different directory".to_owned());
    }
    Ok(())
}

fn nearest_existing_publication_ancestor(path: &Path) -> Result<PathBuf, String> {
    let mut candidate = path.parent().ok_or_else(|| {
        "Project publication parent has no existing directory ancestor".to_owned()
    })?;
    loop {
        match std::fs::symlink_metadata(candidate) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(format!(
                        "Project publication ancestor is not a direct directory: {}",
                        candidate.display()
                    ));
                }
                return Ok(candidate.to_path_buf());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                candidate = candidate.parent().ok_or_else(|| {
                    "Project publication parent has no existing directory ancestor".to_owned()
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "failed to inspect Project publication ancestor {}: {error}",
                    candidate.display()
                ));
            }
        }
    }
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
    /// Exact Session-scoped persistence admission generation.
    pub(super) persistence_generation: ProjectPersistenceGeneration,
    /// Exact author generation represented by the archive.
    pub generation: AuthorGeneration,
    /// Exact Asset Library revision captured even when publication fails.
    pub asset_library_revision: u64,
    /// Original request purpose.
    pub purpose: ProjectPersistencePurpose,
    /// Durable result. Error strings retain the full worker-side cause chain.
    pub result: Result<PersistedProjectState, String>,
    /// Stable, actionable terminal failure evidence. This is orthogonal to
    /// irreversible namespace state in `publication_failure`.
    pub failure: Option<ProjectPersistenceFailure>,
    /// Typed irreversible-boundary failure, when publication reached one of
    /// the archive or Recovery Manifest seams.
    pub publication_failure: Option<ProjectPersistencePublicationFailure>,
    /// Namespace/durability state observed at the archive publication seam.
    archive_publication: ArchivePublicationState,
    /// Exact lease instance that admitted the request, without retaining it.
    pub(super) runtime_lease_id: ProjectRuntimeLeaseId,
}

/// Stable product-facing category for one terminal persistence failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectPersistenceFailureCategory {
    /// The destination filesystem or quota has no remaining capacity.
    StorageExhausted,
    /// The current identity cannot write or replace the requested destination.
    PermissionDenied,
    /// Create-only publication found an existing target.
    TargetConflict,
    /// A required source or destination ancestor disappeared.
    TargetUnavailable,
    /// Persisted input or a filesystem object has an invalid shape.
    InvalidData,
    /// Another classified operating-system I/O failure occurred.
    Io,
    /// A non-I/O invariant, dependency, or request failure occurred.
    Internal,
    /// The worker rejected a request before persistence execution.
    RequestRejected,
}

/// Cloneable typed failure delivered with a persistence completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPersistenceFailure {
    /// Stable category used by product recovery guidance.
    pub category: ProjectPersistenceFailureCategory,
    /// Complete worker-side diagnostic.
    pub reason: String,
}

/// Persistence publication phase that produced a typed terminal failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectPersistencePublicationPhase {
    /// Project archive namespace publication.
    Archive,
    /// Recovery Manifest namespace publication after a durable autosave archive.
    RecoveryManifest,
}

/// Irreversible-boundary classification for a persistence failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectPersistencePublicationFailureKind {
    /// The namespace operation is proven not to have completed.
    BeforeNamespace,
    /// The new object is visible, but crash durability is unconfirmed.
    DurabilityUnconfirmed,
    /// Neither unchanged nor published namespace state can be proven.
    NamespaceIndeterminate,
}

/// Cloneable terminal publication failure delivered across the worker seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPersistencePublicationFailure {
    /// Publication phase.
    pub phase: ProjectPersistencePublicationPhase,
    /// Irreversible-boundary classification.
    pub kind: ProjectPersistencePublicationFailureKind,
    /// Complete worker-side diagnostic.
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchivePublicationState {
    NotPublished,
    Durable,
    DurabilityUnconfirmed,
    NamespaceIndeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreateBindingState {
    Established,
    Poisoned,
}

struct ProjectPersistenceRequest {
    id: ProjectPersistenceRequestId,
    persistence_generation: ProjectPersistenceGeneration,
    snapshot: AuthoringSnapshot,
    purpose: ProjectPersistencePurpose,
    document_revision_to_publish: u64,
    runtime_lease: Arc<ProjectRuntimeLease>,
    #[cfg(test)]
    worker_gate: Option<TestPersistenceWorkerGate>,
    #[cfg(test)]
    worker_io_failure: Option<std::io::ErrorKind>,
}

struct ProjectPersistenceBarrier {
    session_id: AuthoringSessionId,
    admission_generation: ProjectPersistenceGeneration,
    acknowledgement_tx: SyncSender<ProjectPersistenceBarrierAcknowledgement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectPersistenceBarrierAcknowledgement {
    session_id: AuthoringSessionId,
    admission_generation: ProjectPersistenceGeneration,
}

enum ProjectPersistenceWorkerMessage {
    Request(Box<ProjectPersistenceRequest>),
    Barrier(ProjectPersistenceBarrier),
}

/// Bounded, single-writer durable persistence Module.
pub struct ProjectPersistenceService {
    service_id: ProjectPersistenceServiceId,
    worker_tx: mpsc::Sender<ProjectPersistenceWorkerMessage>,
    completion_rx: Receiver<ProjectPersistenceCompletion>,
    queued: Arc<AtomicUsize>,
    pending: Arc<AtomicUsize>,
    session_admission: HashMap<AuthoringSessionId, SessionAdmission>,
    next_request_id: u64,
    startup_error: Option<String>,
    #[cfg(test)]
    next_request_gate: Option<TestPersistenceWorkerGate>,
    #[cfg(test)]
    next_barrier_observer: Option<SyncSender<()>>,
    #[cfg(test)]
    next_submission_error: Option<String>,
    #[cfg(test)]
    next_worker_io_failure: Option<std::io::ErrorKind>,
}

impl ProjectPersistenceService {
    /// Start the lazy-independent persistence worker.
    pub fn new() -> Self {
        let (worker_tx, worker_rx) = mpsc::channel();
        let (completion_tx, completion_rx) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let pending = Arc::new(AtomicUsize::new(0));
        let worker_queued = Arc::clone(&queued);
        let worker_pending = Arc::clone(&pending);
        let (service_id, startup_error) = match next_persistence_service_id() {
            Ok(service_id) => {
                let startup_error = std::thread::Builder::new()
                    .name("mondrian-project-persistence".to_owned())
                    .spawn(move || {
                        persistence_worker(worker_rx, completion_tx, worker_queued, worker_pending)
                    })
                    .err()
                    .map(|error| format!("project persistence worker failed to start: {error}"));
                (service_id, startup_error)
            }
            Err(error) => (ProjectPersistenceServiceId(0), Some(error)),
        };
        Self {
            service_id,
            worker_tx,
            completion_rx,
            queued,
            pending,
            session_admission: HashMap::new(),
            next_request_id: 1,
            startup_error,
            #[cfg(test)]
            next_request_gate: None,
            #[cfg(test)]
            next_barrier_observer: None,
            #[cfg(test)]
            next_submission_error: None,
            #[cfg(test)]
            next_worker_io_failure: None,
        }
    }

    /// Submit an immutable save request without waiting for filesystem I/O.
    pub(super) fn submit(
        &mut self,
        snapshot: AuthoringSnapshot,
        purpose: ProjectPersistencePurpose,
        runtime_lease: Arc<ProjectRuntimeLease>,
    ) -> Result<ProjectPersistenceRequestId, String> {
        let session_id = snapshot.session_id;
        let persistence_generation = self.ensure_submit_admitted(session_id)?;
        #[cfg(test)]
        if let Some(error) = self.next_submission_error.take() {
            return Err(error);
        }
        if let Some(error) = &self.startup_error {
            return Err(error.clone());
        }
        if snapshot.document.project_id != runtime_lease.project_id() {
            return Err("project persistence snapshot belongs to another runtime lease".to_owned());
        }
        if let ProjectPersistencePurpose::Manual { destination } = &purpose {
            if destination.session_id() != snapshot.session_id {
                return Err(
                    "manual Project destination belongs to another Authoring Session".to_owned(),
                );
            }
        }
        runtime_lease.validate()?;
        let reserved_manual_document_revision =
            if matches!(&purpose, ProjectPersistencePurpose::Manual { .. }) {
                let admission = self.session_admission.get(&session_id).ok_or_else(|| {
                    "project persistence Session admission disappeared".to_owned()
                })?;
                let floor = admission
                    .last_reserved_manual_document_revision
                    .unwrap_or(snapshot.document.document_revision)
                    .max(snapshot.document.document_revision);
                Some(floor.checked_add(1).ok_or_else(|| {
                    "manual Project document revision exhausted before queue admission".to_owned()
                })?)
            } else {
                None
            };
        let previous_reserved_manual_document_revision = self
            .session_admission
            .get(&session_id)
            .and_then(|admission| admission.last_reserved_manual_document_revision);
        let document_revision_to_publish =
            reserved_manual_document_revision.unwrap_or(snapshot.document.document_revision);
        self.reserve_request_queue_slot()?;
        let id = ProjectPersistenceRequestId(self.next_request_id);
        let Some(next_request_id) = self.next_request_id.checked_add(1) else {
            self.queued.fetch_sub(1, Ordering::AcqRel);
            return Err("project persistence request identity exhausted".to_owned());
        };
        // A rejected request must not acquire namespace authority. Reserve the
        // bounded payload slot before retaining a manual target, and release
        // that slot if target admission itself fails. Once retained, the lock
        // deliberately survives for the Session lifetime because a queued
        // publisher may still own the route after caller-side completion.
        if let ProjectPersistencePurpose::Manual { destination } = &purpose {
            if let Err(error) = runtime_lease.retain_publication_target(destination.project_file())
            {
                self.queued.fetch_sub(1, Ordering::AcqRel);
                return Err(error);
            }
        }
        self.next_request_id = next_request_id;
        let request = ProjectPersistenceRequest {
            id,
            persistence_generation,
            snapshot,
            purpose,
            document_revision_to_publish,
            runtime_lease,
            #[cfg(test)]
            worker_gate: self.next_request_gate.take(),
            #[cfg(test)]
            worker_io_failure: self.next_worker_io_failure.take(),
        };
        // Publish request ownership before the worker can observe the message.
        self.pending.fetch_add(1, Ordering::AcqRel);
        if let Some(document_revision) = reserved_manual_document_revision {
            let Some(admission) = self.session_admission.get_mut(&session_id) else {
                self.queued.fetch_sub(1, Ordering::AcqRel);
                self.pending.fetch_sub(1, Ordering::AcqRel);
                return Err("project persistence Session admission disappeared".to_owned());
            };
            admission.last_reserved_manual_document_revision = Some(document_revision);
        }
        match self.worker_tx.send(ProjectPersistenceWorkerMessage::Request(Box::new(request))) {
            Ok(()) => Ok(id),
            Err(_) => {
                self.queued.fetch_sub(1, Ordering::AcqRel);
                self.pending.fetch_sub(1, Ordering::AcqRel);
                if let Some(admission) = self.session_admission.get_mut(&session_id) {
                    admission.last_reserved_manual_document_revision =
                        previous_reserved_manual_document_revision;
                }
                Err("project persistence worker is unavailable".to_owned())
            }
        }
    }

    /// Close one Session's admission and wait for a FIFO worker barrier.
    ///
    /// Success proves every earlier admitted request has finished publication
    /// and destroyed its request, Asset Library, and runtime-lease Arcs.
    /// Timeout or worker failure poisons the Session closed and returns no
    /// resumable token.
    pub(super) fn pause_and_quiesce(
        &mut self,
        session_id: AuthoringSessionId,
    ) -> Result<ProjectPersistencePauseToken, String> {
        self.pause_and_quiesce_with_timeout(session_id, PERSISTENCE_QUIESCENCE_TIMEOUT)
    }

    fn pause_and_quiesce_with_timeout(
        &mut self,
        session_id: AuthoringSessionId,
        timeout: Duration,
    ) -> Result<ProjectPersistencePauseToken, String> {
        let ticket = self.begin_pause_and_quiesce_with_timeout(session_id, timeout)?;
        let acknowledgement = match ticket.acknowledgement_rx.recv_timeout(timeout) {
            Ok(acknowledgement) => acknowledgement,
            Err(RecvTimeoutError::Timeout) => {
                self.poison_pausing_session(
                    ticket.token.session_id,
                    ticket.token.admission_generation,
                );
                return Err(Self::quiescence_timeout_error(timeout));
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.poison_pausing_session(
                    ticket.token.session_id,
                    ticket.token.admission_generation,
                );
                return Err(Self::quiescence_disconnected_error());
            }
        };
        self.complete_pause(ticket.token, acknowledgement)
    }

    /// Begin a FIFO persistence barrier without waiting on the calling thread.
    pub(super) fn begin_pause_and_quiesce(
        &mut self,
        session_id: AuthoringSessionId,
    ) -> Result<ProjectPersistencePauseTicket, String> {
        self.begin_pause_and_quiesce_with_timeout(session_id, PERSISTENCE_QUIESCENCE_TIMEOUT)
    }

    fn begin_pause_and_quiesce_with_timeout(
        &mut self,
        session_id: AuthoringSessionId,
        timeout: Duration,
    ) -> Result<ProjectPersistencePauseTicket, String> {
        let admission_generation = self.begin_pause(session_id)?;
        let token = ProjectPersistencePauseToken {
            service_id: self.service_id,
            session_id,
            admission_generation,
        };
        if let Some(error) = &self.startup_error {
            let error = error.clone();
            self.poison_pausing_session(session_id, admission_generation);
            return Err(error);
        }

        // Capacity one lets the worker publish the terminal barrier fact and
        // continue teardown even when the event loop is temporarily asleep.
        // The ticket remains the sole consumer and therefore the sole
        // authority capable of moving Pausing -> Paused.
        let (acknowledgement_tx, acknowledgement_rx) = mpsc::sync_channel(1);
        let barrier = ProjectPersistenceBarrier {
            session_id,
            admission_generation,
            acknowledgement_tx,
        };
        if self.worker_tx.send(ProjectPersistenceWorkerMessage::Barrier(barrier)).is_err() {
            self.poison_pausing_session(session_id, admission_generation);
            return Err("project persistence worker is unavailable during quiescence".to_owned());
        }
        #[cfg(test)]
        if let Some(observer) = self.next_barrier_observer.take() {
            let _ = observer.send(());
        }

        Ok(ProjectPersistencePauseTicket {
            token,
            acknowledgement_rx,
            started_at: Instant::now(),
            timeout,
        })
    }

    /// Poll one non-blocking quiescence ticket.
    ///
    /// `Ok(None)` means the FIFO barrier has not reached the worker yet. A
    /// terminal acknowledgement is validated against the service, Session,
    /// and admission generation before the pause token becomes usable.
    pub(super) fn poll_pause_and_quiesce(
        &mut self,
        ticket: &ProjectPersistencePauseTicket,
    ) -> Result<Option<ProjectPersistencePauseToken>, String> {
        let acknowledgement = match ticket.acknowledgement_rx.try_recv() {
            Ok(acknowledgement) => acknowledgement,
            Err(TryRecvError::Empty) if ticket.started_at.elapsed() < ticket.timeout => {
                return Ok(None);
            }
            Err(TryRecvError::Empty) => {
                self.poison_pausing_session(
                    ticket.token.session_id,
                    ticket.token.admission_generation,
                );
                return Err(Self::quiescence_timeout_error(ticket.timeout));
            }
            Err(TryRecvError::Disconnected) => {
                self.poison_pausing_session(
                    ticket.token.session_id,
                    ticket.token.admission_generation,
                );
                return Err(Self::quiescence_disconnected_error());
            }
        };
        self.complete_pause(ticket.token, acknowledgement).map(Some)
    }

    /// Fail closed a ticket whose caller-side lifecycle invariants were
    /// violated before the worker acknowledgement could be consumed.
    pub(super) fn poison_pause_ticket(&mut self, ticket: &ProjectPersistencePauseTicket) {
        self.poison_pausing_session(ticket.token.session_id, ticket.token.admission_generation);
    }

    fn complete_pause(
        &mut self,
        token: ProjectPersistencePauseToken,
        acknowledgement: ProjectPersistenceBarrierAcknowledgement,
    ) -> Result<ProjectPersistencePauseToken, String> {
        if acknowledgement.session_id != token.session_id
            || acknowledgement.admission_generation != token.admission_generation
        {
            self.poison_pausing_session(token.session_id, token.admission_generation);
            return Err(
                "project persistence worker returned the wrong quiescence acknowledgement; Session admission remains poisoned"
                    .to_owned(),
            );
        }
        let Some(admission) = self.session_admission.get_mut(&token.session_id) else {
            return Err("project persistence Session admission disappeared".to_owned());
        };
        if admission.generation != token.admission_generation
            || admission.phase != SessionAdmissionPhase::Pausing
        {
            admission.phase = SessionAdmissionPhase::Poisoned;
            return Err(
                "project persistence Session admission changed during quiescence and was poisoned"
                    .to_owned(),
            );
        }
        admission.phase = SessionAdmissionPhase::Paused;
        Ok(token)
    }

    fn quiescence_timeout_error(timeout: Duration) -> String {
        format!(
            "project persistence quiescence timed out after {} seconds; Session admission remains poisoned",
            timeout.as_secs()
        )
    }

    fn quiescence_disconnected_error() -> String {
        "project persistence worker disconnected during quiescence; Session admission remains poisoned"
            .to_owned()
    }

    /// Reopen exactly the paused admission generation represented by `token`.
    pub(super) fn resume(&mut self, token: ProjectPersistencePauseToken) -> Result<(), String> {
        let admission = self.validate_pause_token(token)?;
        let next_generation = admission.generation.next()?;
        admission.generation = next_generation;
        admission.phase = SessionAdmissionPhase::Open;
        Ok(())
    }

    /// Permanently reject future submissions from the token's Session.
    pub(super) fn retire(&mut self, token: ProjectPersistencePauseToken) -> Result<(), String> {
        let admission = self.validate_pause_token(token)?;
        admission.phase = SessionAdmissionPhase::Retired;
        Ok(())
    }

    /// Irrevocably detach a failed lifecycle from one exact Session without
    /// claiming worker quiescence.
    ///
    /// Outstanding requests retain their own heavy Arcs and may finish their
    /// filesystem attempt, but every later completion is rejected because the
    /// Session admission is `Retired`. This is the explicit user-authorized
    /// escape from a poisoned/hung close protocol, not a successful barrier.
    pub(super) fn abandon_session(&mut self, session_id: AuthoringSessionId) -> Result<(), String> {
        let admission = self
            .session_admission
            .get_mut(&session_id)
            .ok_or_else(|| "cannot abandon an unknown Project persistence Session".to_owned())?;
        match admission.phase {
            SessionAdmissionPhase::Pausing
            | SessionAdmissionPhase::Paused
            | SessionAdmissionPhase::Poisoned => {
                admission.phase = SessionAdmissionPhase::Retired;
                Ok(())
            }
            SessionAdmissionPhase::Open => Err(
                "cannot abandon an open Project persistence Session without a failed close"
                    .to_owned(),
            ),
            SessionAdmissionPhase::Retired => {
                Err("Project persistence Session is already retired".to_owned())
            }
        }
    }

    /// Drain a bounded number of terminal completions.
    pub fn poll_completions(&self) -> Vec<ProjectPersistenceCompletion> {
        self.completion_rx.try_iter().take(MAX_COMPLETIONS_PER_POLL).collect()
    }

    /// Whether a completion still belongs to the exact admitted Session lifetime.
    ///
    /// A FIFO barrier removes every earlier worker request, but this check also
    /// closes the delivery-queue seam: a completion retained by the App across
    /// resume, retirement, or poisoning can never mutate the current baseline.
    pub(super) fn accepts_completion(&self, completion: &ProjectPersistenceCompletion) -> bool {
        self.session_admission.get(&completion.session_id).is_some_and(|admission| {
            admission.generation == completion.persistence_generation
                && matches!(
                    admission.phase,
                    SessionAdmissionPhase::Open
                        | SessionAdmissionPhase::Pausing
                        | SessionAdmissionPhase::Paused
                )
        })
    }

    /// Number of admitted requests not yet completed.
    #[cfg(test)]
    pub fn pending_requests(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    fn ensure_submit_admitted(
        &mut self,
        session_id: AuthoringSessionId,
    ) -> Result<ProjectPersistenceGeneration, String> {
        let admission =
            self.session_admission.entry(session_id).or_insert_with(SessionAdmission::open);
        match admission.phase {
            SessionAdmissionPhase::Open => Ok(admission.generation),
            SessionAdmissionPhase::Pausing => Err(
                "project persistence admission is closing for this Authoring Session".to_owned(),
            ),
            SessionAdmissionPhase::Paused => {
                Err("project persistence admission is paused for this Authoring Session".to_owned())
            }
            SessionAdmissionPhase::Retired => Err(
                "project persistence admission is retired for this Authoring Session".to_owned(),
            ),
            SessionAdmissionPhase::Poisoned => Err(
                "project persistence admission is poisoned for this Authoring Session".to_owned(),
            ),
        }
    }

    fn reserve_request_queue_slot(&self) -> Result<(), String> {
        self.queued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |queued| {
                (queued < PERSISTENCE_QUEUE_CAPACITY).then_some(queued + 1)
            })
            .map(|_| ())
            .map_err(|_| "project persistence queue is full".to_owned())
    }

    fn begin_pause(
        &mut self,
        session_id: AuthoringSessionId,
    ) -> Result<ProjectPersistenceGeneration, String> {
        let admission =
            self.session_admission.entry(session_id).or_insert_with(SessionAdmission::open);
        match admission.phase {
            SessionAdmissionPhase::Open => {
                admission.phase = SessionAdmissionPhase::Pausing;
                Ok(admission.generation)
            }
            SessionAdmissionPhase::Pausing => Err(
                "project persistence admission is already closing for this Authoring Session"
                    .to_owned(),
            ),
            SessionAdmissionPhase::Paused => Err(
                "project persistence admission is already paused for this Authoring Session"
                    .to_owned(),
            ),
            SessionAdmissionPhase::Retired => Err(
                "project persistence admission is retired for this Authoring Session".to_owned(),
            ),
            SessionAdmissionPhase::Poisoned => Err(
                "project persistence admission is poisoned for this Authoring Session".to_owned(),
            ),
        }
    }

    fn poison_pausing_session(
        &mut self,
        session_id: AuthoringSessionId,
        admission_generation: ProjectPersistenceGeneration,
    ) {
        if let Some(admission) = self.session_admission.get_mut(&session_id) {
            if admission.generation == admission_generation
                && admission.phase == SessionAdmissionPhase::Pausing
            {
                admission.phase = SessionAdmissionPhase::Poisoned;
            }
        }
    }

    fn validate_pause_token(
        &mut self,
        token: ProjectPersistencePauseToken,
    ) -> Result<&mut SessionAdmission, String> {
        if token.service_id != self.service_id {
            return Err(
                "Project persistence pause token belongs to another persistence service".to_owned(),
            );
        }
        let admission = self.session_admission.get_mut(&token.session_id).ok_or_else(|| {
            "Project persistence pause token belongs to an unknown Authoring Session".to_owned()
        })?;
        if admission.generation != token.admission_generation {
            return Err("Project persistence pause token is stale".to_owned());
        }
        if admission.phase != SessionAdmissionPhase::Paused {
            return Err("Project persistence pause token is no longer active".to_owned());
        }
        Ok(admission)
    }

    #[cfg(test)]
    pub(super) fn gate_next_request(&mut self) -> TestPersistenceWorkerGateControl {
        assert!(
            self.next_request_gate.is_none(),
            "only one injected persistence worker gate may be pending"
        );
        let (gate, control) = TestPersistenceWorkerGate::new();
        self.next_request_gate = Some(gate);
        control
    }

    #[cfg(test)]
    fn observe_next_barrier(&mut self) -> Receiver<()> {
        assert!(
            self.next_barrier_observer.is_none(),
            "only one persistence barrier observer may be pending"
        );
        let (observer_tx, observer_rx) = mpsc::sync_channel(0);
        self.next_barrier_observer = Some(observer_tx);
        observer_rx
    }

    #[cfg(test)]
    pub(super) fn poison_session_admission_for_test(&mut self, session_id: AuthoringSessionId) {
        let admission =
            self.session_admission.entry(session_id).or_insert_with(SessionAdmission::open);
        admission.phase = SessionAdmissionPhase::Poisoned;
    }

    #[cfg(test)]
    pub(super) fn fail_next_submission_for_test(&mut self, reason: impl Into<String>) {
        self.next_submission_error = Some(reason.into());
    }

    #[cfg(test)]
    pub(super) fn fail_next_worker_io_for_test(&mut self, kind: std::io::ErrorKind) {
        assert!(
            self.next_worker_io_failure.is_none(),
            "only one injected persistence worker I/O failure may be pending"
        );
        self.next_worker_io_failure = Some(kind);
    }
}

impl Default for ProjectPersistenceService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
struct TestPersistenceWorkerGate {
    started_tx: SyncSender<()>,
    release_rx: Receiver<()>,
}

#[cfg(test)]
impl TestPersistenceWorkerGate {
    fn new() -> (Self, TestPersistenceWorkerGateControl) {
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        (
            Self { started_tx, release_rx },
            TestPersistenceWorkerGateControl { started_rx, release_tx },
        )
    }

    fn wait(self) {
        let _ = self.started_tx.send(());
        let _ = self.release_rx.recv();
    }
}

#[cfg(test)]
pub(super) struct TestPersistenceWorkerGateControl {
    started_rx: Receiver<()>,
    release_tx: SyncSender<()>,
}

#[cfg(test)]
impl TestPersistenceWorkerGateControl {
    pub(super) fn wait_until_running(&self) {
        self.started_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("persistence worker did not reach injected gate");
    }

    pub(super) fn release(self) {
        self.release_tx.send(()).expect("release persistence worker gate");
    }
}

fn persistence_worker(
    worker_rx: Receiver<ProjectPersistenceWorkerMessage>,
    completion_tx: mpsc::Sender<ProjectPersistenceCompletion>,
    queued: Arc<AtomicUsize>,
    pending: Arc<AtomicUsize>,
) {
    let mut create_bindings = HashMap::new();
    let mut last_successful_manual_revision = HashMap::new();
    while let Ok(message) = worker_rx.recv() {
        match message {
            ProjectPersistenceWorkerMessage::Request(request) => {
                queued.fetch_sub(1, Ordering::AcqRel);
                let mut request = *request;
                #[cfg(test)]
                if let Some(gate) = request.worker_gate.take() {
                    gate.wait();
                }
                let requested_create_binding = match &request.purpose {
                    ProjectPersistencePurpose::Manual { destination }
                        if destination.publication() == ProjectArchivePublication::CreateNew =>
                    {
                        Some(destination.lineage_key())
                    }
                    _ => None,
                };
                let effective_publication = match requested_create_binding
                    .and_then(|key| create_bindings.get(&key).copied())
                {
                    Some(CreateBindingState::Established) => {
                        Ok(ProjectArchivePublication::ReplaceExisting)
                    }
                    Some(CreateBindingState::Poisoned) => Err(
                        "the previous create-only publication has an indeterminate namespace; choose a different Save As destination or recover the retained archive before retrying"
                            .to_owned(),
                    ),
                    None => Ok(if matches!(
                        &request.purpose,
                        ProjectPersistencePurpose::Autosave { .. }
                    ) || requested_create_binding.is_some()
                    {
                        ProjectArchivePublication::CreateNew
                    } else {
                        ProjectArchivePublication::ReplaceExisting
                    }),
                };
                if matches!(&request.purpose, ProjectPersistencePurpose::Autosave { .. }) {
                    if let Some(successful_revision) =
                        last_successful_manual_revision.get(&request.snapshot.session_id)
                    {
                        request.document_revision_to_publish =
                            request.document_revision_to_publish.max(*successful_revision);
                    }
                }
                let completion = match effective_publication {
                    Ok(publication) => execute_persistence_request(request, publication),
                    Err(reason) => reject_persistence_request(request, reason),
                };
                update_worker_publication_state(
                    &mut create_bindings,
                    &mut last_successful_manual_revision,
                    requested_create_binding,
                    &completion,
                );
                pending.fetch_sub(1, Ordering::AcqRel);
                let _ = completion_tx.send(completion);
            }
            ProjectPersistenceWorkerMessage::Barrier(barrier) => {
                let acknowledgement = ProjectPersistenceBarrierAcknowledgement {
                    session_id: barrier.session_id,
                    admission_generation: barrier.admission_generation,
                };
                let _ = barrier.acknowledgement_tx.send(acknowledgement);
            }
        }
    }
}

fn update_worker_publication_state(
    create_bindings: &mut HashMap<(AuthoringSessionId, u64), CreateBindingState>,
    last_successful_manual_revision: &mut HashMap<AuthoringSessionId, u64>,
    requested_create_binding: Option<(AuthoringSessionId, u64)>,
    completion: &ProjectPersistenceCompletion,
) {
    if let Some(key) = requested_create_binding {
        match completion.archive_publication {
            ArchivePublicationState::Durable | ArchivePublicationState::DurabilityUnconfirmed => {
                create_bindings.insert(key, CreateBindingState::Established);
            }
            ArchivePublicationState::NamespaceIndeterminate => {
                create_bindings.insert(key, CreateBindingState::Poisoned);
            }
            ArchivePublicationState::NotPublished => {}
        }
    }
    if let Ok(persisted) = &completion.result {
        if matches!(
            &completion.purpose,
            ProjectPersistencePurpose::Manual { .. }
        ) {
            last_successful_manual_revision
                .insert(completion.session_id, persisted.document_revision);
        }
    }
}

fn execute_persistence_request(
    request: ProjectPersistenceRequest,
    publication: ProjectArchivePublication,
) -> ProjectPersistenceCompletion {
    let ProjectPersistenceRequest {
        id,
        persistence_generation,
        snapshot,
        purpose,
        document_revision_to_publish,
        runtime_lease,
        #[cfg(test)]
        worker_io_failure,
        ..
    } = request;
    let AuthoringSnapshot {
        session_id,
        generation,
        asset_library_revision,
        document,
        asset_library,
    } = snapshot;
    let runtime_lease_id = runtime_lease.id();
    let target_file = purpose.target_file().to_path_buf();
    let mut archive_publication = ArchivePublicationState::NotPublished;
    let mut publication_failure = None;
    let mut failure = None;
    let result = (|| {
        runtime_lease.validate()?;
        #[cfg(test)]
        if let Some(kind) = worker_io_failure {
            let error = std::io::Error::from(kind);
            failure = Some(project_persistence_failure(&error, error.to_string()));
            return Err(error.to_string());
        }
        let mut document = document;
        if let ProjectPersistencePurpose::Autosave { .. } = &purpose {
            prepare_recovery_archive_target(&runtime_lease, &target_file)?;
        }
        ensure_durable_publication_parent(&target_file)?;
        document.document_revision = document_revision_to_publish;
        document.meta.touch();
        let database_snapshot = asset_library
            .snapshot_database(asset_library_revision, &target_file)
            .map_err(|error| error.to_string())?;
        let mut database_snapshot_reader =
            database_snapshot.try_clone_reader().map_err(|error| error.to_string())?;
        runtime_lease.validate()?;
        let archive_evidence = match save_project_archive_from_open_library_with_publication(
            &document,
            &mut database_snapshot_reader,
            &target_file,
            publication,
        ) {
            Ok(evidence) => {
                archive_publication = ArchivePublicationState::Durable;
                evidence
            }
            Err(error) => {
                failure = Some(project_persistence_failure(&error, error.to_string()));
                let kind = match &error {
                    ProjectArchivePublicationFailure::BeforeNamespace(_) => {
                        archive_publication = ArchivePublicationState::NotPublished;
                        ProjectPersistencePublicationFailureKind::BeforeNamespace
                    }
                    ProjectArchivePublicationFailure::DurabilityUnconfirmed(_) => {
                        archive_publication = ArchivePublicationState::DurabilityUnconfirmed;
                        ProjectPersistencePublicationFailureKind::DurabilityUnconfirmed
                    }
                    ProjectArchivePublicationFailure::NamespaceIndeterminate(_) => {
                        archive_publication = ArchivePublicationState::NamespaceIndeterminate;
                        ProjectPersistencePublicationFailureKind::NamespaceIndeterminate
                    }
                };
                publication_failure = Some(ProjectPersistencePublicationFailure {
                    phase: ProjectPersistencePublicationPhase::Archive,
                    kind,
                    reason: error.to_string(),
                });
                return Err(format!("{error:#}"));
            }
        };
        drop(database_snapshot_reader);
        drop(database_snapshot);
        if let ProjectPersistencePurpose::Autosave {
            destination,
            max_recovery_points,
            retention_days,
            saved_at_unix_ms,
        } = &purpose
        {
            if let Err(error) = publish_recovery_point(RecoveryPointPublication {
                project_file: destination.canonical_project_file(),
                runtime_lease: &runtime_lease,
                archive_evidence,
                author_generation: generation.get(),
                asset_library_revision,
                saved_at_unix_ms: *saved_at_unix_ms,
                max_recovery_points: *max_recovery_points,
                retention_days: *retention_days,
            }) {
                failure = Some(project_persistence_failure(&error, error.to_string()));
                let kind = match &error {
                    RecoveryManifestPublicationFailure::BeforeNamespace(_) => {
                        ProjectPersistencePublicationFailureKind::BeforeNamespace
                    }
                    RecoveryManifestPublicationFailure::DurabilityUnconfirmed(_) => {
                        ProjectPersistencePublicationFailureKind::DurabilityUnconfirmed
                    }
                    RecoveryManifestPublicationFailure::NamespaceIndeterminate(_) => {
                        ProjectPersistencePublicationFailureKind::NamespaceIndeterminate
                    }
                };
                publication_failure = Some(ProjectPersistencePublicationFailure {
                    phase: ProjectPersistencePublicationPhase::RecoveryManifest,
                    kind,
                    reason: error.to_string(),
                });
                return Err(error.to_string());
            }
        }
        Ok(PersistedProjectState {
            document_revision: document.document_revision,
            asset_library_revision,
            meta: document.meta,
        })
    })();
    if result.is_err() && failure.is_none() {
        failure = Some(ProjectPersistenceFailure {
            category: ProjectPersistenceFailureCategory::Internal,
            reason: result.as_ref().expect_err("failure result").clone(),
        });
    }
    // Quiescence relies on these being destroyed before the worker can process
    // the next FIFO barrier. Completions carry only the scalar lease identity.
    drop(asset_library);
    drop(runtime_lease);
    ProjectPersistenceCompletion {
        request_id: id,
        session_id,
        persistence_generation,
        generation,
        asset_library_revision,
        purpose,
        result,
        failure,
        publication_failure,
        archive_publication,
        runtime_lease_id,
    }
}

fn reject_persistence_request(
    request: ProjectPersistenceRequest,
    reason: String,
) -> ProjectPersistenceCompletion {
    let ProjectPersistenceRequest {
        id,
        persistence_generation,
        snapshot,
        purpose,
        runtime_lease,
        ..
    } = request;
    let runtime_lease_id = runtime_lease.id();
    ProjectPersistenceCompletion {
        request_id: id,
        session_id: snapshot.session_id,
        persistence_generation,
        generation: snapshot.generation,
        asset_library_revision: snapshot.asset_library_revision,
        purpose,
        result: Err(reason.clone()),
        failure: Some(ProjectPersistenceFailure {
            category: ProjectPersistenceFailureCategory::RequestRejected,
            reason: reason.clone(),
        }),
        publication_failure: None,
        archive_publication: ArchivePublicationState::NotPublished,
        runtime_lease_id,
    }
}

fn project_persistence_failure(
    error: &(dyn std::error::Error + 'static),
    reason: String,
) -> ProjectPersistenceFailure {
    let mut source = Some(error);
    while let Some(current) = source {
        if let Some(io_error) = current.downcast_ref::<std::io::Error>() {
            return ProjectPersistenceFailure {
                category: classify_persistence_io_error(io_error),
                reason,
            };
        }
        source = current.source();
    }
    ProjectPersistenceFailure {
        category: ProjectPersistenceFailureCategory::Internal,
        reason,
    }
}

fn classify_persistence_io_error(error: &std::io::Error) -> ProjectPersistenceFailureCategory {
    use std::io::ErrorKind;

    match error.kind() {
        ErrorKind::StorageFull => ProjectPersistenceFailureCategory::StorageExhausted,
        ErrorKind::PermissionDenied => ProjectPersistenceFailureCategory::PermissionDenied,
        ErrorKind::AlreadyExists => ProjectPersistenceFailureCategory::TargetConflict,
        ErrorKind::NotFound => ProjectPersistenceFailureCategory::TargetUnavailable,
        ErrorKind::InvalidData | ErrorKind::InvalidInput => {
            ProjectPersistenceFailureCategory::InvalidData
        }
        _ if is_storage_exhausted_os_error(error.raw_os_error()) => {
            ProjectPersistenceFailureCategory::StorageExhausted
        }
        _ => ProjectPersistenceFailureCategory::Io,
    }
}

fn is_storage_exhausted_os_error(raw: Option<i32>) -> bool {
    #[cfg(target_os = "windows")]
    {
        matches!(raw, Some(39 | 112 | 1816))
    }
    #[cfg(target_os = "linux")]
    {
        matches!(raw, Some(28 | 122))
    }
    #[cfg(target_os = "macos")]
    {
        matches!(raw, Some(28 | 69))
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = raw;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::super::project_runtime::{
        claim_project_runtime_lease_for_test, lease_existing_project_runtime_for_test,
    };
    use super::*;
    use mondrian_assets::AssetLibrary;
    use mondrian_core::ProjectSettings;
    use mondrian_editor_state::AuthoringSession;
    use mondrian_project::{load_project_archive, ProjectDocument};
    use mondrian_timeline::{Sequence, SequenceCollection, SequenceSettings};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn unique_root(_name: &str) -> PathBuf {
        static NEXT_ROOT_ID: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "mp-{:x}-{:x}",
            std::process::id(),
            NEXT_ROOT_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn current_unix_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_millis() as u64
    }

    fn session(root: &Path) -> (AuthoringSession, Arc<ProjectRuntimeLease>) {
        let document = ProjectDocument::new(
            "Persistence Test",
            SequenceCollection::new(Sequence::new("Sequence")),
            mondrian_core::ProjectColorEnvironment::default(),
            SequenceSettings::default(),
            ProjectSettings::default(),
        );
        let project_file = root.join("project.mdp");
        let lease = claim_project_runtime_lease_for_test(
            &root.join("runtime-roots"),
            &project_file,
            document.project_id,
        )
        .expect("claim runtime owner");
        let runtime_root = lease.runtime_root().to_path_buf();
        let library = AssetLibrary::open(runtime_root.join("library")).expect("asset library");
        (
            AuthoringSession::new_unsaved(document, project_file, runtime_root, library)
                .expect("authoring session"),
            lease,
        )
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

    fn wait_for_all(
        service: &ProjectPersistenceService,
        request_ids: &[ProjectPersistenceRequestId],
    ) -> HashMap<u64, ProjectPersistenceCompletion> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut completions = HashMap::new();
        while completions.len() < request_ids.len() {
            for completion in service.poll_completions() {
                if request_ids.contains(&completion.request_id) {
                    completions.insert(completion.request_id.get(), completion);
                }
            }
            assert!(
                Instant::now() < deadline,
                "persistence requests did not complete"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        completions
    }

    #[test]
    fn create_binding_distinguishes_unconfirmed_from_indeterminate_without_saved_baseline() {
        let root = unique_root("create-binding-outcomes");
        std::fs::create_dir_all(&root).expect("create root");
        let (authoring, lease) = session(&root);
        let snapshot = authoring.snapshot().expect("snapshot");
        let destination = ManualProjectFileDestination::initial_create(
            snapshot.session_id,
            root.join("new-project.mdp"),
        )
        .expect("create destination");
        let key = destination.lineage_key();
        let mut completion = ProjectPersistenceCompletion {
            request_id: ProjectPersistenceRequestId(1),
            session_id: snapshot.session_id,
            persistence_generation: ProjectPersistenceGeneration::INITIAL,
            generation: snapshot.generation,
            asset_library_revision: snapshot.asset_library_revision,
            purpose: ProjectPersistencePurpose::Manual { destination },
            result: Err("platform durability barrier was not confirmed".to_owned()),
            failure: Some(ProjectPersistenceFailure {
                category: ProjectPersistenceFailureCategory::Io,
                reason: "modeled durability uncertainty".to_owned(),
            }),
            publication_failure: Some(ProjectPersistencePublicationFailure {
                phase: ProjectPersistencePublicationPhase::Archive,
                kind: ProjectPersistencePublicationFailureKind::DurabilityUnconfirmed,
                reason: "modeled durability uncertainty".to_owned(),
            }),
            archive_publication: ArchivePublicationState::DurabilityUnconfirmed,
            runtime_lease_id: lease.id(),
        };
        let mut bindings = HashMap::new();
        let mut successful_revisions = HashMap::new();
        update_worker_publication_state(
            &mut bindings,
            &mut successful_revisions,
            Some(key),
            &completion,
        );
        assert_eq!(bindings.get(&key), Some(&CreateBindingState::Established));
        assert!(
            successful_revisions.is_empty(),
            "durability-unconfirmed publication cannot advance the saved high-water mark"
        );

        completion.archive_publication = ArchivePublicationState::NamespaceIndeterminate;
        completion.publication_failure = Some(ProjectPersistencePublicationFailure {
            phase: ProjectPersistencePublicationPhase::Archive,
            kind: ProjectPersistencePublicationFailureKind::NamespaceIndeterminate,
            reason: "modeled namespace ambiguity".to_owned(),
        });
        bindings.clear();
        update_worker_publication_state(
            &mut bindings,
            &mut successful_revisions,
            Some(key),
            &completion,
        );
        assert_eq!(bindings.get(&key), Some(&CreateBindingState::Poisoned));
        assert!(successful_revisions.is_empty());
    }

    #[test]
    fn background_save_embeds_the_exact_sqlite_snapshot() {
        let root = unique_root("sqlite-snapshot");
        let (authoring, lease) = session(&root);
        let asset_id = authoring
            .asset_library()
            .create_solid_color_asset(Some("Snapshot Asset"))
            .expect("create asset");
        let snapshot = authoring.snapshot().expect("authoring snapshot");
        let target = root.join("saved.mdp");
        let mut service = ProjectPersistenceService::new();
        let destination =
            ManualProjectFileDestination::initial(snapshot.session_id, target.clone())
                .expect("manual destination");
        let request_id = service
            .submit(
                snapshot.clone(),
                ProjectPersistencePurpose::Manual { destination },
                lease,
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
    fn persistence_request_cleans_its_opaque_snapshot_after_archive_publication() {
        let root = unique_root("sqlite-snapshot-cleanup");
        let (authoring, lease) = session(&root);
        let snapshot = authoring.snapshot().expect("authoring snapshot");
        let target = root.join("saved.mdp");
        let mut service = ProjectPersistenceService::new();
        let expected_request_id = ProjectPersistenceRequestId(service.next_request_id);
        let destination =
            ManualProjectFileDestination::initial(snapshot.session_id, target.clone())
                .expect("manual destination");

        let request_id = service
            .submit(
                snapshot,
                ProjectPersistencePurpose::Manual { destination },
                lease,
            )
            .expect("submit save");
        assert_eq!(request_id, expected_request_id);
        wait_for(&service, request_id).result.expect("opaque snapshot save");

        assert!(target.is_file());
        let leaked_snapshots = std::fs::read_dir(&root)
            .expect("read persistence root")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_name().to_string_lossy().starts_with(".m-asset-library-snapshot-")
            })
            .collect::<Vec<_>>();
        assert!(
            leaked_snapshots.is_empty(),
            "request-owned snapshot objects must be cleaned after archive publication"
        );
    }

    #[test]
    fn queued_manual_publications_reserve_distinct_checked_document_revisions() {
        let root = unique_root("manual-document-revisions");
        let (authoring, lease) = session(&root);
        let snapshot = authoring.snapshot().expect("authoring snapshot");
        let baseline = snapshot.document.document_revision;
        let target = root.join("saved.mdp");
        let destination =
            ManualProjectFileDestination::initial(snapshot.session_id, target.clone())
                .expect("manual destination");
        let mut service = ProjectPersistenceService::new();

        let first = service
            .submit(
                snapshot.clone(),
                ProjectPersistencePurpose::Manual { destination: destination.clone() },
                Arc::clone(&lease),
            )
            .expect("submit first save");
        let second = service
            .submit(
                snapshot,
                ProjectPersistencePurpose::Manual { destination },
                lease,
            )
            .expect("submit second save before applying the first completion");
        let mut completions = wait_for_all(&service, &[first, second]);
        let first_revision = completions
            .remove(&first.get())
            .expect("first completion")
            .result
            .expect("first publication")
            .document_revision;
        let second_revision = completions
            .remove(&second.get())
            .expect("second completion")
            .result
            .expect("second publication")
            .document_revision;

        assert_eq!(first_revision, baseline + 1);
        assert_eq!(second_revision, baseline + 2);
        assert_eq!(
            load_project_archive(&target, &root.join("manual-revision-extract"))
                .expect("open final archive")
                .document
                .document_revision,
            second_revision
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn autosave_uses_the_latest_successful_manual_revision_without_advancing_it() {
        let root = unique_root("autosave-document-revision");
        let (authoring, lease) = session(&root);
        let snapshot = authoring.snapshot().expect("authoring snapshot");
        let baseline = snapshot.document.document_revision;
        let manual_target = root.join("saved.mdp");
        let autosave_target =
            authoring.runtime_root().join("autosave").join("after-manual.autosave.mdp");
        let mut service = ProjectPersistenceService::new();

        let manual = service
            .submit(
                snapshot.clone(),
                ProjectPersistencePurpose::Manual {
                    destination: ManualProjectFileDestination::initial(
                        snapshot.session_id,
                        manual_target,
                    )
                    .expect("manual destination"),
                },
                Arc::clone(&lease),
            )
            .expect("submit manual save");
        let autosave = service
            .submit(
                snapshot,
                ProjectPersistencePurpose::Autosave {
                    destination: AutosaveArchiveDestination::new(
                        autosave_target.clone(),
                        authoring.project_file().to_path_buf(),
                    )
                    .expect("autosave destination"),
                    max_recovery_points: 2,
                    retention_days: 7,
                    saved_at_unix_ms: current_unix_ms(),
                },
                lease,
            )
            .expect("submit autosave behind manual publication");
        let mut completions = wait_for_all(&service, &[manual, autosave]);
        let manual_revision = completions
            .remove(&manual.get())
            .expect("manual completion")
            .result
            .expect("manual publication")
            .document_revision;
        let autosave_revision = completions
            .remove(&autosave.get())
            .expect("autosave completion")
            .result
            .expect("autosave publication")
            .document_revision;

        assert_eq!(manual_revision, baseline + 1);
        assert_eq!(autosave_revision, manual_revision);
        assert_eq!(
            load_project_archive(&autosave_target, &root.join("autosave-revision-extract"))
                .expect("open autosave archive")
                .document
                .document_revision,
            manual_revision
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_manual_publication_does_not_advance_a_following_autosave_revision() {
        let root = unique_root("failed-manual-document-revision");
        let (authoring, lease) = session(&root);
        let snapshot = authoring.snapshot().expect("authoring snapshot");
        let baseline = snapshot.document.document_revision;
        let blocked_parent = root.join("blocked");
        std::fs::create_dir_all(&root).expect("create root");
        std::fs::write(&blocked_parent, b"not a directory").expect("create path blocker");
        let autosave_target =
            authoring.runtime_root().join("autosave").join("after-failure.autosave.mdp");
        let mut service = ProjectPersistenceService::new();

        let manual = service
            .submit(
                snapshot.clone(),
                ProjectPersistencePurpose::Manual {
                    destination: ManualProjectFileDestination::initial(
                        snapshot.session_id,
                        blocked_parent.join("failed.mdp"),
                    )
                    .expect("manual destination"),
                },
                Arc::clone(&lease),
            )
            .expect("submit failing manual save");
        let autosave = service
            .submit(
                snapshot,
                ProjectPersistencePurpose::Autosave {
                    destination: AutosaveArchiveDestination::new(
                        autosave_target.clone(),
                        authoring.project_file().to_path_buf(),
                    )
                    .expect("autosave destination"),
                    max_recovery_points: 2,
                    retention_days: 7,
                    saved_at_unix_ms: current_unix_ms(),
                },
                lease,
            )
            .expect("submit autosave behind failed manual publication");
        let mut completions = wait_for_all(&service, &[manual, autosave]);
        assert!(completions.remove(&manual.get()).expect("manual completion").result.is_err());
        let autosave_revision = completions
            .remove(&autosave.get())
            .expect("autosave completion")
            .result
            .expect("autosave publication")
            .document_revision;

        assert_eq!(autosave_revision, baseline);
        assert_eq!(
            load_project_archive(&autosave_target, &root.join("failed-manual-extract"))
                .expect("open autosave archive")
                .document
                .document_revision,
            baseline
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn injected_storage_and_permission_failures_preserve_canonical_project_bytes() {
        let cases = [
            (
                std::io::ErrorKind::StorageFull,
                ProjectPersistenceFailureCategory::StorageExhausted,
            ),
            (
                std::io::ErrorKind::PermissionDenied,
                ProjectPersistenceFailureCategory::PermissionDenied,
            ),
        ];

        for (io_kind, expected_category) in cases {
            let root = unique_root("typed-worker-io-failure");
            std::fs::create_dir_all(&root).expect("create root");
            let (authoring, lease) = session(&root);
            let target = root.join("project.mdp");
            let sentinel = format!("canonical-before-{expected_category:?}").into_bytes();
            std::fs::write(&target, &sentinel).expect("write canonical sentinel");
            let snapshot = authoring.snapshot().expect("authoring snapshot");
            let destination =
                ManualProjectFileDestination::initial(snapshot.session_id, target.clone())
                    .expect("manual destination");
            let mut service = ProjectPersistenceService::new();
            service.fail_next_worker_io_for_test(io_kind);

            let request = service
                .submit(
                    snapshot,
                    ProjectPersistencePurpose::Manual { destination },
                    lease,
                )
                .expect("submit injected failure");
            let completion = wait_for(&service, request);

            assert!(completion.result.is_err());
            assert_eq!(
                completion.failure.as_ref().map(|failure| failure.category),
                Some(expected_category)
            );
            assert!(
                completion.publication_failure.is_none(),
                "failure before publication must not invent namespace evidence"
            );
            assert_eq!(
                std::fs::read(&target).expect("read canonical sentinel"),
                sentinel
            );
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn manual_document_revision_exhaustion_fails_before_queue_admission() {
        let root = unique_root("manual-document-revision-exhaustion");
        let (authoring, lease) = session(&root);
        let mut snapshot = authoring.snapshot().expect("authoring snapshot");
        snapshot.document.document_revision = u64::MAX;
        let target = root.join("must-not-publish.mdp");
        let destination =
            ManualProjectFileDestination::initial(snapshot.session_id, target.clone())
                .expect("manual destination");
        let mut service = ProjectPersistenceService::new();

        let error = service
            .submit(
                snapshot,
                ProjectPersistencePurpose::Manual { destination },
                lease,
            )
            .expect_err("document revision exhaustion must fail admission");

        assert!(error.contains("revision exhausted"));
        assert_eq!(service.pending_requests(), 0);
        assert!(!target.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn manual_destination_from_another_session_fails_before_queue_admission() {
        let root = unique_root("manual-destination-session");
        let (authoring, lease) = session(&root);
        let other_root = unique_root("manual-destination-other-session");
        let (other_authoring, other_lease) = session(&other_root);
        let snapshot = authoring.snapshot().expect("authoring snapshot");
        let target = root.join("must-not-publish.mdp");
        let destination =
            ManualProjectFileDestination::initial(other_authoring.session_id(), target.clone())
                .expect("foreign manual destination");
        let mut service = ProjectPersistenceService::new();

        let error = service
            .submit(
                snapshot,
                ProjectPersistencePurpose::Manual { destination },
                lease,
            )
            .expect_err("foreign Session destination must fail admission");

        assert!(error.contains("another Authoring Session"));
        assert_eq!(service.pending_requests(), 0);
        assert!(!target.exists());
        drop(other_lease);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(other_root);
    }

    #[test]
    fn manual_destination_identity_is_stable_for_one_path_and_advances_on_retarget() {
        let root = unique_root("manual-destination-identity");
        let (authoring, lease) = session(&root);
        let session_id = authoring.session_id();
        let first_path = root.join("first.mdp");
        let second_path = root.join("second.mdp");
        let first = ManualProjectFileDestination::initial(session_id, first_path.clone())
            .expect("initial destination");
        let repeated = first
            .retarget(first_path, ProjectArchivePublication::ReplaceExisting)
            .expect("repeat destination");
        let retargeted = repeated
            .retarget(
                second_path.clone(),
                ProjectArchivePublication::ReplaceExisting,
            )
            .expect("retarget destination");

        assert_eq!(repeated, first);
        assert_ne!(retargeted, first);
        assert_eq!(retargeted.session_id(), session_id);
        assert_eq!(retargeted.project_file(), second_path);
        drop(lease);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn persistence_destinations_capture_absolute_paths_at_admission() {
        let root = unique_root("absolute-destinations");
        let (authoring, lease) = session(&root);
        let session_id = authoring.session_id();
        let relative_project = PathBuf::from("relative-project.mdp");
        let relative_autosave =
            PathBuf::from("relative-runtime").join("autosave").join("recovery.autosave.mdp");
        let manual = ManualProjectFileDestination::initial(session_id, relative_project.clone())
            .expect("absolute manual destination");
        let autosave = AutosaveArchiveDestination::new(relative_autosave, relative_project.clone())
            .expect("absolute autosave destination");

        assert!(manual.project_file().is_absolute());
        assert!(autosave.archive_file().is_absolute());
        assert!(autosave.canonical_project_file().is_absolute());
        assert_eq!(
            manual.project_file(),
            std::path::absolute(relative_project).expect("expected absolute path")
        );
        drop(authoring);
        drop(lease);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn autosave_archive_precedes_repeatable_manifest_publication() {
        let root = unique_root("autosave-manifest");
        let (authoring, lease) = session(&root);
        let original = root.join("project.mdp");
        let runtime_root = authoring.runtime_root().to_path_buf();
        assert!(
            !runtime_root.join("autosave").exists(),
            "the worker must support the first autosave in a fresh runtime"
        );
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
                    ProjectPersistencePurpose::Autosave {
                        destination: AutosaveArchiveDestination::new(
                            autosave.clone(),
                            original.clone(),
                        )
                        .expect("autosave destination"),
                        max_recovery_points: 2,
                        retention_days: 7,
                        saved_at_unix_ms: saved_at_base + index,
                    },
                    Arc::clone(&lease),
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

    #[test]
    fn autosave_collision_fails_without_replacing_existing_archive_bytes() {
        let root = unique_root("autosave-create-new-collision");
        let (authoring, lease) = session(&root);
        let runtime_root = lease.runtime_root().to_path_buf();
        let target = runtime_root.join("autosave").join("collision.autosave.mdp");
        prepare_recovery_archive_target(&lease, &target).expect("prepare autosave authority");
        let sentinel = b"existing recovery archive must survive";
        std::fs::write(&target, sentinel).expect("write competing archive");

        let mut service = ProjectPersistenceService::new();
        let request_id = service
            .submit(
                authoring.snapshot().expect("authoring snapshot"),
                ProjectPersistencePurpose::Autosave {
                    destination: AutosaveArchiveDestination::new(
                        target.clone(),
                        root.join("project.mdp"),
                    )
                    .expect("autosave destination"),
                    max_recovery_points: 2,
                    retention_days: 7,
                    saved_at_unix_ms: current_unix_ms(),
                },
                lease,
            )
            .expect("submit colliding autosave");

        let completion = wait_for(&service, request_id);
        assert!(
            completion.result.is_err(),
            "autosave must retain create-only publication semantics"
        );
        assert_eq!(
            std::fs::read(&target).expect("read preserved archive"),
            sentinel
        );
        assert!(
            !runtime_root.join("autosave").join("manifest.json").exists(),
            "a rejected archive cannot become recovery authority"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn autosave_owner_mismatch_fails_before_creating_any_runtime_payload() {
        let root = unique_root("autosave-owner-mismatch");
        let (authoring, _lease) = session(&root);
        let snapshot = authoring.snapshot().expect("authoring snapshot");
        let foreign_project_id = mondrian_core::ProjectId::new();
        let foreign_project_file = root.join("foreign.mdp");
        let foreign_lease = claim_project_runtime_lease_for_test(
            &root.join("runtime-roots"),
            &foreign_project_file,
            foreign_project_id,
        )
        .expect("claim foreign runtime owner");
        let foreign_runtime_root = foreign_lease.runtime_root().to_path_buf();
        let sentinel = foreign_runtime_root.join("foreign-owner.sentinel");
        std::fs::write(&sentinel, b"must survive").expect("write foreign sentinel");
        let target = foreign_runtime_root.join("autosave").join("mismatched.autosave.mdp");
        let mut service = ProjectPersistenceService::new();
        let error = service
            .submit(
                snapshot,
                ProjectPersistencePurpose::Autosave {
                    destination: AutosaveArchiveDestination::new(
                        target.clone(),
                        root.join("project.mdp"),
                    )
                    .expect("autosave destination"),
                    max_recovery_points: 2,
                    retention_days: 7,
                    saved_at_unix_ms: 1,
                },
                foreign_lease,
            )
            .expect_err("foreign runtime must fail admission");

        assert!(error.contains("another runtime lease"));
        assert!(
            !foreign_runtime_root.join("autosave").exists(),
            "owner validation must precede autosave directory creation"
        );
        assert!(!target.exists());
        assert_eq!(
            std::fs::read(sentinel).expect("foreign sentinel survives"),
            b"must survive"
        );
    }

    #[test]
    fn quiescence_waits_for_request_arcs_and_completion_does_not_retain_the_lease() {
        let root = unique_root("quiescence-releases-request-arcs");
        let (authoring, lease) = session(&root);
        let session_id = authoring.session_id();
        let project_id = authoring.project_id();
        let project_file = authoring.project_file().to_path_buf();
        let runtime_root = lease.runtime_root().to_path_buf();
        let lease_id = lease.id();
        let library = Arc::downgrade(authoring.asset_library());
        let target = root.join("quiesced-save.mdp");
        let mut service = ProjectPersistenceService::new();
        let gate = service.gate_next_request();
        let barrier_observer = service.observe_next_barrier();
        let snapshot = authoring.snapshot().expect("snapshot");
        let request_id = service
            .submit(
                snapshot,
                ProjectPersistencePurpose::Manual {
                    destination: ManualProjectFileDestination::initial(session_id, target)
                        .expect("manual destination"),
                },
                Arc::clone(&lease),
            )
            .expect("submit gated save");
        drop(authoring);
        drop(lease);
        gate.wait_until_running();

        let error =
            lease_existing_project_runtime_for_test(&runtime_root, project_id, &project_file)
                .expect_err("running request must retain the submitted lease");
        assert!(error.contains("already leased"));
        assert!(
            library.upgrade().is_some(),
            "running request must retain its Asset Library snapshot"
        );

        let (paused_tx, paused_rx) = mpsc::sync_channel(0);
        let pause_thread = std::thread::spawn(move || {
            let pause = service.pause_and_quiesce(session_id);
            assert!(
                paused_tx.send((service, pause)).is_ok(),
                "return paused service"
            );
        });
        barrier_observer
            .recv_timeout(Duration::from_secs(10))
            .expect("barrier was not enqueued");
        assert!(
            matches!(paused_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "quiescence returned while the earlier request was still gated"
        );

        gate.release();
        let (mut service, pause) = paused_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("quiescence did not finish after releasing the request");
        pause_thread.join().expect("pause thread");
        let token = pause.expect("pause and quiesce");
        assert!(
            library.upgrade().is_none(),
            "barrier must follow destruction of the request Asset Library Arc"
        );

        let completion = service
            .poll_completions()
            .into_iter()
            .find(|completion| completion.request_id == request_id)
            .expect("completion precedes barrier");
        completion.result.as_ref().expect("save completion");
        assert_eq!(completion.runtime_lease_id, lease_id);
        assert_eq!(
            completion.persistence_generation,
            token.admission_generation
        );

        let reacquired =
            lease_existing_project_runtime_for_test(&runtime_root, project_id, &project_file)
                .expect("scalar completion must not retain the old runtime lease");
        service.retire(token).expect("retire quiesced Session");
        drop(reacquired);
        drop(completion);
        drop(service);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn asynchronous_quiescence_never_waits_on_the_calling_thread() {
        let root = unique_root("async-quiescence-ticket");
        let (authoring, lease) = session(&root);
        let session_id = authoring.session_id();
        let mut service = ProjectPersistenceService::new();
        let gate = service.gate_next_request();
        service
            .submit(
                authoring.snapshot().expect("snapshot"),
                ProjectPersistencePurpose::Manual {
                    destination: ManualProjectFileDestination::initial(
                        session_id,
                        root.join("slow-save.mdp"),
                    )
                    .expect("manual destination"),
                },
                Arc::clone(&lease),
            )
            .expect("submit gated save");
        gate.wait_until_running();

        let started = Instant::now();
        let ticket = service.begin_pause_and_quiesce(session_id).expect("begin non-blocking pause");
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "beginning a FIFO barrier must not wait for the gated worker"
        );
        assert_eq!(
            service.poll_pause_and_quiesce(&ticket).expect("poll pending barrier"),
            None
        );
        let pausing_error = service
            .submit(
                authoring.snapshot().expect("second snapshot"),
                ProjectPersistencePurpose::Manual {
                    destination: ManualProjectFileDestination::initial(
                        session_id,
                        root.join("must-not-enter.mdp"),
                    )
                    .expect("second destination"),
                },
                Arc::clone(&lease),
            )
            .expect_err("Pausing admission must reject later work");
        assert!(pausing_error.contains("closing"));

        gate.release();
        let deadline = Instant::now() + Duration::from_secs(10);
        let token = loop {
            if let Some(token) =
                service.poll_pause_and_quiesce(&ticket).expect("poll asynchronous barrier")
            {
                break token;
            }
            assert!(Instant::now() < deadline, "asynchronous barrier timed out");
            std::thread::yield_now();
        };
        assert_eq!(service.pending_requests(), 0);
        service.retire(token).expect("retire quiesced Session");

        drop(authoring);
        drop(lease);
        drop(service);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn paused_and_retired_sessions_reject_submit() {
        let root = unique_root("paused-retired-admission");
        let (authoring, lease) = session(&root);
        let other_root = unique_root("independent-admission");
        let (other_authoring, other_lease) = session(&other_root);
        let session_id = authoring.session_id();
        let snapshot = authoring.snapshot().expect("snapshot");
        let mut service = ProjectPersistenceService::new();
        let token = service.pause_and_quiesce(session_id).expect("pause empty Session");

        let paused_error = service
            .submit(
                snapshot.clone(),
                ProjectPersistencePurpose::Manual {
                    destination: ManualProjectFileDestination::initial(
                        session_id,
                        root.join("paused.mdp"),
                    )
                    .expect("paused destination"),
                },
                Arc::clone(&lease),
            )
            .expect_err("paused Session must reject submit");
        assert!(paused_error.contains("paused"));

        let other_session_id = other_authoring.session_id();
        let other_request = service
            .submit(
                other_authoring.snapshot().expect("other Session snapshot"),
                ProjectPersistencePurpose::Manual {
                    destination: ManualProjectFileDestination::initial(
                        other_session_id,
                        other_root.join("independent.mdp"),
                    )
                    .expect("other Session destination"),
                },
                Arc::clone(&other_lease),
            )
            .expect("pausing one Session must not close another Session's admission");
        let other_token = service
            .pause_and_quiesce(other_session_id)
            .expect("quiesce independent Session");
        assert!(
            service
                .poll_completions()
                .into_iter()
                .any(|completion| completion.request_id == other_request),
            "independent Session request completes before its barrier"
        );
        service.retire(other_token).expect("retire independent Session");

        service.retire(token).expect("retire paused Session");
        let retired_error = service
            .submit(
                snapshot,
                ProjectPersistencePurpose::Manual {
                    destination: ManualProjectFileDestination::initial(
                        session_id,
                        root.join("retired.mdp"),
                    )
                    .expect("retired destination"),
                },
                Arc::clone(&lease),
            )
            .expect_err("retired Session must reject submit");
        assert!(retired_error.contains("retired"));
        assert_eq!(service.pending_requests(), 0);

        drop(authoring);
        drop(lease);
        drop(other_authoring);
        drop(other_lease);
        drop(service);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(other_root);
    }

    #[test]
    fn only_the_exact_current_pause_token_can_resume_or_retire() {
        let root = unique_root("exact-pause-token");
        let (authoring, lease) = session(&root);
        let session_id = authoring.session_id();
        let mut service = ProjectPersistenceService::new();
        let first_request = service
            .submit(
                authoring.snapshot().expect("first-generation snapshot"),
                ProjectPersistencePurpose::Manual {
                    destination: ManualProjectFileDestination::initial(
                        session_id,
                        root.join("first-generation.mdp"),
                    )
                    .expect("first-generation destination"),
                },
                Arc::clone(&lease),
            )
            .expect("submit first-generation request");
        let first = service.pause_and_quiesce(session_id).expect("first pause");
        let first_completion = service
            .poll_completions()
            .into_iter()
            .find(|completion| completion.request_id == first_request)
            .expect("first-generation completion precedes barrier");
        assert!(service.accepts_completion(&first_completion));
        service.resume(first).expect("resume first generation");
        assert!(
            !service.accepts_completion(&first_completion),
            "a completion retained across resume must not enter the new persistence generation"
        );
        let current = service.pause_and_quiesce(session_id).expect("second pause");

        assert!(
            service.resume(first).expect_err("old token must be stale").contains("stale"),
            "old generation must not resume the current pause"
        );
        assert!(
            service.retire(first).expect_err("old token must not retire").contains("stale"),
            "old generation must not retire the current pause"
        );

        let mut other_service = ProjectPersistenceService::new();
        let foreign = other_service.pause_and_quiesce(session_id).expect("foreign service pause");
        assert!(service
            .resume(foreign)
            .expect_err("foreign token must fail")
            .contains("another persistence service"));

        service.resume(current).expect("exact token reopens admission");
        let request_id = service
            .submit(
                authoring.snapshot().expect("snapshot after resume"),
                ProjectPersistencePurpose::Manual {
                    destination: ManualProjectFileDestination::initial(
                        session_id,
                        root.join("resumed.mdp"),
                    )
                    .expect("resumed destination"),
                },
                Arc::clone(&lease),
            )
            .expect("resumed Session admits submit");
        let final_token = service.pause_and_quiesce(session_id).expect("drain resumed generation");
        let completion = service
            .poll_completions()
            .into_iter()
            .find(|completion| completion.request_id == request_id)
            .expect("resumed request completed before barrier");
        assert_eq!(
            completion.persistence_generation,
            final_token.admission_generation
        );
        assert!(service.accepts_completion(&completion));
        service.retire(final_token).expect("retire final generation");
        assert!(!service.accepts_completion(&completion));
        other_service.retire(foreign).expect("retire foreign pause");

        drop(completion);
        drop(authoring);
        drop(lease);
        drop(service);
        drop(other_service);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn timed_out_barrier_poisons_session_admission() {
        let root = unique_root("barrier-timeout-poisons");
        let (authoring, lease) = session(&root);
        let session_id = authoring.session_id();
        let mut service = ProjectPersistenceService::new();
        let gate = service.gate_next_request();
        service
            .submit(
                authoring.snapshot().expect("snapshot"),
                ProjectPersistencePurpose::Manual {
                    destination: ManualProjectFileDestination::initial(
                        session_id,
                        root.join("timed-out.mdp"),
                    )
                    .expect("manual destination"),
                },
                Arc::clone(&lease),
            )
            .expect("submit gated request");
        gate.wait_until_running();

        let error = service
            .pause_and_quiesce_with_timeout(session_id, Duration::ZERO)
            .expect_err("gated request prevents immediate barrier");
        assert!(error.contains("timed out"));
        let poisoned_error = service
            .submit(
                authoring.snapshot().expect("snapshot after timeout"),
                ProjectPersistencePurpose::Manual {
                    destination: ManualProjectFileDestination::initial(
                        session_id,
                        root.join("must-reject.mdp"),
                    )
                    .expect("rejected destination"),
                },
                Arc::clone(&lease),
            )
            .expect_err("timed-out barrier must leave admission closed");
        assert!(poisoned_error.contains("poisoned"));

        service
            .abandon_session(session_id)
            .expect("explicit abandon retires poisoned admission");
        gate.release();
        let completion = service
            .completion_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("gated request completes after release");
        assert!(
            !service.accepts_completion(&completion),
            "abandoned Session completion must never regain publication authority"
        );
        drop(authoring);
        drop(lease);
        drop(service);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn timed_out_asynchronous_barrier_fails_closed_without_waiting() {
        let root = unique_root("async-barrier-timeout-poisons");
        let (authoring, lease) = session(&root);
        let session_id = authoring.session_id();
        let mut service = ProjectPersistenceService::new();
        let gate = service.gate_next_request();
        service
            .submit(
                authoring.snapshot().expect("snapshot"),
                ProjectPersistencePurpose::Manual {
                    destination: ManualProjectFileDestination::initial(
                        session_id,
                        root.join("timed-out.mdp"),
                    )
                    .expect("manual destination"),
                },
                Arc::clone(&lease),
            )
            .expect("submit gated request");
        gate.wait_until_running();

        let ticket = service
            .begin_pause_and_quiesce_with_timeout(session_id, Duration::ZERO)
            .expect("begin asynchronous barrier");
        let started = Instant::now();
        let error = service
            .poll_pause_and_quiesce(&ticket)
            .expect_err("zero-budget asynchronous barrier must time out");
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(error.contains("timed out"));
        let poisoned_error = service
            .submit(
                authoring.snapshot().expect("snapshot after timeout"),
                ProjectPersistencePurpose::Manual {
                    destination: ManualProjectFileDestination::initial(
                        session_id,
                        root.join("must-reject.mdp"),
                    )
                    .expect("rejected destination"),
                },
                Arc::clone(&lease),
            )
            .expect_err("timed-out asynchronous barrier must poison admission");
        assert!(poisoned_error.contains("poisoned"));

        gate.release();
        service
            .completion_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("gated request completes after release");
        drop(authoring);
        drop(lease);
        drop(service);
        let _ = std::fs::remove_dir_all(root);
    }
}
