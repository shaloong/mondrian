use super::*;
use mondrian_core::{MediaFileFingerprint, MediaProbeSnapshot};

struct FakeBackend {
    started: Mutex<Vec<PathBuf>>,
    started_changed: Condvar,
    permits: Mutex<usize>,
    permit_changed: Condvar,
    honor_cancellation: bool,
    preparations: AtomicU64,
    commits: AtomicU64,
}

impl FakeBackend {
    fn new(honor_cancellation: bool) -> Arc<Self> {
        Arc::new(Self {
            started: Mutex::new(Vec::new()),
            started_changed: Condvar::new(),
            permits: Mutex::new(0),
            permit_changed: Condvar::new(),
            honor_cancellation,
            preparations: AtomicU64::new(0),
            commits: AtomicU64::new(0),
        })
    }

    fn release(&self, count: usize) {
        *self.permits.lock() += count;
        self.permit_changed.notify_all();
    }

    fn wait_started(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut started = self.started.lock();
        while started.len() < count {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "import worker did not start");
            self.started_changed.wait_for(&mut started, remaining);
        }
    }
}

impl MediaImportPreparationBackend for FakeBackend {
    fn prepare(
        &self,
        path: &Path,
        folder_id: Option<&str>,
        cancellation: &ExecutionCancellationToken,
    ) -> MediaImportWorkerOutcome {
        self.started.lock().push(path.to_path_buf());
        self.started_changed.notify_all();
        let mut permits = self.permits.lock();
        while *permits == 0 && (!self.honor_cancellation || !cancellation.is_canceled()) {
            self.permit_changed.wait_for(&mut permits, Duration::from_millis(5));
        }
        if self.honor_cancellation && cancellation.is_canceled() {
            return MediaImportWorkerOutcome::Canceled;
        }
        *permits = permits.saturating_sub(1);
        self.preparations.fetch_add(1, Ordering::AcqRel);
        MediaImportWorkerOutcome::Prepared(Box::new(MediaImportPreparedCandidate {
            canonical_path: path.to_path_buf(),
            source_fingerprint: MediaFileFingerprint::default(),
            info: MediaProbeSnapshot {
                duration: Duration::ZERO,
                file_size: 0,
                container: "test".to_owned(),
                video_streams: Vec::new(),
                audio_streams: Vec::new(),
                has_video: false,
                has_audio: false,
            },
            folder_id: folder_id.map(str::to_owned),
        }))
    }
}

impl MediaImportCommitBackend for FakeBackend {
    fn commit(
        &self,
        _library: &AssetLibrary,
        _candidate: MediaImportPreparedCandidate,
    ) -> MediaImportPublicationOutcome {
        self.commits.fetch_add(1, Ordering::AcqRel);
        MediaImportPublicationOutcome::Imported(AssetId::new())
    }
}

struct BlockingCommitBackend {
    commit_started: Mutex<bool>,
    commit_started_changed: Condvar,
    commit_permitted: Mutex<bool>,
    commit_permitted_changed: Condvar,
}

impl BlockingCommitBackend {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            commit_started: Mutex::new(false),
            commit_started_changed: Condvar::new(),
            commit_permitted: Mutex::new(false),
            commit_permitted_changed: Condvar::new(),
        })
    }

    fn wait_commit_started(&self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut started = self.commit_started.lock();
        while !*started {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "import publication did not start");
            self.commit_started_changed.wait_for(&mut started, remaining);
        }
    }

    fn release_commit(&self) {
        *self.commit_permitted.lock() = true;
        self.commit_permitted_changed.notify_all();
    }
}

impl MediaImportPreparationBackend for BlockingCommitBackend {
    fn prepare(
        &self,
        path: &Path,
        folder_id: Option<&str>,
        _cancellation: &ExecutionCancellationToken,
    ) -> MediaImportWorkerOutcome {
        MediaImportWorkerOutcome::Prepared(Box::new(MediaImportPreparedCandidate {
            canonical_path: path.to_path_buf(),
            source_fingerprint: MediaFileFingerprint::default(),
            info: MediaProbeSnapshot {
                duration: Duration::ZERO,
                file_size: 0,
                container: "test".to_owned(),
                video_streams: Vec::new(),
                audio_streams: Vec::new(),
                has_video: false,
                has_audio: false,
            },
            folder_id: folder_id.map(str::to_owned),
        }))
    }
}

