use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
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
    PublicationBeforeNamespace,
    PublicationDurabilityUnconfirmed,
    PublicationNamespaceIndeterminate,
    Panic,
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
