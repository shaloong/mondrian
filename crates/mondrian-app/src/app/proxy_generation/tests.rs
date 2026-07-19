use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mondrian_core::{
    AssetId, ColorSpace, ExecutionCancellationToken, ExecutionTerminalDisposition, ProjectId,
};
use mondrian_media::{
    DecodedVideoRange, MediaFileFingerprint, ProxyColorContract, ProxyConfig,
    ProxyGenerationOutcome, ProxyStatus,
};
use parking_lot::{Condvar, Mutex};

use crate::app::AppState;

use super::backend::ProxyGenerationBackend;
use super::state::{request_admission, ProxyGenerationRequest, ProxyGenerationState};
use super::*;

struct TestFiles(Vec<PathBuf>);

impl TestFiles {
    fn create(&mut self, seed: u64) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mondrian-proxy-service-{}-{seed}.mov",
            AssetId::new()
        ));
        std::fs::write(&path, seed.to_le_bytes()).expect("write proxy service fixture");
        self.0.push(path.clone());
        path
    }
}

impl Drop for TestFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

struct FakeBackend {
    status: Mutex<ProxyStatus>,
    started: Mutex<Vec<AssetId>>,
    started_changed: Condvar,
    permits: Mutex<usize>,
    permit_changed: Condvar,
    outcomes: Mutex<VecDeque<Result<ProxyGenerationOutcome, ProxyGenerationFailure>>>,
}

impl FakeBackend {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(ProxyStatus::Missing),
            started: Mutex::new(Vec::new()),
            started_changed: Condvar::new(),
            permits: Mutex::new(0),
            permit_changed: Condvar::new(),
            outcomes: Mutex::new(VecDeque::new()),
        })
    }

    fn release(&self, count: usize) {
        let mut permits = self.permits.lock();
        *permits = permits.saturating_add(count);
        self.permit_changed.notify_all();
    }

    fn wait_started(&self, count: usize) -> Vec<AssetId> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut started = self.started.lock();
        while started.len() < count {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "proxy worker did not start expected request"
            );
            self.started_changed.wait_for(&mut started, remaining);
        }
        started.clone()
    }

    fn started(&self) -> Vec<AssetId> {
        self.started.lock().clone()
    }

    fn push_outcome(&self, outcome: Result<ProxyGenerationOutcome, ProxyGenerationFailure>) {
        self.outcomes.lock().push_back(outcome);
    }
}

impl ProxyGenerationBackend for FakeBackend {
    fn status(
        &self,
        _request: &ProxyGenerationRequest,
    ) -> Result<ProxyStatus, ProxyGenerationFailure> {
        Ok(*self.status.lock())
    }

    fn execute(
        &self,
        _runtime: &tokio::runtime::Runtime,
        request: &ProxyGenerationRequest,
        cancellation: ExecutionCancellationToken,
    ) -> Result<ProxyGenerationOutcome, ProxyGenerationFailure> {
        self.started.lock().push(request.key.asset_id);
        self.started_changed.notify_all();
        let mut permits = self.permits.lock();
        while *permits == 0 && !cancellation.is_canceled() {
            self.permit_changed.wait_for(&mut permits, Duration::from_millis(5));
        }
        if cancellation.is_canceled() {
            return Ok(ProxyGenerationOutcome::Canceled);
        }
        *permits -= 1;
        self.outcomes.lock().pop_front().unwrap_or_else(|| {
            Ok(ProxyGenerationOutcome::Completed(
                request.key.source_path.with_extension("proxy"),
            ))
        })
    }
}

fn color() -> ProxyColorContract {
    ProxyColorContract::try_new(ColorSpace::Rec709, 8, DecodedVideoRange::Limited)
        .expect("valid proxy color")
}

fn config() -> ProxyConfig {
    ProxyConfig::default()
}