impl MediaImportCommitBackend for BlockingCommitBackend {
    fn commit(
        &self,
        _library: &AssetLibrary,
        _candidate: MediaImportPreparedCandidate,
    ) -> MediaImportPublicationOutcome {
        *self.commit_started.lock() = true;
        self.commit_started_changed.notify_all();
        let mut permitted = self.commit_permitted.lock();
        while !*permitted {
            self.commit_permitted_changed.wait(&mut permitted);
        }
        MediaImportPublicationOutcome::Imported(AssetId::new())
    }
}

fn library() -> Arc<AssetLibrary> {
    let root = std::env::temp_dir().join(format!("mondrian-import-service-{}", AssetId::new()));
    AssetLibrary::open(root).expect("test asset library")
}

fn paths(seed: usize, count: usize) -> Vec<PathBuf> {
    (0..count)
        .map(|index| PathBuf::from(format!("import-{seed}-{index}.mov")))
        .collect()
}

fn wait_publications(
    execution: &MediaImportExecution,
    count: usize,
) -> Vec<MediaImportPublication> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut results = Vec::new();
    let library = library();
    while results.len() < count {
        results.extend(execution.poll_results(Some(&library), count - results.len()));
        assert!(Instant::now() < deadline, "import result timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
    results
}

#[test]
fn sqlite_publication_does_not_hold_execution_state_lock() {
    let backend = BlockingCommitBackend::new();
    let execution = Arc::new(MediaImportExecution::with_backend(1, backend.clone()));
    execution.bind_project(Some(ProjectId::new()));
    execution.admit_batch(paths(200, 1), None).expect("admit publication probe");
    let library = library();
    let poll_execution = Arc::clone(&execution);
    let poll_library = Arc::clone(&library);
    let poller = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let publications = poll_execution.poll_results(Some(&poll_library), 1);
            if !publications.is_empty() {
                return publications;
            }
            assert!(
                Instant::now() < deadline,
                "import result did not reach publication"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    });

    backend.wait_commit_started();
    let observed_at = Instant::now();
    let diagnostics = execution.diagnostics();
    assert!(
        observed_at.elapsed() < Duration::from_millis(100),
        "SQLite publication held the execution-state lock"
    );
    assert_eq!(diagnostics.running_files, 0);
    execution.set_resource_policy(false, 1);

    backend.release_commit();
    let publications = poller.join().expect("publication poller");
    assert_eq!(publications.len(), 1);
    assert_eq!(
        publications[0].evidence.disposition,
        ExecutionTerminalDisposition::Completed
    );
}

#[test]
fn paused_execution_retains_explicit_batch_and_resumes() {
    let backend = FakeBackend::new(true);
    let execution = MediaImportExecution::with_backend(2, backend.clone());
    execution.bind_project(Some(ProjectId::new()));
    execution.set_resource_policy(false, 1);
    let admission = execution.admit_batch(paths(1, 2), None).expect("admit paused batch");
    std::thread::sleep(Duration::from_millis(20));
    assert!(backend.started.lock().is_empty());
    assert_eq!(execution.diagnostics().queued_files, 2);

    execution.set_resource_policy(true, 1);
    backend.release(2);
    backend.wait_started(2);
    let results = wait_publications(&execution, admission.total);
    assert!(results
        .iter()
        .all(|result| result.evidence.disposition == ExecutionTerminalDisposition::Completed));
}

#[test]
fn resource_policy_changes_only_complete_diagnostics_revision() {
    let backend = FakeBackend::new(true);
    let execution = MediaImportExecution::with_backend(2, backend);
    assert!(!execution.poll_model_changed());
    let initial = execution.diagnostics();

    execution.set_resource_policy(false, 1);

    let paused = execution.diagnostics();
    assert_ne!(paused.diagnostics_revision, initial.diagnostics_revision);
    assert_eq!(paused.model_revision, initial.model_revision);
    assert!(!paused.dispatch_enabled);
    assert_eq!(paused.dispatch_parallelism, 1);
    assert!(!execution.poll_model_changed());

    execution.set_resource_policy(false, 1);
    let unchanged = execution.diagnostics();
    assert_eq!(unchanged.diagnostics_revision, paused.diagnostics_revision);
    assert_eq!(unchanged.model_revision, paused.model_revision);
}

#[test]
fn app_poll_ignores_policy_only_diagnostics_change() {
    let backend = FakeBackend::new(true);
    let mut state = crate::app::AppState::new();
    state.media_import = MediaImportExecution::with_backend(1, backend);
    assert!(!state.poll_media_imports());

    state.media_import.set_resource_policy(false, 1);

    assert!(!state.poll_media_imports());
    assert!(!state.media_import_diagnostics().dispatch_enabled);
}

