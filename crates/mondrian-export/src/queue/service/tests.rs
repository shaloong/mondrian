use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mondrian_core::{ExecutionTerminalDisposition, ProjectColorManagement};
use mondrian_timeline::sequence::{DeliveryBitDepth, Sequence};
use parking_lot::{Condvar, Mutex};

use super::*;
use crate::preset::{ExportParameter, ExportPreset, TimelineExportRange, TimelineExportSnapshot};
use crate::queue::{ExportExecutor, ExportJobDiagnostics, JobExecutionResult};

enum GateOutcome {
    Complete,
    Fail(String),
    Panic,
}

struct GateExecutor {
    outcomes: Mutex<VecDeque<GateOutcome>>,
    honor_cancellation: bool,
    started: Mutex<usize>,
    started_changed: Condvar,
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
}

impl ExportExecutor for GateExecutor {
    fn execute(
        &self,
        _job: &RenderJob,
        cancellation: &ExecutionCancellationToken,
        report: &mut dyn FnMut(ExportProgress),
        _report_diagnostics: &mut dyn FnMut(ExportJobDiagnostics),
    ) -> JobExecutionResult {
        *self.started.lock() += 1;
        self.started_changed.notify_all();
        report(ExportProgress::preparing(0.1));
        let mut permits = self.permits.lock();
        while *permits == 0 && (!self.honor_cancellation || !cancellation.is_canceled()) {
            self.permit_changed.wait_for(&mut permits, Duration::from_millis(5));
        }
        if self.honor_cancellation && cancellation.is_canceled() {
            return JobExecutionResult::Cancelled;
        }
        *permits -= 1;
        drop(permits);
        match self.outcomes.lock().pop_front().unwrap_or(GateOutcome::Complete) {
            GateOutcome::Complete => JobExecutionResult::Completed,
            GateOutcome::Fail(detail) => JobExecutionResult::Failed(detail),
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
            range: TimelineExportRange::EntireSequence,
            project_color_management: ProjectColorManagement::default(),
        }),
        output_path: output_path.into(),
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
fn admission_is_bounded_and_reserves_normalized_active_output_paths() {
    let backend = GateExecutor::new([]);
    let queue = RenderQueue::new_with_executor(backend.clone());
    queue
        .enqueue(RenderJob::new(dummy_config("target/export-a.mp4")))
        .expect("admit first export");
    backend.wait_started(1);

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

    assert_eq!(queue.cancel(job_id), ExportCancelOutcome::Requested);
    backend.release(1);

    let diagnostics = wait_diagnostics(&queue, |diagnostics| diagnostics.completions == 1);
    let job = diagnostics.jobs.iter().find(|job| job.id == job_id).expect("terminal evidence");
    assert!(matches!(job.status, JobStatus::Completed));
    assert_eq!(diagnostics.cancellations, 0);
    assert_eq!(
        job.terminal_evidence.expect("completed terminal").disposition,
        ExecutionTerminalDisposition::Completed
    );
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
                preset_name: job.config.preset.name.clone(),
                status: JobStatus::Completed,
                progress: ExportProgress::publishing(1.0),
                diagnostics: ExportJobDiagnostics::default(),
                created_at: job.created_at,
                started_at: Some(job.created_at),
                completed_at: Some(job.created_at),
                terminal_evidence: None,
                executed: true,
            },
            payload: None,
            cancellation: ExecutionCancellationToken::new(),
            output_key: generation.to_string(),
        });
    }
    let active = RenderJob::new(dummy_config("active.mp4"));
    let active_id = active.id;
    state.jobs.push_back(ExportJobEntry {
        snapshot: ExportJobSnapshot {
            id: active.id,
            generation: 10_000,
            output_path: active.config.output_path.clone(),
            preset_name: active.config.preset.name.clone(),
            status: JobStatus::Pending,
            progress: ExportProgress::default(),
            diagnostics: ExportJobDiagnostics::default(),
            created_at: active.created_at,
            started_at: None,
            completed_at: None,
            terminal_evidence: None,
            executed: false,
        },
        payload: Some(active),
        cancellation: ExecutionCancellationToken::new(),
        output_key: "active".to_owned(),
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