fn wait_diagnostics(
    service: &ProxyGenerationService,
    predicate: impl Fn(&ProxyGenerationDiagnostics) -> bool,
) -> ProxyGenerationDiagnostics {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let diagnostics = service.diagnostics();
        if predicate(&diagnostics) {
            return diagnostics;
        }
        assert!(
            Instant::now() < deadline,
            "proxy diagnostics condition timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn synthetic_request(seed: u64) -> ProxyGenerationRequest {
    ProxyGenerationRequest::new(
        AssetId::new(),
        PathBuf::from(format!("E:/media/proxy-{seed}.mov")),
        MediaFileFingerprint {
            len: Some(seed),
            modified_secs: Some(seed),
            modified_nanos: Some(seed as u32),
        },
        config(),
        color(),
    )
}

#[test]
fn worker_count_reserves_capacity_for_realtime_preview() {
    assert_eq!(proxy_generation_worker_count_for(0), 1);
    assert_eq!(proxy_generation_worker_count_for(1), 1);
    assert_eq!(proxy_generation_worker_count_for(2), 1);
    assert_eq!(proxy_generation_worker_count_for(4), 2);
    assert_eq!(
        proxy_generation_worker_count_for(16),
        MAX_PROXY_GENERATION_WORKERS
    );
}

#[test]
fn bounded_admission_rejects_the_first_request_beyond_capacity() {
    let mut state = ProxyGenerationState::default();
    for seed in 0..PROXY_PENDING_CAPACITY as u64 {
        assert!(matches!(
            request_admission(
                &mut state,
                synthetic_request(seed),
                ProxyGenerationOrigin::Import,
                ProxyStatus::Missing,
            ),
            ProxyGenerationRequestOutcome::Admitted { .. }
        ));
    }
    assert!(matches!(
        request_admission(
            &mut state,
            synthetic_request(PROXY_PENDING_CAPACITY as u64),
            ProxyGenerationOrigin::Import,
            ProxyStatus::Missing,
        ),
        ProxyGenerationRequestOutcome::Failed(ProxyGenerationFailure {
            reason: ProxyGenerationFailureReason::AdmissionRejected,
            ..
        })
    ));
    let diagnostics = diagnostics_snapshot(&state);
    assert_eq!(diagnostics.queued, PROXY_PENDING_CAPACITY);
    assert_eq!(diagnostics.rejections, 1);
    assert!(!diagnostics.terminal_records.last().expect("rejection terminal").executed);
}

#[test]
fn user_request_promotes_an_exact_queued_import_without_duplication() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut files = TestFiles(Vec::new());
    let blocker_id = AssetId::new();
    let target_id = AssetId::new();
    assert!(matches!(
        service.request(
            blocker_id,
            files.create(1),
            config(),
            color(),
            ProxyGenerationOrigin::Import,
        ),
        ProxyGenerationRequestOutcome::Admitted { .. }
    ));
    backend.wait_started(1);
    let target_path = files.create(2);
    assert!(matches!(
        service.request(
            target_id,
            target_path.clone(),
            config(),
            color(),
            ProxyGenerationOrigin::Import,
        ),
        ProxyGenerationRequestOutcome::Admitted { .. }
    ));
    assert_eq!(
        service.request(
            target_id,
            target_path,
            config(),
            color(),
            ProxyGenerationOrigin::User,
        ),
        ProxyGenerationRequestOutcome::Deduplicated { promoted: true }
    );

    backend.release(2);
    let started = backend.wait_started(2);
    assert_eq!(started, vec![blocker_id, target_id]);
    let diagnostics = wait_diagnostics(&service, |diagnostics| diagnostics.completions == 2);
    assert_eq!(diagnostics.admissions, 2);
    assert_eq!(diagnostics.deduplications, 1);
    assert_eq!(diagnostics.promotions, 1);
}

#[test]
fn user_and_playback_recovery_precede_import_but_import_is_not_starved() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut files = TestFiles(Vec::new());
    let blocker = AssetId::new();
    service.request(
        blocker,
        files.create(10),
        config(),
        color(),
        ProxyGenerationOrigin::Import,
    );
    backend.wait_started(1);

    let import_id = AssetId::new();
    service.request(
        import_id,
        files.create(11),
        config(),
        color(),
        ProxyGenerationOrigin::Import,
    );
    let mut foreground = Vec::new();
    for seed in 0..PROXY_FOREGROUND_BURST as u64 {
        let asset_id = AssetId::new();
        foreground.push(asset_id);
        service.request(
            asset_id,
            files.create(20 + seed),
            config(),
            color(),
            if seed % 2 == 0 {
                ProxyGenerationOrigin::User
            } else {
                ProxyGenerationOrigin::PlaybackRecovery
            },
        );
    }
    backend.release(PROXY_FOREGROUND_BURST + 2);
    let started = backend.wait_started(PROXY_FOREGROUND_BURST + 2);
    assert_eq!(started[0], blocker);
    assert_eq!(started[PROXY_FOREGROUND_BURST + 1], import_id);
    assert!(foreground
        .iter()
        .all(|asset_id| started[1..=PROXY_FOREGROUND_BURST].contains(asset_id)));
}

#[test]
fn workers_do_not_dequeue_past_cache_root_transcode_capacity() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(4, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut files = TestFiles(Vec::new());
    for seed in 0..4 {
        service.request(
            AssetId::new(),
            files.create(70 + seed),
            config(),
            color(),
            ProxyGenerationOrigin::Import,
        );
    }
    backend.wait_started(2);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(backend.started().len(), 2);
    let diagnostics = service.diagnostics();
    assert_eq!(diagnostics.running, 2);
    assert_eq!(diagnostics.queued, 2);

    let user_id = AssetId::new();
    service.request(
        user_id,
        files.create(80),
        config(),
        color(),
        ProxyGenerationOrigin::User,
    );
    backend.release(1);
    let started = backend.wait_started(3);
    assert_eq!(started[2], user_id);
    backend.release(4);
}