#[test]
fn batch_publication_and_terminal_remain_product_model_changes() {
    let backend = FakeBackend::new(true);
    let execution = MediaImportExecution::with_backend(1, backend.clone());
    execution.bind_project(Some(ProjectId::new()));
    assert!(execution.poll_model_changed());
    assert!(!execution.poll_model_changed());

    execution.set_resource_policy(false, 1);
    assert!(!execution.poll_model_changed());
    let before_admission = execution.diagnostics();
    let admission = execution.admit_batch(paths(4, 1), None).expect("admit batch");
    let admitted = execution.diagnostics();
    assert_ne!(admitted.model_revision, before_admission.model_revision);
    assert!(execution.poll_model_changed());
    assert!(!execution.poll_model_changed());

    execution.set_resource_policy(true, 1);
    assert!(!execution.poll_model_changed());
    backend.wait_started(1);
    assert!(!execution.poll_model_changed());
    backend.release(1);
    let publications = wait_publications(&execution, admission.total);
    assert_eq!(
        publications[0].evidence.disposition,
        ExecutionTerminalDisposition::Completed
    );
    assert!(execution.poll_model_changed());
    assert!(!execution.poll_model_changed());

    let completed = execution.diagnostics();
    assert_eq!(completed.active_batches, 0);
    assert_eq!(completed.imported_files, 1);
    assert!(completed.terminal_records.iter().any(|terminal| {
        terminal.batch_id == admission.batch_id
            && terminal.evidence.disposition == ExecutionTerminalDisposition::Completed
    }));
}

#[test]
fn rejected_admission_publishes_product_terminal_change() {
    let backend = FakeBackend::new(true);
    let execution = MediaImportExecution::with_backend(1, backend);
    assert!(!execution.poll_model_changed());

    assert!(matches!(
        execution.admit_batch(paths(5, 1), None),
        Err(MediaImportAdmissionError::ProjectUnavailable)
    ));

    assert!(execution.poll_model_changed());
    assert!(!execution.poll_model_changed());
    let diagnostics = execution.diagnostics();
    assert_eq!(diagnostics.rejections, 1);
    assert!(diagnostics.terminal_records.iter().any(|terminal| {
        terminal.evidence.disposition == ExecutionTerminalDisposition::Rejected
            && terminal.failure == Some(MediaImportFailureReason::AdmissionRejected)
    }));
}

#[test]
fn concurrent_batches_share_fixed_workers_without_cross_batch_results() {
    let backend = FakeBackend::new(true);
    let execution = MediaImportExecution::with_backend(2, backend.clone());
    execution.bind_project(Some(ProjectId::new()));
    let first = execution.admit_batch(paths(10, 1), None).expect("admit first batch");
    let second = execution.admit_batch(paths(11, 1), None).expect("admit second batch");
    backend.wait_started(2);
    assert_eq!(execution.diagnostics().running_files, 2);

    backend.release(2);
    let results = wait_publications(&execution, 2);
    assert!(results.iter().any(|result| result.batch_id == first.batch_id));
    assert!(results.iter().any(|result| result.batch_id == second.batch_id));
    assert_eq!(execution.diagnostics().active_batches, 0);
}

#[test]
fn batch_and_file_capacity_reject_without_unbounded_growth() {
    let backend = FakeBackend::new(true);
    let execution = MediaImportExecution::with_backend(1, backend);
    execution.bind_project(Some(ProjectId::new()));
    execution.set_resource_policy(false, 1);
    for seed in 0..MEDIA_IMPORT_BATCH_CAPACITY {
        execution.admit_batch(paths(seed, 1), None).expect("fill batch capacity");
    }
    assert!(matches!(
        execution.admit_batch(paths(99, 1), None),
        Err(MediaImportAdmissionError::BatchCapacityExceeded { .. })
    ));
    assert!(matches!(
        execution.admit_batch(paths(100, MEDIA_IMPORT_FILES_PER_BATCH_CAPACITY + 1), None),
        Err(MediaImportAdmissionError::BatchTooLarge { .. })
    ));
    let diagnostics = execution.diagnostics();
    assert_eq!(diagnostics.active_batches, MEDIA_IMPORT_BATCH_CAPACITY);
    assert_eq!(diagnostics.rejections, 2);
}

#[test]
fn empty_batch_rejects_without_queue_or_batch_state() {
    let backend = FakeBackend::new(true);
    let execution = MediaImportExecution::with_backend(1, backend.clone());
    execution.bind_project(Some(ProjectId::new()));

    assert!(matches!(
        execution.admit_batch(Vec::new(), None),
        Err(MediaImportAdmissionError::EmptyBatch)
    ));
    let diagnostics = execution.diagnostics();
    assert_eq!(diagnostics.active_batches, 0);
    assert_eq!(diagnostics.queued_files, 0);
    assert_eq!(diagnostics.outstanding_files, 0);
    assert_eq!(diagnostics.rejections, 1);
    assert!(backend.started.lock().is_empty());
}

