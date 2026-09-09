use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mondrian_core::ExecutionTerminalDisposition;
use mondrian_timeline::sequence::{DeliveryBitDepth, Sequence};
use parking_lot::{Condvar, Mutex};

use super::*;
use crate::preset::{
    ExportOutputPolicy, ExportParameter, ExportPreset, TimelineExportRange, TimelineExportSnapshot,
};
use crate::queue::{
    DurableExportPublication, ExportExecutor, ExportJobDiagnostics, ExportPublicationFailure,
    JobExecutionResult,
};

enum GateOutcome {
    Complete,
    Fail(String),
    AudioOwnerClean,
    AudioOwnerDirty,
    AudioOwnerUnclosed,
    PublicationBeforeNamespace,
    PublicationDurabilityUnconfirmed,
    PublicationNamespaceIndeterminate,
    Panic,
    OpaquePanic(Arc<AtomicBool>),
}

struct GateExecutor {
    outcomes: Mutex<VecDeque<GateOutcome>>,
    honor_cancellation: bool,
    started: Mutex<usize>,
    started_changed: Condvar,
    finished: Mutex<usize>,
    finished_changed: Condvar,
    permits: Mutex<usize>,
    permit_changed: Condvar,
}

struct DropProbeExecutor {
    dropped: Arc<AtomicBool>,
}

struct BlockingDropExecutor {
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
}

struct PanicDropProbe {
    dropped: Arc<AtomicBool>,
}

#[derive(Debug)]
struct IoErrorDropProbe {
    dropped: Arc<AtomicBool>,
}

impl Drop for PanicDropProbe {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}

impl std::fmt::Display for IoErrorDropProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("synthetic foreign spawn error")
    }
}

impl std::error::Error for IoErrorDropProbe {}

impl Drop for IoErrorDropProbe {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}

impl Drop for DropProbeExecutor {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}

impl ExportExecutor for DropProbeExecutor {
    fn execute(
        &self,
        _job: &RenderJob,
        _cancellation: &ExecutionCancellationToken,
        _execution_gate: &ExportExecutionGate,
        _report: &mut dyn FnMut(ExportProgress),
        _report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
        _report_owner: &mut dyn FnMut(super::ExportExecutionOwnerEvent),
    ) -> JobExecutionResult {
        panic!("spawn-failure executor must never execute")
    }
}

impl Drop for BlockingDropExecutor {
    fn drop(&mut self) {
        self.entered.store(true, Ordering::Release);
        while !self.release.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }
}

impl ExportExecutor for BlockingDropExecutor {
    fn execute(
        &self,
        _job: &RenderJob,
        _cancellation: &ExecutionCancellationToken,
        _execution_gate: &ExportExecutionGate,
        _report: &mut dyn FnMut(ExportProgress),
        _report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
        _report_owner: &mut dyn FnMut(super::ExportExecutionOwnerEvent),
    ) -> JobExecutionResult {
        panic!("destructor-order executor must never execute")
    }
}

impl GateExecutor {
    fn new(outcomes: impl IntoIterator<Item = GateOutcome>) -> Arc<Self> {
        Arc::new(Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            honor_cancellation: true,
            started: Mutex::new(0),
            started_changed: Condvar::new(),
            finished: Mutex::new(0),
            finished_changed: Condvar::new(),
            permits: Mutex::new(0),
            permit_changed: Condvar::new(),
        })
    }

    fn committed(outcomes: impl IntoIterator<Item = GateOutcome>) -> Arc<Self> {
        Arc::new(Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            honor_cancellation: false,
            started: Mutex::new(0),
            started_changed: Condvar::new(),
            finished: Mutex::new(0),
            finished_changed: Condvar::new(),
            permits: Mutex::new(0),
            permit_changed: Condvar::new(),
        })
    }

    fn release(&self, count: usize) {
        let mut permits = self.permits.lock();
        *permits = permits.saturating_add(count);
        self.permit_changed.notify_all();
    }

    fn wait_started(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut started = self.started.lock();
        while *started < count {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "export worker did not start expected job"
            );
            self.started_changed.wait_for(&mut started, remaining);
        }
    }

    fn wait_finished(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut finished = self.finished.lock();
        while *finished < count {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "export worker did not finish expected job"
            );
            self.finished_changed.wait_for(&mut finished, remaining);
        }
    }
}

struct GateExecutionFinished<'a>(&'a GateExecutor);

impl Drop for GateExecutionFinished<'_> {
    fn drop(&mut self) {
        *self.0.finished.lock() += 1;
        self.0.finished_changed.notify_all();
    }
}

impl ExportExecutor for GateExecutor {
    fn execute(
        &self,
        job: &RenderJob,
        cancellation: &ExecutionCancellationToken,
        execution_gate: &ExportExecutionGate,
        report: &mut dyn FnMut(ExportProgress),
        _report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
        report_owner: &mut dyn FnMut(super::ExportExecutionOwnerEvent),
    ) -> JobExecutionResult {
        let _finished = GateExecutionFinished(self);
        report(ExportProgress::preparing(0.1));
        if !self.honor_cancellation
            && !execution_gate.wait_at_boundary(ExportProgressPhase::Publishing, cancellation)
        {
            return JobExecutionResult::Cancelled;
        }
        *self.started.lock() += 1;
        self.started_changed.notify_all();
        loop {
            if self.honor_cancellation
                && !execution_gate.wait_at_boundary(ExportProgressPhase::Preparing, cancellation)
            {
                return JobExecutionResult::Cancelled;
            }
            let mut permits = self.permits.lock();
            if *permits > 0 {
                *permits -= 1;
                break;
            }
            self.permit_changed.wait_for(&mut permits, Duration::from_millis(5));
        }
        match self.outcomes.lock().pop_front().unwrap_or(GateOutcome::Complete) {
            GateOutcome::Complete => {
                if cancellation.is_canceled() {
                    JobExecutionResult::Cancelled
                } else if execution_gate
                    .wait_at_boundary(ExportProgressPhase::Publishing, cancellation)
                {
                    JobExecutionResult::Published(DurableExportPublication::synthetic(
                        &job.config.output_path,
                    ))
                } else {
                    JobExecutionResult::Cancelled
                }
            }
            GateOutcome::Fail(detail) => JobExecutionResult::Failed(detail),
            GateOutcome::AudioOwnerClean => {
                report_owner(super::ExportExecutionOwnerEvent::AudioSourceStarted);
                report_owner(super::ExportExecutionOwnerEvent::AudioSourceClosed {
                    all_resources_released: true,
                });
                JobExecutionResult::Failed("synthetic owner lifecycle completed".to_owned())
            }
            GateOutcome::AudioOwnerDirty => {
                report_owner(super::ExportExecutionOwnerEvent::AudioSourceStarted);
                report_owner(super::ExportExecutionOwnerEvent::AudioSourceClosed {
                    all_resources_released: false,
                });
                JobExecutionResult::Failed("synthetic dirty owner lifecycle".to_owned())
            }
            GateOutcome::AudioOwnerUnclosed => {
                report_owner(super::ExportExecutionOwnerEvent::AudioSourceStarted);
                JobExecutionResult::Failed("synthetic unclosed owner lifecycle".to_owned())
            }
            GateOutcome::PublicationBeforeNamespace => {
                JobExecutionResult::PublicationFailed(ExportPublicationFailure::BeforeNamespace {
                    output_path: job.config.output_path.clone(),
                    retained_partial_path: Some(
                        job.config.output_path.with_extension("validated.partial"),
                    ),
                    detail: "publication stopped before namespace mutation".to_owned(),
                })
            }
            GateOutcome::PublicationDurabilityUnconfirmed => JobExecutionResult::PublicationFailed(
                ExportPublicationFailure::DurabilityUnconfirmed {
                    output_path: job.config.output_path.clone(),
                    detail: "directory durability was not confirmed".to_owned(),
                },
            ),
            GateOutcome::PublicationNamespaceIndeterminate => {
                JobExecutionResult::PublicationFailed(
                    ExportPublicationFailure::NamespaceIndeterminate {
                        output_path: job.config.output_path.clone(),
                        retained_partial_path: Some(
                            job.config.output_path.with_extension("retained.partial"),
                        ),
                        detail: "namespace postcondition is indeterminate".to_owned(),
                    },
                )
            }
            GateOutcome::Panic => panic!("synthetic export executor panic"),
            GateOutcome::OpaquePanic(dropped) => std::panic::panic_any(PanicDropProbe { dropped }),
        }
    }
}