#[test]
fn project_rotation_cancels_running_and_queued_attempts() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut files = TestFiles(Vec::new());
    service.request(
        AssetId::new(),
        files.create(30),
        config(),
        color(),
        ProxyGenerationOrigin::Import,
    );
    backend.wait_started(1);
    service.request(
        AssetId::new(),
        files.create(31),
        config(),
        color(),
        ProxyGenerationOrigin::Import,
    );

    service.bind_project(Some(ProjectId::new()));
    let diagnostics = wait_diagnostics(&service, |diagnostics| diagnostics.cancellations == 2);
    assert_eq!(diagnostics.queued, 0);
    assert_eq!(diagnostics.running, 0);
    assert_eq!(diagnostics.terminal_records.len(), 2);
    assert!(diagnostics.terminal_records.iter().all(|terminal| {
        terminal.evidence.disposition == ExecutionTerminalDisposition::Canceled
    }));
}

#[test]
fn source_revision_change_is_a_distinct_attempt() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut files = TestFiles(Vec::new());
    let path = files.create(40);
    let asset_id = AssetId::new();
    assert!(matches!(
        service.request(
            asset_id,
            path.clone(),
            config(),
            color(),
            ProxyGenerationOrigin::Import,
        ),
        ProxyGenerationRequestOutcome::Admitted { .. }
    ));
    backend.wait_started(1);
    assert!(matches!(
        service.request(
            asset_id,
            path.clone(),
            config(),
            color(),
            ProxyGenerationOrigin::Import,
        ),
        ProxyGenerationRequestOutcome::Deduplicated { .. }
    ));
    std::fs::write(&path, b"changed source fingerprint").expect("replace source fixture");
    assert!(matches!(
        service.request(
            asset_id,
            path,
            config(),
            color(),
            ProxyGenerationOrigin::Import,
        ),
        ProxyGenerationRequestOutcome::Admitted { .. }
    ));
    assert_eq!(service.diagnostics().queued, 1);
    backend.release(2);
}

#[test]
fn automatic_failure_is_retained_and_explicit_user_request_retries() {
    let backend = FakeBackend::new();
    backend.push_outcome(Err(ProxyGenerationFailure::new(
        ProxyGenerationFailureReason::GenerationFailed,
        "synthetic failure",
    )));
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut files = TestFiles(Vec::new());
    let path = files.create(50);
    let asset_id = AssetId::new();
    service.request(
        asset_id,
        path.clone(),
        config(),
        color(),
        ProxyGenerationOrigin::Import,
    );
    backend.release(1);
    wait_diagnostics(&service, |diagnostics| diagnostics.failures == 1);
    assert!(matches!(
        service.request(
            asset_id,
            path.clone(),
            config(),
            color(),
            ProxyGenerationOrigin::PlaybackRecovery,
        ),
        ProxyGenerationRequestOutcome::RetainedFailure(_)
    ));
    assert!(matches!(
        service.request(
            asset_id,
            path,
            config(),
            color(),
            ProxyGenerationOrigin::User,
        ),
        ProxyGenerationRequestOutcome::Admitted { .. }
    ));
    backend.release(1);
    let diagnostics = wait_diagnostics(&service, |diagnostics| diagnostics.completions == 1);
    assert_eq!(diagnostics.retained_failures, 0);
}

#[test]
fn completion_revision_is_consumed_once_by_event_loop_poll() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    assert!(service.poll_finished());
    assert!(!service.poll_finished());
    let mut files = TestFiles(Vec::new());
    service.request(
        AssetId::new(),
        files.create(60),
        config(),
        color(),
        ProxyGenerationOrigin::Import,
    );
    backend.release(1);
    wait_diagnostics(&service, |diagnostics| diagnostics.completions == 1);
    assert!(service.poll_finished());
    assert!(!service.poll_finished());
}

#[test]
fn app_poll_projects_only_executed_background_failure() {
    let backend = FakeBackend::new();
    backend.push_outcome(Err(ProxyGenerationFailure::new(
        ProxyGenerationFailureReason::GenerationFailed,
        "synthetic worker failure",
    )));
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut state = AppState::new();
    state.proxy_generation = service;
    let mut files = TestFiles(Vec::new());

    state.request_proxy_generation(
        AssetId::new(),
        files.create(90),
        config(),
        color(),
        ProxyGenerationOrigin::Import,
    );
    backend.release(1);
    wait_diagnostics(&state.proxy_generation, |diagnostics| {
        diagnostics.failures == 1
    });

    assert!(state.poll_proxy_generation());
    assert!(
        state.status_hint.as_ref().is_some_and(|(message, is_error)| {
            *is_error && message.contains("synthetic worker failure")
        })
    );
    assert!(state
        .proxy_generation_diagnostics()
        .terminal_records
        .last()
        .is_some_and(|terminal| terminal.executed));
}