#[test]
fn missing_worker_rejects_before_queue_admission() {
    let backend = FakeBackend::new(true);
    let execution = MediaImportExecution::with_backend(0, backend.clone());
    execution.bind_project(Some(ProjectId::new()));

    assert!(matches!(
        execution.admit_batch(paths(70, 1), None),
        Err(MediaImportAdmissionError::WorkerUnavailable)
    ));
    let diagnostics = execution.diagnostics();
    assert_eq!(diagnostics.active_batches, 0);
    assert_eq!(diagnostics.transport_occupied_files, 0);
    assert_eq!(diagnostics.rejections, 1);
    assert!(backend.started.lock().is_empty());
}

#[test]
fn total_outstanding_capacity_rejects_without_partial_admission() {
    let backend = FakeBackend::new(true);
    let execution = MediaImportExecution::with_backend(1, backend.clone());
    execution.bind_project(Some(ProjectId::new()));
    execution.set_resource_policy(false, 1);
    execution
        .admit_batch(paths(71, MEDIA_IMPORT_FILES_PER_BATCH_CAPACITY), None)
        .expect("admit first capacity batch");
    execution
        .admit_batch(paths(72, MEDIA_IMPORT_FILES_PER_BATCH_CAPACITY), None)
        .expect("admit second capacity batch");

    assert!(matches!(
        execution.admit_batch(paths(73, 1), None),
        Err(MediaImportAdmissionError::FileCapacityExceeded { files: 1, available: 0 })
    ));
    let diagnostics = execution.diagnostics();
    assert_eq!(diagnostics.active_batches, 2);
    assert_eq!(
        diagnostics.queued_files,
        MEDIA_IMPORT_OUTSTANDING_FILE_CAPACITY
    );
    assert_eq!(
        diagnostics.transport_occupied_files,
        MEDIA_IMPORT_OUTSTANDING_FILE_CAPACITY
    );
    assert_eq!(diagnostics.rejections, 1);
    assert!(backend.started.lock().is_empty());
}

#[test]
fn unbound_execution_rejects_before_worker_or_queue_admission() {
    let backend = FakeBackend::new(true);
    let execution = MediaImportExecution::with_backend(1, backend.clone());
    assert!(matches!(
        execution.admit_batch(paths(1, 1), None),
        Err(MediaImportAdmissionError::ProjectUnavailable)
    ));
    let diagnostics = execution.diagnostics();
    assert_eq!(diagnostics.active_batches, 0);
    assert_eq!(diagnostics.outstanding_files, 0);
    assert_eq!(diagnostics.rejections, 1);
    assert!(backend.started.lock().is_empty());
}

#[test]
fn queued_cancellation_is_terminal_without_worker_dispatch() {
    let backend = FakeBackend::new(true);
    let execution = MediaImportExecution::with_backend(1, backend.clone());
    execution.bind_project(Some(ProjectId::new()));
    execution.set_resource_policy(false, 1);
    let admission = execution.admit_batch(paths(2, 3), None).expect("admit canceled batch");

    assert!(execution.cancel_batch(MediaImportBatchId(admission.batch_id)));
    assert!(backend.started.lock().is_empty());
    let diagnostics = execution.diagnostics();
    assert_eq!(diagnostics.active_batches, 0);
    assert_eq!(diagnostics.canceled_files, 3);
    assert_eq!(diagnostics.outstanding_files, 0);
    assert_eq!(
        diagnostics
            .terminal_records
            .iter()
            .filter(|record| {
                record.evidence.disposition == ExecutionTerminalDisposition::Canceled
            })
            .count(),
        3
    );
    assert!(diagnostics.terminal_records.iter().all(|record| {
        record.batch_id == admission.batch_id
            && record.path.is_some()
            && record.elapsed.is_zero()
            && record.failure == Some(MediaImportFailureReason::Canceled)
    }));
}

#[test]
fn running_cancellation_observes_token_and_terminates_batch() {
    let backend = FakeBackend::new(true);
    let execution = MediaImportExecution::with_backend(1, backend.clone());
    execution.bind_project(Some(ProjectId::new()));
    let admission = execution.admit_batch(paths(20, 1), None).expect("admit running batch");
    backend.wait_started(1);

    assert!(execution.cancel_batch(MediaImportBatchId(admission.batch_id)));
    let results = wait_publications(&execution, 1);
    assert_eq!(
        results[0].evidence.disposition,
        ExecutionTerminalDisposition::Canceled
    );
    let diagnostics = execution.diagnostics();
    assert_eq!(diagnostics.active_batches, 0);
    assert_eq!(diagnostics.canceled_files, 1);
    assert_eq!(backend.commits.load(Ordering::Acquire), 0);
}