fn dummy_config(output_path: impl Into<PathBuf>) -> ExportConfig {
    ExportConfig {
        preset: ExportPreset::h264_aac_sdr_1080p(),
        timeline: Box::new(TimelineExportSnapshot {
            sequence: Sequence::new("queue-test"),
            sequences: Vec::new(),
            media: HashMap::new(),
            color_environment: mondrian_core::ProjectColorEnvironment::default(),
            prepared_execution: None,
            range: TimelineExportRange::EntireSequence,
        }),
        output_path: output_path.into(),
        output_policy: ExportOutputPolicy::CreateNew,
        smart_render: crate::preset::ExportSmartRenderPolicy::Automatic,
        broadcast_qc: None,
        regulatory_pse: None,
        frozen_ancillary: None,
        approved_bmx: None,
    }
}

fn wait_diagnostics(
    queue: &RenderQueue,
    predicate: impl Fn(&ExportQueueDiagnostics) -> bool,
) -> ExportQueueDiagnostics {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let diagnostics = queue.diagnostics();
        if predicate(&diagnostics) {
            return diagnostics;
        }
        assert!(
            Instant::now() < deadline,
            "export diagnostics condition timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_worker_completion(inner: &RenderQueueInner) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if inner.state.lock().worker_completed_at.is_some() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "export worker completion stamp timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn wait_worker_started(inner: &RenderQueueInner) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if inner.state.lock().worker_started {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "export worker start observation timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn policy_diagnostics_do_not_invalidate_retained_job_snapshots() {
    let backend = GateExecutor::new([]);
    let queue = RenderQueue::new_with_executor(backend);
    let initial_diagnostics_revision = queue.revision();
    let initial_jobs_revision = queue.jobs_revision();

    queue.set_dispatch_enabled(false);
    assert!(queue.revision() > initial_diagnostics_revision);
    assert_eq!(queue.jobs_revision(), initial_jobs_revision);

    let before_policy_revision = queue.revision();
    let mut policy = ExportExecutionResourcePolicy::default();
    policy.title_cache_entries = policy.title_cache_entries.saturating_add(1);
    queue.set_resource_policy(policy);
    assert!(queue.revision() > before_policy_revision);
    assert_eq!(queue.jobs_revision(), initial_jobs_revision);

    queue
        .enqueue(RenderJob::new(dummy_config("jobs-revision.mp4")))
        .expect("paused queue should retain the admitted job");
    assert!(queue.jobs_revision() > initial_jobs_revision);
}

#[test]
fn identical_job_publications_are_observation_noops() {
    let backend = GateExecutor::new([GateOutcome::Complete]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    let job_id = queue
        .enqueue(RenderJob::new(dummy_config("identical-publication.mp4")))
        .expect("admit export");
    backend.wait_started(1);

    let snapshot = queue.list_jobs().into_iter().find(|job| job.id == job_id).expect("running job");
    let before = queue.jobs_revision();
    update_job_progress(&queue.inner, job_id, snapshot.generation, snapshot.progress);
    update_job_diagnostics(
        &queue.inner,
        job_id,
        snapshot.generation,
        snapshot.diagnostics,
    );
    assert_eq!(queue.jobs_revision(), before);

    backend.release(1);
    backend.wait_finished(1);
}

#[test]
fn admission_rejects_an_incoherent_delivery_before_worker_dispatch() {
    let backend = GateExecutor::new([]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    let mut config = dummy_config("invalid-delivery.mp4");
    config.preset.video_signal.bit_depth = ExportParameter::Explicit(DeliveryBitDepth::Ten);

    assert!(matches!(
        queue.enqueue(RenderJob::new(config)),
        Err(ExportAdmissionError::InvalidDelivery { .. })
    ));
    assert_eq!(queue.diagnostics().rejections, 1);
    assert_eq!(*backend.started.lock(), 0);
}

#[test]
fn admission_requires_an_existing_output_parent() {
    let backend = GateExecutor::new([]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    let root = std::env::temp_dir().join(format!("mondrian-export-parent-{}", JobId::new()));
    let output = root.join("missing").join("deliverable.mp4");

    assert!(matches!(
        queue.enqueue(RenderJob::new(dummy_config(output.clone()))),
        Err(ExportAdmissionError::InvalidOutputPath { path }) if path == output
    ));
    assert_eq!(*backend.started.lock(), 0);
}

#[test]
fn create_only_rejects_an_existing_output_but_explicit_overwrite_is_frozen() {
    let backend = GateExecutor::new([]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    let root =
        std::env::temp_dir().join(format!("mondrian-export-admission-policy-{}", JobId::new()));
    std::fs::create_dir_all(&root).expect("create export policy root");
    let output = root.join("deliverable.mp4");
    std::fs::write(&output, b"existing").expect("write existing output");

    assert!(matches!(
        queue.enqueue(RenderJob::new(dummy_config(output.clone()))),
        Err(ExportAdmissionError::OutputAlreadyExists { path })
            if path.is_absolute() && path.file_name() == output.file_name()
    ));

    let mut overwrite = dummy_config(output.clone());
    overwrite.output_policy = ExportOutputPolicy::OverwriteExisting;
    queue
        .enqueue(RenderJob::new(overwrite))
        .expect("explicit overwrite must be admitted");
    let snapshot = queue.list_jobs().pop().expect("admitted job snapshot");
    assert_eq!(
        snapshot.output_policy,
        ExportOutputPolicy::OverwriteExisting
    );
    assert_eq!(
        std::fs::read(&output).expect("read existing output"),
        b"existing"
    );
    backend.release(1);
    backend.wait_finished(1);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn admission_is_bounded_and_reserves_normalized_active_output_paths() {
    // The relative fixture routes resolve against the test process CWD, so
    // make sure the relative parent exists on every runner.
    std::fs::create_dir_all("target").expect("create relative output parent");
    let backend = GateExecutor::new([]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    queue
        .enqueue(RenderJob::new(dummy_config("target/export-a.mp4")))
        .expect("admit first export");
    backend.wait_started(1);
    assert!(
        queue.list_jobs().first().is_some_and(|job| job.output_path.is_absolute()),
        "queue admission must freeze an absolute output route"
    );

    assert!(matches!(
        queue.enqueue(RenderJob::new(dummy_config("target/./export-a.mp4"))),
        Err(ExportAdmissionError::OutputPathBusy { .. })
    ));
    for index in 1..EXPORT_IN_FLIGHT_CAPACITY {
        queue
            .enqueue(RenderJob::new(dummy_config(format!(
                "target/export-{index}.mp4"
            ))))
            .expect("fill bounded export capacity");
    }
    assert!(matches!(
        queue.enqueue(RenderJob::new(dummy_config("target/overflow.mp4"))),
        Err(ExportAdmissionError::CapacityExceeded { capacity: EXPORT_IN_FLIGHT_CAPACITY })
    ));

    let diagnostics = queue.diagnostics();
    assert_eq!(diagnostics.running, 1);
    assert_eq!(diagnostics.pending, EXPORT_IN_FLIGHT_CAPACITY - 1);
    assert_eq!(diagnostics.admissions, EXPORT_IN_FLIGHT_CAPACITY as u64);
    assert_eq!(diagnostics.rejections, 2);
}

#[test]
fn paused_dispatch_retains_explicit_jobs_and_resumes_them() {
    let backend = GateExecutor::new([GateOutcome::Complete]);
    backend.release(1);
    let queue = RenderQueue::new_with_executor(backend.clone());
    queue.set_dispatch_enabled(false);

    let job_id = queue
        .enqueue(RenderJob::new(dummy_config("paused-explicit.mp4")))
        .expect("paused queue still admits explicit export");
    std::thread::sleep(Duration::from_millis(20));
    let paused = queue.diagnostics();
    assert!(!paused.dispatch_enabled);
    assert_eq!(paused.pending, 1);
    assert!(
        !paused.running_yield_requested,
        "pending-only dispatch pause is not a running yield request"
    );
    assert_eq!(*backend.started.lock(), 0);
    assert!(paused.jobs.iter().any(|job| job.id == job_id));

    queue.set_dispatch_enabled(true);
    backend.wait_started(1);
    let completed = wait_diagnostics(&queue, |diagnostics| diagnostics.completions == 1);
    assert!(completed.dispatch_enabled);
    assert!(matches!(
        completed.jobs.iter().find(|job| job.id == job_id).map(|job| &job.status),
        Some(JobStatus::Completed)
    ));
}

#[test]
fn running_export_yields_on_pause_and_resumes_to_completion() {
    let backend = GateExecutor::new([GateOutcome::Complete]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    let job_id = queue
        .enqueue(RenderJob::new(dummy_config("running-yield-resume.mp4")))
        .expect("admit running export");
    backend.wait_started(1);

    queue.set_dispatch_enabled(false);
    let yielded = wait_diagnostics(&queue, |diagnostics| diagnostics.running_yielded == 1);
    assert!(!yielded.dispatch_enabled);
    assert!(yielded.running_yield_requested);
    assert_eq!(yielded.running, 1);
    assert_eq!(yielded.completions, 0);

    backend.release(1);
    std::thread::sleep(Duration::from_millis(20));
    let still_yielded = queue.diagnostics();
    assert_eq!(still_yielded.running_yielded, 1);
    assert_eq!(still_yielded.completions, 0);

    queue.set_dispatch_enabled(true);
    let completed = wait_diagnostics(&queue, |diagnostics| diagnostics.completions == 1);
    assert!(completed.dispatch_enabled);
    assert!(!completed.running_yield_requested);
    assert_eq!(completed.running_yielded, 0);
    assert!(matches!(
        completed.jobs.iter().find(|job| job.id == job_id).map(|job| &job.status),
        Some(JobStatus::Completed)
    ));
}

#[test]
fn cancelling_a_running_export_while_yielded_wakes_the_gate() {
    let backend = GateExecutor::new([GateOutcome::Complete]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    let job_id = queue
        .enqueue(RenderJob::new(dummy_config("cancel-while-yielded.mp4")))
        .expect("admit running export");
    backend.wait_started(1);

    queue.set_dispatch_enabled(false);
    wait_diagnostics(&queue, |diagnostics| diagnostics.running_yielded == 1);
    assert_eq!(queue.cancel(job_id), ExportCancelOutcome::Requested);

    let cancelled = wait_diagnostics(&queue, |diagnostics| diagnostics.cancellations == 1);
    assert!(
        !cancelled.running_yield_requested,
        "a terminal attempt must not leave a synthetic running-yield request"
    );
    assert_eq!(cancelled.running_yielded, 0);
    assert_eq!(cancelled.cancelling, 0);
    let job = cancelled.jobs.iter().find(|job| job.id == job_id).expect("cancelled job");
    assert!(matches!(job.status, JobStatus::Cancelled));
    assert_eq!(
        job.terminal_evidence.expect("cancellation evidence").disposition,
        ExecutionTerminalDisposition::Canceled
    );
}

#[test]
fn dropping_the_queue_wakes_a_yielded_executor() {
    let backend = GateExecutor::new([GateOutcome::Complete]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    queue
        .enqueue(RenderJob::new(dummy_config("drop-while-yielded.mp4")))
        .expect("admit running export");
    backend.wait_started(1);
    queue.set_dispatch_enabled(false);
    wait_diagnostics(&queue, |diagnostics| diagnostics.running_yielded == 1);

    drop(queue);

    backend.wait_finished(1);
}

#[test]
fn queued_and_running_cancellation_preserve_execution_boundary_evidence() {
    let backend = GateExecutor::new([]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    let running = queue
        .enqueue(RenderJob::new(dummy_config("cancel-running.mp4")))
        .expect("admit running export");
    backend.wait_started(1);
    let queued = queue
        .enqueue(RenderJob::new(dummy_config("cancel-queued.mp4")))
        .expect("admit queued export");

    assert_eq!(queue.cancel(queued), ExportCancelOutcome::Requested);
    assert_eq!(queue.cancel(queued), ExportCancelOutcome::AlreadyTerminal);
    assert_eq!(queue.cancel(running), ExportCancelOutcome::Requested);
    assert_eq!(queue.cancel(running), ExportCancelOutcome::AlreadyRequested);

    let diagnostics = wait_diagnostics(&queue, |diagnostics| diagnostics.cancellations == 2);
    let queued = diagnostics.jobs.iter().find(|job| job.id == queued).expect("queued evidence");
    let running = diagnostics.jobs.iter().find(|job| job.id == running).expect("running evidence");
    assert!(!queued.executed);
    assert!(running.executed);
    assert_eq!(
        queued.terminal_evidence.expect("queued terminal").disposition,
        ExecutionTerminalDisposition::Canceled
    );
    assert_eq!(
        running.terminal_evidence.expect("running terminal").disposition,
        ExecutionTerminalDisposition::Canceled
    );
}

#[test]
fn completed_publication_wins_over_a_late_cancellation_request() {
    let backend = GateExecutor::committed([GateOutcome::Complete]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    let job_id = queue
        .enqueue(RenderJob::new(dummy_config("committed.mp4")))
        .expect("admit export");
    backend.wait_started(1);

    queue.set_dispatch_enabled(false);
    std::thread::sleep(Duration::from_millis(20));
    let publishing = queue.diagnostics();
    assert!(!publishing.running_yield_requested);
    assert_eq!(publishing.running, 0);
    assert_eq!(publishing.committing, 1);
    let publishing_job =
        publishing.jobs.iter().find(|job| job.id == job_id).expect("committing job");
    assert_eq!(
        publishing_job.publication,
        ExportPublicationState::Committing
    );
    assert!(!publishing_job.status.can_cancel());
    assert_eq!(
        publishing.running_yielded, 0,
        "an attempt past the Publishing gate is irreversible"
    );
    assert_eq!(queue.cancel(job_id), ExportCancelOutcome::TooLateCommitting);
    assert_eq!(queue.diagnostics().too_late_cancellation_requests, 1);
    backend.release(1);

    let diagnostics = wait_diagnostics(&queue, |diagnostics| diagnostics.completions == 1);
    let job = diagnostics.jobs.iter().find(|job| job.id == job_id).expect("terminal evidence");
    assert!(matches!(job.status, JobStatus::Completed));
    assert_eq!(job.publication, ExportPublicationState::Published);
    assert!(matches!(
        job.artifact_publication.as_ref(),
        Some(ExportArtifactPublicationEvidence::Durable { output_path })
            if output_path.is_absolute()
    ));
    assert_eq!(diagnostics.cancellations, 0);
    assert_eq!(
        job.terminal_evidence.expect("completed terminal").disposition,
        ExecutionTerminalDisposition::Completed
    );
}

#[test]
fn publication_failures_retain_typed_terminal_evidence() {
    let cases = [
        (
            GateOutcome::PublicationBeforeNamespace,
            ExportPublicationState::NotPublished,
            ExportFailureReason::PublicationBeforeNamespace,
            "before-namespace.mp4",
        ),
        (
            GateOutcome::PublicationDurabilityUnconfirmed,
            ExportPublicationState::DurabilityUnconfirmed,
            ExportFailureReason::PublicationDurabilityUnconfirmed,
            "durability-unconfirmed.mp4",
        ),
        (
            GateOutcome::PublicationNamespaceIndeterminate,
            ExportPublicationState::OutcomeUnknown,
            ExportFailureReason::PublicationNamespaceIndeterminate,
            "namespace-indeterminate.mp4",
        ),
    ];

    for (outcome, expected_publication, expected_reason, output_name) in cases {
        let backend = GateExecutor::committed([outcome]);
        let queue = RenderQueue::new_with_executor(backend.clone());
        let job_id =
            queue.enqueue(RenderJob::new(dummy_config(output_name))).expect("admit export");
        backend.wait_started(1);
        backend.release(1);

        let diagnostics = wait_diagnostics(&queue, |diagnostics| diagnostics.failures == 1);
        let job = diagnostics
            .jobs
            .iter()
            .find(|job| job.id == job_id)
            .expect("failed publication");
        assert_eq!(job.publication, expected_publication);
        let JobStatus::Failed(failure) = &job.status else {
            panic!("publication failure must fail the job");
        };
        assert_eq!(failure.reason, expected_reason);
        let evidence =
            job.artifact_publication.as_ref().expect("typed artifact publication evidence");
        match evidence {
            ExportArtifactPublicationEvidence::BeforeNamespace {
                output_path,
                retained_partial_path,
            } => {
                assert!(output_path.is_absolute());
                assert!(retained_partial_path.as_ref().is_some_and(|path| path.is_absolute()));
            }
            ExportArtifactPublicationEvidence::DurabilityUnconfirmed { output_path } => {
                assert!(output_path.is_absolute());
            }
            ExportArtifactPublicationEvidence::NamespaceIndeterminate {
                output_path,
                retained_partial_path,
            } => {
                assert!(output_path.is_absolute());
                assert!(retained_partial_path.as_ref().is_some_and(|path| path.is_absolute()));
            }
            ExportArtifactPublicationEvidence::Durable { .. } => {
                panic!("failure must not carry durable completion evidence");
            }
        }
    }
}

#[test]
fn progress_rejects_phase_and_unit_regression() {
    let previous = ExportProgress::rendering(0.5, 50, 100);
    let regressed = ExportProgress::rendering(0.4, 40, 100).normalized(previous);
    assert_eq!(regressed.fraction, 0.5);
    assert_eq!(
        regressed.detail,
        ExportProgressDetail::Frames { completed: 50, total: 100 }
    );

    let inconsistent_total = ExportProgress::rendering(0.6, 60, 90).normalized(regressed);
    assert_eq!(inconsistent_total.fraction, 0.6);
    assert_eq!(inconsistent_total.detail, regressed.detail);

    let wrong_unit = ExportProgress {
        phase: ExportProgressPhase::Encoding,
        fraction: 0.7,
        detail: ExportProgressDetail::Frames { completed: 70, total: 100 },
    }
    .normalized(inconsistent_total);
    assert_eq!(wrong_unit.detail, ExportProgressDetail::None);

    assert!(ExportProgressPhase::Encoding.rank() < ExportProgressPhase::Packaging.rank());
    assert!(ExportProgressPhase::Packaging.rank() < ExportProgressPhase::Validating.rank());
    assert_eq!(
        serde_json::to_string(&ExportProgressPhase::Packaging).expect("serialize packaging phase"),
        "\"packaging\""
    );
    assert_eq!(
        serde_json::from_str::<ExportProgressPhase>("\"packaging\"")
            .expect("deserialize packaging phase"),
        ExportProgressPhase::Packaging
    );
}

#[test]
fn packaging_is_a_cooperative_cancellation_boundary_before_publication() {
    let gate = ExportExecutionGate::always_open_for_test();
    let cancellation = ExecutionCancellationToken::new();
    cancellation.cancel();

    assert!(!gate.wait_at_boundary(ExportProgressPhase::Packaging, &cancellation));
    assert!(gate.wait_at_boundary(ExportProgressPhase::Publishing, &cancellation));
}

#[test]
fn exhausted_generation_space_rejects_without_aliasing_attempts() {
    let queue = RenderQueue::new_with_executor(GateExecutor::new([]));
    queue.inner.state.lock().next_generation = u64::MAX;

    assert!(matches!(
        queue.enqueue(RenderJob::new(dummy_config("generation-exhausted.mp4"))),
        Err(ExportAdmissionError::GenerationExhausted)
    ));
    let diagnostics = queue.diagnostics();
    assert_eq!(diagnostics.admissions, 0);
    assert_eq!(diagnostics.rejections, 1);
}

#[test]
fn executor_panic_is_isolated_and_next_job_runs() {
    let backend = GateExecutor::new([GateOutcome::Panic, GateOutcome::Complete]);
    backend.release(2);
    let queue = RenderQueue::new_with_executor(backend);
    let panicked = queue
        .enqueue(RenderJob::new(dummy_config("panic.mp4")))
        .expect("admit panic export");
    let completed = queue
        .enqueue(RenderJob::new(dummy_config("after-panic.mp4")))
        .expect("admit following export");

    let diagnostics = wait_diagnostics(&queue, |diagnostics| {
        diagnostics.failures == 1 && diagnostics.completions == 1
    });
    assert!(matches!(
        diagnostics.jobs.iter().find(|job| job.id == panicked).map(|job| &job.status),
        Some(JobStatus::Failed(ExportFailure {
            reason: ExportFailureReason::ExecutorPanicked,
            ..
        }))
    ));
    assert!(matches!(
        diagnostics.jobs.iter().find(|job| job.id == completed).map(|job| &job.status),
        Some(JobStatus::Completed)
    ));
    let shutdown = queue.shutdown_until(Instant::now() + Duration::from_secs(2));
    assert!(shutdown.worker_terminated);
    assert!(!shutdown.worker_panicked);
    assert!(!shutdown.worker_timed_out);
    assert!(!shutdown.worker_detached);
    assert!(shutdown.all_resources_released());
}

#[test]
fn opaque_executor_panic_payload_is_abandoned_without_stopping_worker() {
    let payload_dropped = Arc::new(AtomicBool::new(false));
    let backend = GateExecutor::new([
        GateOutcome::OpaquePanic(Arc::clone(&payload_dropped)),
        GateOutcome::Complete,
    ]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    queue.set_dispatch_enabled(false);
    queue
        .enqueue(RenderJob::new(dummy_config("opaque-panic.mp4")))
        .expect("admit opaque-panic export");
    queue
        .enqueue(RenderJob::new(dummy_config("after-opaque-panic.mp4")))
        .expect("admit following export");
    backend.release(2);
    queue.set_dispatch_enabled(true);

    let diagnostics = wait_diagnostics(&queue, |diagnostics| {
        diagnostics.failures == 1 && diagnostics.completions == 1
    });
    assert!(diagnostics.worker_failure.is_some());
    let shutdown = queue.shutdown_until(Instant::now() + Duration::from_secs(2));

    assert!(!payload_dropped.load(Ordering::Acquire));
    assert!(shutdown.worker_terminated);
    assert!(!shutdown.worker_panicked);
    assert!(shutdown.worker_owner_abandoned);
    assert!(!shutdown.all_resources_released());
}

#[test]
fn terminal_failure_detail_is_bounded_and_heavy_payload_is_released() {
    let backend = GateExecutor::new([GateOutcome::Fail("x".repeat(5_000))]);
    backend.release(1);
    let queue = RenderQueue::new_with_executor(backend);
    let job_id = queue
        .enqueue(RenderJob::new(dummy_config("bounded-failure.mp4")))
        .expect("admit failing export");
    let diagnostics = wait_diagnostics(&queue, |diagnostics| diagnostics.failures == 1);
    let job = diagnostics.jobs.iter().find(|job| job.id == job_id).expect("failed job");
    let JobStatus::Failed(failure) = &job.status else {
        panic!("expected failed job");
    };
    assert_eq!(
        failure.detail.chars().count(),
        EXPORT_FAILURE_DETAIL_CHARS + 1
    );
    assert!(failure.detail.ends_with('…'));
    let state = queue.inner.state.lock();
    assert!(state
        .jobs
        .iter()
        .find(|entry| entry.snapshot.id == job_id)
        .is_some_and(|entry| entry.payload.is_none()));
}

#[test]
fn terminal_history_trimming_never_removes_active_work() {
    let mut state = ExportQueueState { next_generation: 1, ..ExportQueueState::default() };
    for generation in 1..=(EXPORT_TERMINAL_HISTORY_CAPACITY as u64 + 1) {
        let job = RenderJob::new(dummy_config(format!("history-{generation}.mp4")));
        state.jobs.push_back(ExportJobEntry {
            snapshot: ExportJobSnapshot {
                id: job.id,
                generation,
                output_path: job.config.output_path.clone(),
                output_policy: job.config.output_policy,
                preset_name: job.config.preset.name.clone(),
                status: JobStatus::Completed,
                progress: ExportProgress::publishing(1.0),
                publication: ExportPublicationState::Published,
                diagnostics: ExportJobDiagnostics::default(),
                created_at: job.created_at,
                started_at: Some(job.created_at),
                completed_at: Some(job.created_at),
                terminal_evidence: None,
                artifact_publication: None,
                executed: true,
            },
            payload: None,
            cancellation: ExecutionCancellationToken::new(),
            output_key: generation.to_string(),
            execution_yielded: false,
        });
    }
    let active = RenderJob::new(dummy_config("active.mp4"));
    let active_id = active.id;
    state.jobs.push_back(ExportJobEntry {
        snapshot: ExportJobSnapshot {
            id: active.id,
            generation: 10_000,
            output_path: active.config.output_path.clone(),
            output_policy: active.config.output_policy,
            preset_name: active.config.preset.name.clone(),
            status: JobStatus::Pending,
            progress: ExportProgress::default(),
            publication: ExportPublicationState::Reversible,
            diagnostics: ExportJobDiagnostics::default(),
            created_at: active.created_at,
            started_at: None,
            completed_at: None,
            terminal_evidence: None,
            artifact_publication: None,
            executed: false,
        },
        payload: Some(active),
        cancellation: ExecutionCancellationToken::new(),
        output_key: "active".to_owned(),
        execution_yielded: false,
    });

    trim_terminal_history(&mut state);

    assert_eq!(
        state.jobs.iter().filter(|entry| entry.snapshot.status.is_terminal()).count(),
        EXPORT_TERMINAL_HISTORY_CAPACITY
    );
    assert!(state.jobs.iter().any(|entry| entry.snapshot.id == active_id));
    assert_eq!(
        state.jobs.front().expect("oldest retained").snapshot.generation,
        2
    );
}

#[test]
fn endurance_snapshot_retains_cumulative_frames_and_durable_artifacts() {
    let backend = GateExecutor::new([GateOutcome::Complete]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    let job_id = queue
        .enqueue(RenderJob::new(dummy_config("endurance-snapshot.mp4")))
        .expect("enqueue");
    backend.wait_started(1);
    let generation = queue
        .list_jobs()
        .into_iter()
        .find(|job| job.id == job_id)
        .expect("job")
        .generation;
    update_job_progress(
        &queue.inner,
        job_id,
        generation,
        ExportProgress::rendering(0.5, 12, 24),
    );
    let running = queue.endurance_snapshot(100);
    assert!(running.worker_running);
    assert!(!running.worker_terminated);
    assert_eq!(running.rendered_frames, 12);
    assert_eq!(running.active_jobs, 1);

    backend.release(1);
    wait_diagnostics(&queue, |diagnostics| diagnostics.completions == 1);
    let completed = queue.endurance_snapshot(200);
    assert_eq!(completed.completions, 1);
    assert_eq!(completed.durable_artifacts, 1);
    assert_eq!(completed.rendered_frames, 12);
    assert_eq!(completed.pending_jobs, 0);
    assert_eq!(completed.active_jobs, 0);
    assert!(completed.activity_events > running.activity_events);

    let shutdown = queue.shutdown_and_wait(Duration::from_secs(2));
    assert!(shutdown.worker_terminated);
    assert_eq!(shutdown.pending_jobs, 0);
    assert_eq!(shutdown.active_jobs, 0);
}

#[test]
fn audio_source_owner_lifecycle_is_latched_into_endurance_and_shutdown_evidence() {
    fn execute_owner_outcome(
        outcome: GateOutcome,
        output_name: &str,
    ) -> (ExportEnduranceSnapshot, ExportQueueShutdownEvidence) {
        let backend = GateExecutor::new([outcome]);
        let queue = RenderQueue::new_with_executor(backend.clone());
        queue
            .enqueue(RenderJob::new(dummy_config(output_name)))
            .expect("enqueue owner lifecycle export");
        backend.wait_started(1);
        backend.release(1);
        backend.wait_finished(1);
        wait_diagnostics(&queue, |diagnostics| diagnostics.failures == 1);
        let snapshot = queue.endurance_snapshot(42);
        let shutdown = queue.shutdown_and_wait(Duration::from_secs(2));
        (snapshot, shutdown)
    }

    let (clean, clean_shutdown) =
        execute_owner_outcome(GateOutcome::AudioOwnerClean, "owner-clean.mp4");
    assert_eq!(clean.schema_version, 2);
    assert_eq!(clean.audio_source_owners_started, 1);
    assert_eq!(clean.audio_source_owners_closed, 1);
    assert_eq!(clean.audio_source_owner_failures, 0);
    assert_eq!(clean.active_audio_source_owners, 0);
    assert_eq!(clean_shutdown.schema_version, 4);
    assert!(clean_shutdown.all_resources_released());

    let (dirty, dirty_shutdown) =
        execute_owner_outcome(GateOutcome::AudioOwnerDirty, "owner-dirty.mp4");
    assert_eq!(dirty.audio_source_owners_started, 1);
    assert_eq!(dirty.audio_source_owners_closed, 1);
    assert_eq!(dirty.audio_source_owner_failures, 1);
    assert_eq!(dirty.active_audio_source_owners, 0);
    assert!(!dirty_shutdown.all_resources_released());

    let (unclosed, unclosed_shutdown) =
        execute_owner_outcome(GateOutcome::AudioOwnerUnclosed, "owner-unclosed.mp4");
    assert_eq!(unclosed.audio_source_owners_started, 1);
    assert_eq!(unclosed.audio_source_owners_closed, 0);
    assert_eq!(unclosed.audio_source_owner_failures, 0);
    assert_eq!(unclosed.active_audio_source_owners, 1);
    assert!(!unclosed_shutdown.all_resources_released());
}

#[test]
fn endurance_snapshot_linearizes_pending_committing_terminal_and_shutdown_states() {
    let backend = GateExecutor::committed([GateOutcome::Complete]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    queue.set_dispatch_enabled(false);
    let initial = queue.endurance_snapshot(0);
    let job_id = queue
        .enqueue(RenderJob::new(dummy_config("endurance-linearized.mp4")))
        .expect("enqueue pending export");
    let pending = queue.endurance_snapshot(1);
    assert_eq!(pending.admissions, 1);
    assert_eq!(pending.pending_jobs, 1);
    assert_eq!(pending.active_jobs, 0);
    assert!(pending.activity_events > initial.activity_events);

    queue.set_dispatch_enabled(true);
    backend.wait_started(1);
    let committing = queue.endurance_snapshot(2);
    assert_eq!(committing.pending_jobs, 0);
    assert_eq!(committing.active_jobs, 1);
    assert_eq!(committing.completions, 0);
    assert!(committing.activity_events > pending.activity_events);
    assert_eq!(queue.cancel(job_id), ExportCancelOutcome::TooLateCommitting);
    let after_late_cancel = queue.endurance_snapshot(3);
    assert_eq!(after_late_cancel.active_jobs, 1);
    assert_eq!(after_late_cancel.cancellations, 0);
    assert_eq!(
        after_late_cancel.activity_events,
        committing.activity_events
    );

    backend.release(1);
    wait_diagnostics(&queue, |diagnostics| diagnostics.completions == 1);
    let terminal = queue.endurance_snapshot(4);
    assert_eq!(terminal.pending_jobs, 0);
    assert_eq!(terminal.active_jobs, 0);
    assert_eq!(terminal.completions, 1);
    assert_eq!(terminal.durable_artifacts, 1);
    assert!(terminal.activity_events > after_late_cancel.activity_events);

    let shutdown = queue.shutdown_and_wait(Duration::from_secs(2));
    let closed = queue.endurance_snapshot(5);
    assert!(shutdown.worker_terminated);
    assert!(closed.shutdown_requested);
    assert!(!closed.worker_running);
    assert!(closed.worker_terminated);
    assert_eq!(closed.pending_jobs, 0);
    assert_eq!(closed.active_jobs, 0);
    assert_eq!(closed.activity_events, shutdown.activity_events);
}

#[test]
fn endurance_shutdown_timeout_detaches_once_and_late_return_cannot_upgrade_receipt() {
    let backend = GateExecutor::committed([GateOutcome::Complete]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    queue
        .enqueue(RenderJob::new(dummy_config("endurance-timeout.mp4")))
        .expect("enqueue export");
    backend.wait_started(1);

    let timed_out = queue.shutdown_and_wait(Duration::from_millis(1));
    let retained = queue.endurance_snapshot(10);
    assert!(!timed_out.worker_terminated);
    assert!(!timed_out.worker_panicked);
    assert!(timed_out.worker_timed_out);
    assert!(timed_out.worker_detached);
    assert!(!timed_out.all_resources_released());
    assert_eq!(timed_out.active_jobs, 1);
    assert!(retained.shutdown_requested);
    assert!(retained.worker_running);
    assert!(!retained.worker_terminated);
    assert_eq!(retained.active_jobs, 1);

    backend.release(1);
    backend.wait_finished(1);
    wait_worker_completion(&queue.inner);
    let closed = queue.shutdown_and_wait(Duration::from_secs(2));
    let terminal = queue.endurance_snapshot(11);
    assert!(!closed.worker_terminated);
    assert!(closed.worker_timed_out);
    assert!(closed.worker_detached);
    assert!(!closed.all_resources_released());
    assert_eq!(closed.active_jobs, 0);
    assert!(!terminal.worker_running);
    assert!(!terminal.worker_terminated);
    assert_eq!(terminal.active_jobs, 0);
}

#[test]
fn worker_panic_outside_executor_boundary_is_joined_and_classified_exactly() {
    let payload_dropped = Arc::new(AtomicBool::new(false));
    let worker_payload_dropped = Arc::clone(&payload_dropped);
    let queue = RenderQueue::new_with_executor_and_spawner(
        GateExecutor::new([]),
        Some(Box::new(move || {
            std::panic::panic_any(PanicDropProbe { dropped: worker_payload_dropped })
        })),
        |task| {
            std::thread::Builder::new()
                .name("mondrian-export-worker-panic-test".to_owned())
                .spawn(task)
        },
    );

    let evidence = queue.shutdown_until(Instant::now() + Duration::from_secs(2));

    assert!(evidence.worker_started);
    assert!(!evidence.worker_start_failed);
    assert!(!evidence.worker_terminated);
    assert!(evidence.worker_panicked);
    assert!(!evidence.worker_timed_out);
    assert!(!evidence.worker_detached);
    assert!(evidence.worker_owner_abandoned);
    assert!(!payload_dropped.load(Ordering::Acquire));
    assert!(!evidence.all_resources_released());
}

#[test]
fn already_finished_late_worker_is_joined_without_detach_but_cannot_be_clean() {
    let queue = RenderQueue::new_with_executor(GateExecutor::new([]));
    let deadline = Instant::now();
    queue.begin_shutdown();
    let observation_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let completed = queue.inner.state.lock().worker_completed_at.is_some();
        let finished = queue.worker.lock().as_ref().is_some_and(JoinHandle::is_finished);
        if completed && finished {
            break;
        }
        assert!(
            Instant::now() < observation_deadline,
            "export worker did not finish for late-completion test"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let evidence = queue.shutdown_until(deadline);

    assert!(evidence.worker_started);
    assert!(evidence.worker_terminated);
    assert!(!evidence.worker_panicked);
    assert!(evidence.worker_timed_out);
    assert!(!evidence.worker_detached);
    assert!(!evidence.all_resources_released());
}

#[test]
fn completion_stamp_follows_foreign_executor_destruction() {
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let queue = RenderQueue::new_with_executor(Arc::new(BlockingDropExecutor {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    }));
    queue.begin_shutdown();

    let observation_deadline = Instant::now() + Duration::from_secs(2);
    while !entered.load(Ordering::Acquire) {
        assert!(
            Instant::now() < observation_deadline,
            "export executor destructor did not start"
        );
        std::thread::yield_now();
    }
    let deadline = Instant::now();
    release.store(true, Ordering::Release);
    let finish_observation_deadline = Instant::now() + Duration::from_secs(2);
    while !queue.worker.lock().as_ref().is_some_and(JoinHandle::is_finished) {
        assert!(
            Instant::now() < finish_observation_deadline,
            "export worker did not finish after destructor release"
        );
        std::thread::yield_now();
    }

    let evidence = queue.shutdown_until(deadline);

    assert!(evidence.worker_terminated);
    assert!(evidence.worker_timed_out);
    assert!(!evidence.worker_detached);
    assert!(!evidence.all_resources_released());
}

#[test]
fn worker_spawn_error_abandons_retained_executor_without_caller_drop() {
    let dropped = Arc::new(AtomicBool::new(false));
    let error_dropped = Arc::new(AtomicBool::new(false));
    let spawner_error_dropped = Arc::clone(&error_dropped);
    let queue = RenderQueue::new_with_executor_and_spawner(
        Arc::new(DropProbeExecutor { dropped: Arc::clone(&dropped) }),
        None,
        move |_task| {
            Err(std::io::Error::other(IoErrorDropProbe {
                dropped: spawner_error_dropped,
            }))
        },
    );

    let evidence = queue.shutdown_until(Instant::now() + Duration::from_secs(1));

    assert!(!dropped.load(Ordering::Acquire));
    assert!(!evidence.worker_started);
    assert!(evidence.worker_start_failed);
    assert!(!evidence.worker_terminated);
    assert!(!evidence.worker_panicked);
    assert!(!evidence.worker_timed_out);
    assert!(!evidence.worker_detached);
    assert!(evidence.worker_owner_abandoned);
    assert!(!evidence.all_resources_released());
    drop(queue);
    assert!(!dropped.load(Ordering::Acquire));
    assert!(!error_dropped.load(Ordering::Acquire));
}

#[test]
fn detached_worker_latches_a_late_outer_panic_without_upgrading_termination() {
    let release = Arc::new(AtomicBool::new(false));
    let worker_release = Arc::clone(&release);
    let payload_dropped = Arc::new(AtomicBool::new(false));
    let worker_payload_dropped = Arc::clone(&payload_dropped);
    let queue = RenderQueue::new_with_executor_and_spawner(
        GateExecutor::new([]),
        Some(Box::new(move || {
            while !worker_release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            std::panic::panic_any(PanicDropProbe { dropped: worker_payload_dropped });
        })),
        |task| {
            std::thread::Builder::new()
                .name("mondrian-export-worker-late-panic-test".to_owned())
                .spawn(task)
        },
    );
    wait_worker_started(&queue.inner);

    let detached = queue.shutdown_until(Instant::now() + Duration::from_millis(1));
    assert!(detached.worker_timed_out);
    assert!(detached.worker_detached);
    assert!(!detached.worker_panicked);
    release.store(true, Ordering::Release);
    let observation_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = queue.inner.state.lock();
        if state.worker_completed_at.is_some() && state.worker_owner_abandoned {
            break;
        }
        drop(state);
        assert!(
            Instant::now() < observation_deadline,
            "late detached worker panic facts were not published"
        );
        std::thread::yield_now();
    }

    let late = queue.shutdown_until(Instant::now() + Duration::from_secs(1));
    assert!(!payload_dropped.load(Ordering::Acquire));
    assert!(!late.worker_terminated);
    assert!(late.worker_panicked);
    assert!(late.worker_timed_out);
    assert!(late.worker_detached);
    assert!(late.worker_owner_abandoned);
    assert!(!late.all_resources_released());
}

#[test]
fn worker_spawner_panic_abandons_retained_executor_without_caller_drop() {
    let dropped = Arc::new(AtomicBool::new(false));
    let panic_payload_dropped = Arc::new(AtomicBool::new(false));
    let spawner_payload_dropped = Arc::clone(&panic_payload_dropped);
    let queue = RenderQueue::new_with_executor_and_spawner(
        Arc::new(DropProbeExecutor { dropped: Arc::clone(&dropped) }),
        None,
        move |_task| -> std::io::Result<JoinHandle<()>> {
            std::panic::panic_any(PanicDropProbe { dropped: spawner_payload_dropped })
        },
    );

    let evidence = queue.shutdown_until(Instant::now() + Duration::from_secs(1));

    assert!(!dropped.load(Ordering::Acquire));
    assert!(!evidence.worker_started);
    assert!(evidence.worker_start_failed);
    assert!(!evidence.worker_terminated);
    assert!(!evidence.worker_panicked);
    assert!(!evidence.worker_timed_out);
    assert!(!evidence.worker_detached);
    assert!(evidence.worker_owner_abandoned);
    assert!(!evidence.all_resources_released());
    drop(queue);
    assert!(!dropped.load(Ordering::Acquire));
    assert!(!panic_payload_dropped.load(Ordering::Acquire));
}

#[test]
fn ordinary_drop_signals_and_detaches_active_worker_without_waiting() {
    let backend = GateExecutor::committed([GateOutcome::Complete]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    queue
        .enqueue(RenderJob::new(dummy_config("ordinary-drop-bounded.mp4")))
        .expect("enqueue export");
    backend.wait_started(1);
    let worker_inner = Arc::clone(&queue.inner);

    let started_at = Instant::now();
    drop(queue);
    assert!(started_at.elapsed() < Duration::from_millis(500));
    {
        let state = worker_inner.state.lock();
        assert!(state.shutdown_requested);
        assert!(state.worker_detached);
        assert!(!state.worker_timed_out);
        assert!(!state.worker_terminated);
    }

    backend.release(1);
    backend.wait_finished(1);
    wait_worker_completion(&worker_inner);
    let state = worker_inner.state.lock();
    assert!(state.worker_detached);
    assert!(!state.worker_terminated);
}

#[test]
fn ordinary_drop_joins_an_already_finished_worker_without_detach() {
    let queue = RenderQueue::new_with_executor(GateExecutor::new([]));
    queue.begin_shutdown();
    let observation_deadline = Instant::now() + Duration::from_secs(2);
    while !queue.worker.lock().as_ref().is_some_and(JoinHandle::is_finished) {
        assert!(
            Instant::now() < observation_deadline,
            "export worker did not finish before ordinary Drop"
        );
        std::thread::yield_now();
    }
    let worker_inner = Arc::clone(&queue.inner);

    drop(queue);

    let state = worker_inner.state.lock();
    assert!(state.worker_terminated);
    assert!(!state.worker_panicked);
    assert!(!state.worker_detached);
}

#[test]
fn explicit_shutdown_terminalizes_pending_jobs_and_reaps_worker() {
    let backend = GateExecutor::new([]);
    let queue = RenderQueue::new_with_executor(backend);
    queue.set_dispatch_enabled(false);
    let job_id = queue
        .enqueue(RenderJob::new(dummy_config("shutdown-pending.mp4")))
        .expect("enqueue pending");

    let evidence = queue.shutdown_and_wait(Duration::from_secs(2));
    assert!(evidence.worker_terminated);
    assert!(!evidence.worker_panicked);
    assert!(!evidence.worker_timed_out);
    assert!(!evidence.worker_detached);
    assert!(!evidence.worker_owner_abandoned);
    assert!(evidence.all_resources_released());
    assert_eq!(evidence.pending_jobs, 0);
    assert_eq!(evidence.active_jobs, 0);
    let repeated = queue.shutdown_until(Instant::now() + Duration::from_secs(2));
    assert_eq!(repeated, evidence);
    let snapshot = queue.endurance_snapshot(300);
    assert!(snapshot.shutdown_requested);
    assert!(!snapshot.worker_running);
    assert!(snapshot.worker_terminated);
    assert_eq!(snapshot.activity_events, evidence.activity_events);
    assert_eq!(snapshot.cancellations, 1);
    assert!(matches!(
        queue
            .list_jobs()
            .into_iter()
            .find(|job| job.id == job_id)
            .expect("retained terminal")
            .status,
        JobStatus::Cancelled
    ));

    assert!(matches!(
        queue.enqueue(RenderJob::new(dummy_config("after-shutdown.mp4"))),
        Err(ExportAdmissionError::QueueShutdown)
    ));
    let after_rejection = queue.endurance_snapshot(301);
    assert_eq!(after_rejection.pending_jobs, 0);
    assert_eq!(after_rejection.active_jobs, 0);
    assert_eq!(after_rejection.admissions, 1);
}

#[test]
fn regulatory_pse_missing_provider_is_notrun_before_any_export_owner_or_job() {
    let work = tempfile::tempdir().expect("work");
    let queue = RenderQueue::new_unstarted();
    let mut config = dummy_config(work.path().join("never-started.mp4"));
    let delivery = crate::delivery::resolve_export_delivery(
        &config.preset,
        &config.timeline.sequence.settings,
        &config.timeline.color_environment,
    )
    .expect("delivery");
    config.broadcast_qc = Some(mondrian_broadcast::BroadcastQcProfile {
        id: "synthetic-admission".to_owned(),
        edition: "1".to_owned(),
        source_sha256: [1; 32],
        signal_color_space: delivery.color_target.color_space,
        observation_tap:
            mondrian_broadcast::BroadcastQcObservationTap::DeliveryPictureAfterLegalizer,
        active_picture: mondrian_broadcast::QcActivePicture::full(
            delivery.resolution.width,
            delivery.resolution.height,
        ),
        rules: vec![mondrian_broadcast::BroadcastQcRule::LumaFlashCandidate {
            rule_id: "triage".to_owned(),
            minimum_mean_luma_delta: 0.5,
            severity: mondrian_broadcast::BroadcastQcSeverity::Info,
        }],
        maximum_retained_findings: 1,
        require_regulatory_flash_analysis: true,
        require_encoded_artifact_revalidation: true,
    });
    assert!(matches!(
        queue.enqueue(RenderJob::new(config)),
        Err(ExportAdmissionError::RegulatoryPseNotRun {
            reason: crate::RegulatoryPseNotRun::ProviderMissing
        })
    ));
    assert!(queue.list_jobs().is_empty());
    assert!(!work.path().join("never-started.mp4").exists());
    let _ = queue.shutdown_and_wait(Duration::from_secs(1));
}

#[test]
fn broadcast_qc_on_unimplemented_final_artifact_families_is_rejected_before_enqueue() {
    let work = tempfile::tempdir().expect("work");
    let queue = RenderQueue::new_unstarted();
    for (index, preset) in [
        ExportPreset::png_sequence(),
        ExportPreset::audio_stems_pcm24(),
        ExportPreset::imf_app_prores_rdd45_1080p25(),
        ExportPreset::smpte_dcp_2k_flat_24(),
    ]
    .into_iter()
    .enumerate()
    {
        let mut config = dummy_config(work.path().join(format!("never-started-{index}")));
        config.preset = preset;
        let delivery = crate::delivery::resolve_export_delivery(
            &config.preset,
            &config.timeline.sequence.settings,
            &config.timeline.color_environment,
        )
        .expect("builtin delivery");
        config.broadcast_qc = Some(mondrian_broadcast::BroadcastQcProfile {
            id: "synthetic-admission".to_owned(),
            edition: "1".to_owned(),
            source_sha256: [1; 32],
            signal_color_space: delivery.color_target.color_space,
            observation_tap:
                mondrian_broadcast::BroadcastQcObservationTap::DeliveryPictureAfterLegalizer,
            active_picture: mondrian_broadcast::QcActivePicture::full(
                delivery.resolution.width,
                delivery.resolution.height,
            ),
            rules: vec![mondrian_broadcast::BroadcastQcRule::LumaFlashCandidate {
                rule_id: "triage".to_owned(),
                minimum_mean_luma_delta: 0.5,
                severity: mondrian_broadcast::BroadcastQcSeverity::Info,
            }],
            maximum_retained_findings: 1,
            require_regulatory_flash_analysis: false,
            require_encoded_artifact_revalidation: true,
        });
        let Err(ExportAdmissionError::InvalidDelivery { detail }) =
            queue.enqueue(RenderJob::new(config))
        else {
            panic!("unsupported final scan must not enqueue")
        };
        assert!(detail.contains("final-file scan path"), "{detail}");
        assert!(queue.list_jobs().is_empty());
    }
    let _ = queue.shutdown_and_wait(Duration::from_secs(1));
}