#[test]
fn cancellation_after_prepare_still_prevents_serialized_commit() {
    let backend = FakeBackend::new(false);
    let execution = MediaImportExecution::with_backend(1, backend.clone());
    execution.bind_project(Some(ProjectId::new()));
    let admission = execution.admit_batch(paths(21, 1), None).expect("admit batch");
    backend.wait_started(1);
    backend.release(1);
    let deadline = Instant::now() + Duration::from_secs(2);
    while execution.diagnostics().running_files != 0 {
        assert!(
            Instant::now() < deadline,
            "import preparation did not finish"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    assert!(execution.cancel_batch(MediaImportBatchId(admission.batch_id)));
    let results = wait_publications(&execution, 1);
    assert_eq!(
        results[0].evidence.disposition,
        ExecutionTerminalDisposition::Canceled
    );
    assert_eq!(backend.commits.load(Ordering::Acquire), 0);
}

#[test]
fn old_project_completion_is_superseded_and_cannot_publish() {
    let backend = FakeBackend::new(false);
    let execution = MediaImportExecution::with_backend(1, backend.clone());
    let project_id = ProjectId::new();
    execution.bind_project(Some(project_id));
    let admission = execution.admit_batch(paths(3, 1), None).expect("admit old batch");
    backend.wait_started(1);

    // Reopening/replacing the same Project identity still installs a new
    // library binding and must rotate execution generation.
    execution.bind_project(Some(project_id));
    let rotated = execution.diagnostics();
    assert_eq!(rotated.outstanding_files, 0);
    assert_eq!(rotated.transport_occupied_files, 1);
    backend.release(1);
    let results = wait_publications(&execution, admission.total);
    assert_eq!(
        results[0].evidence.disposition,
        ExecutionTerminalDisposition::Superseded
    );
    assert_eq!(execution.diagnostics().superseded_files, 1);
    assert_eq!(execution.diagnostics().transport_occupied_files, 0);
    assert_eq!(backend.commits.load(Ordering::Acquire), 0);
}

#[test]
fn terminal_diagnostics_evict_oldest_records_at_the_fixed_capacity() {
    let backend = FakeBackend::new(true);
    let execution = MediaImportExecution::with_backend(1, backend);
    for _ in 0..=MEDIA_IMPORT_TERMINAL_CAPACITY {
        assert!(matches!(
            execution.admit_batch(paths(74, 1), None),
            Err(MediaImportAdmissionError::ProjectUnavailable)
        ));
    }

    let diagnostics = execution.diagnostics();
    assert_eq!(
        diagnostics.rejections,
        (MEDIA_IMPORT_TERMINAL_CAPACITY + 1) as u64
    );
    assert_eq!(
        diagnostics.terminal_records.len(),
        MEDIA_IMPORT_TERMINAL_CAPACITY
    );
    assert_eq!(diagnostics.terminal_records[0].batch_id, 2);
    assert_eq!(
        diagnostics.terminal_records.last().map(|record| record.batch_id),
        Some((MEDIA_IMPORT_TERMINAL_CAPACITY + 1) as u64)
    );
}

#[test]
fn project_generation_rotates_at_counter_exhaustion() {
    let backend = FakeBackend::new(true);
    let execution = MediaImportExecution::with_backend(1, backend);
    execution.inner.state.lock().generation = u64::MAX;

    execution.bind_project(Some(ProjectId::new()));

    assert_eq!(execution.diagnostics().generation, 1);
}

#[test]
fn shutdown_is_bounded_when_foreign_probe_ignores_cancellation() {
    let backend = FakeBackend::new(false);
    let execution = MediaImportExecution::with_backend(1, backend.clone());
    execution.bind_project(Some(ProjectId::new()));
    execution.admit_batch(paths(30, 1), None).expect("admit blocking batch");
    backend.wait_started(1);

    let started = Instant::now();
    drop(execution);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "shutdown exceeded its bounded grace"
    );
    assert_eq!(backend.commits.load(Ordering::Acquire), 0);

    backend.release(1);
    let deadline = Instant::now() + Duration::from_secs(2);
    while backend.preparations.load(Ordering::Acquire) == 0 {
        assert!(
            Instant::now() < deadline,
            "detached preparation did not exit"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(backend.commits.load(Ordering::Acquire), 0);
}
