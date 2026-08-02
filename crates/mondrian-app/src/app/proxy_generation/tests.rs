use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mondrian_core::{
    AssetId, ColorSpace, ExecutionCancellationToken, ExecutionTerminalDisposition, ProjectId,
};
use mondrian_media::{
    DecodedVideoRange, MediaFileFingerprint, ProxyColorContract, ProxyConfig,
    ProxyGenerationOutcome, ProxyGenerator, ProxyPublicationEvidence, ProxyPublicationFailure,
    ProxyPublicationFailureKind, ProxyPublicationPhase, ProxyStatus,
};
use parking_lot::{Condvar, Mutex};

use crate::app::AppState;

use super::backend::ProxyGenerationBackend;
use super::state::{
    record_immediate_failure, request_admission, terminal_delta_snapshot, ProxyGenerationRequest,
    ProxyGenerationState,
};
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
    observed_cache_roots: Mutex<Vec<PathBuf>>,
    started: Mutex<Vec<AssetId>>,
    started_changed: Condvar,
    permits: Mutex<usize>,
    permit_changed: Condvar,
    outcomes: Mutex<VecDeque<Result<ProxyGenerationOutcome, ProxyGenerationFailure>>>,
    cancellation_return_enabled: AtomicBool,
}

impl FakeBackend {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(ProxyStatus::Missing),
            observed_cache_roots: Mutex::new(Vec::new()),
            started: Mutex::new(Vec::new()),
            started_changed: Condvar::new(),
            permits: Mutex::new(0),
            permit_changed: Condvar::new(),
            outcomes: Mutex::new(VecDeque::new()),
            cancellation_return_enabled: AtomicBool::new(true),
        })
    }

    fn defer_cancellation_return(&self) {
        self.cancellation_return_enabled.store(false, Ordering::Release);
    }

    fn allow_cancellation_return(&self) {
        self.cancellation_return_enabled.store(true, Ordering::Release);
        self.permit_changed.notify_all();
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

    fn last_cache_root(&self) -> Option<PathBuf> {
        self.observed_cache_roots.lock().last().cloned()
    }
}

impl ProxyGenerationBackend for FakeBackend {
    fn status(
        &self,
        request: &ProxyGenerationRequest,
    ) -> Result<ProxyStatus, ProxyGenerationFailure> {
        self.observed_cache_roots.lock().push(request.config.cache_dir.clone());
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
        while *permits == 0
            && (!cancellation.is_canceled()
                || !self.cancellation_return_enabled.load(Ordering::Acquire))
        {
            self.permit_changed.wait_for(&mut permits, Duration::from_millis(5));
        }
        if cancellation.is_canceled() && self.cancellation_return_enabled.load(Ordering::Acquire) {
            return Ok(ProxyGenerationOutcome::Canceled);
        }
        *permits -= 1;
        self.outcomes.lock().pop_front().unwrap_or_else(|| {
            let media_path = request.key.source_path.with_extension("proxy");
            Ok(ProxyGenerationOutcome::Completed(
                ProxyPublicationEvidence::durable(
                    media_path.clone(),
                    ProxyGenerator::manifest_path(&media_path),
                ),
            ))
        })
    }
}

fn color() -> ProxyColorContract {
    ProxyColorContract::try_new(ColorSpace::Rec709, 8, DecodedVideoRange::Limited)
        .expect("valid proxy color")
}

fn config() -> ProxyConfig {
    ProxyConfig::default().freeze_cache_root().expect("absolute test cache root")
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

fn wait_model_change(service: &ProxyGenerationService) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if service.poll_finished() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "proxy model revision did not advance"
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
            object_identity: Some(mondrian_core::MediaFileObjectIdentity::Unix {
                device: 1,
                inode: seed,
            }),
            change_stamp: Some(mondrian_core::MediaFileChangeStamp::Unix {
                seconds: seed as i64,
                nanoseconds: i64::from(seed as u32),
            }),
        },
        config(),
        color(),
    )
    .expect("absolute synthetic proxy request")
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
fn resource_pause_retains_bounded_user_work_and_resumes_dispatch() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(2, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    service.set_resource_policy(false, 1, false);
    let mut files = TestFiles(Vec::new());
    let asset_id = AssetId::new();

    assert!(matches!(
        service.request(
            asset_id,
            files.create(99),
            config(),
            color(),
            ProxyGenerationOrigin::User,
        ),
        ProxyGenerationRequestOutcome::Admitted { .. }
    ));
    std::thread::sleep(Duration::from_millis(20));
    assert!(backend.started().is_empty());
    let paused = service.diagnostics();
    assert!(!paused.dispatch_enabled);
    assert_eq!(paused.dispatch_parallelism, 1);
    assert_eq!(paused.queued, 1);

    service.set_resource_policy(true, 1, true);
    backend.release(1);
    assert_eq!(backend.wait_started(1), vec![asset_id]);
    let completed = wait_diagnostics(&service, |diagnostics| diagnostics.completions == 1);
    assert!(completed.dispatch_enabled);
    assert!(completed
        .terminal_records
        .last()
        .expect("completion terminal")
        .publication
        .is_some());
}

#[test]
fn resource_pause_yields_a_running_attempt_and_resumes_the_same_demand() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut files = TestFiles(Vec::new());
    let asset_id = AssetId::new();

    assert!(matches!(
        service.request(
            asset_id,
            files.create(109),
            config(),
            color(),
            ProxyGenerationOrigin::User,
        ),
        ProxyGenerationRequestOutcome::Admitted { .. }
    ));
    assert_eq!(backend.wait_started(1), vec![asset_id]);

    service.set_resource_policy(false, 1, false);
    let yielded = wait_diagnostics(&service, |diagnostics| {
        diagnostics.resource_yields == 1 && diagnostics.running == 0 && diagnostics.queued == 1
    });
    assert_eq!(yielded.cancellations, 0);
    assert_eq!(yielded.terminal_records.len(), 0);

    service.set_resource_policy(true, 1, true);
    assert_eq!(backend.wait_started(2), vec![asset_id, asset_id]);
    backend.release(1);
    let completed = wait_diagnostics(&service, |diagnostics| diagnostics.completions == 1);
    assert_eq!(completed.resource_yields, 1);
    assert_eq!(completed.cancellations, 0);
    assert_eq!(completed.terminal_records.len(), 1);
}

#[test]
fn policy_and_yield_are_diagnostics_only_while_attempt_lifecycle_changes_model() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    assert!(service.poll_finished());
    assert!(!service.poll_finished());

    let initial_revision = service.diagnostics().revision;
    service.set_resource_policy(false, 1, false);
    let paused_revision = service.diagnostics().revision;
    assert_ne!(paused_revision, initial_revision);
    assert!(!service.poll_finished());

    service.set_resource_policy(false, 1, false);
    assert_eq!(service.diagnostics().revision, paused_revision);
    assert!(!service.poll_finished());

    let mut files = TestFiles(Vec::new());
    let asset_id = AssetId::new();
    assert!(matches!(
        service.request(
            asset_id,
            files.create(110),
            config(),
            color(),
            ProxyGenerationOrigin::User,
        ),
        ProxyGenerationRequestOutcome::Admitted { .. }
    ));
    assert!(service.poll_finished());
    assert!(!service.poll_finished());

    service.set_resource_policy(true, 1, true);
    assert_eq!(backend.wait_started(1), vec![asset_id]);
    wait_model_change(&service);
    assert!(!service.poll_finished());

    let running_revision = service.diagnostics().revision;
    service.set_resource_policy(false, 1, false);
    let yielded = wait_diagnostics(&service, |diagnostics| {
        diagnostics.resource_yields == 1
            && diagnostics.running == 0
            && diagnostics.queued == 1
            && diagnostics.revision != running_revision
    });
    assert_eq!(yielded.cancellations, 0);
    assert!(yielded.terminal_records.is_empty());
    assert!(!service.poll_finished());

    service.set_resource_policy(true, 1, true);
    assert_eq!(backend.wait_started(2), vec![asset_id, asset_id]);
    wait_model_change(&service);
    assert!(!service.poll_finished());

    backend.release(1);
    wait_diagnostics(&service, |diagnostics| diagnostics.completions == 1);
    wait_model_change(&service);
    assert!(!service.poll_finished());
}

#[test]
fn automatic_pause_dispatches_user_request_before_retained_import_work() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    service.set_resource_policy(false, 1, false);
    let mut files = TestFiles(Vec::new());
    let automatic_asset = AssetId::new();
    let user_asset = AssetId::new();

    for (asset_id, origin, seed) in [
        (automatic_asset, ProxyGenerationOrigin::Import, 100),
        (user_asset, ProxyGenerationOrigin::User, 101),
    ] {
        assert!(matches!(
            service.request(asset_id, files.create(seed), config(), color(), origin),
            ProxyGenerationRequestOutcome::Admitted { .. }
        ));
    }

    service.set_resource_policy(true, 1, false);
    assert_eq!(backend.wait_started(1), vec![user_asset]);
    backend.release(1);
    let explicit = wait_diagnostics(&service, |diagnostics| diagnostics.completions == 1);
    assert_eq!(explicit.queued, 1);
    assert_eq!(explicit.queued_user, 0);
    assert!(!explicit.automatic_dispatch_enabled);

    service.set_resource_policy(true, 1, true);
    assert_eq!(backend.wait_started(2), vec![user_asset, automatic_asset]);
    backend.release(1);
    let complete = wait_diagnostics(&service, |diagnostics| diagnostics.completions == 2);
    assert_eq!(complete.queued, 0);
}

#[test]
fn automatic_pause_yields_only_running_automatic_work() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(2, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut files = TestFiles(Vec::new());
    let automatic_asset = AssetId::new();
    let user_asset = AssetId::new();

    for (asset_id, origin, seed) in [
        (automatic_asset, ProxyGenerationOrigin::Import, 102),
        (user_asset, ProxyGenerationOrigin::User, 103),
    ] {
        assert!(matches!(
            service.request(asset_id, files.create(seed), config(), color(), origin),
            ProxyGenerationRequestOutcome::Admitted { .. }
        ));
    }
    let started = backend.wait_started(2);
    assert!(started.contains(&automatic_asset));
    assert!(started.contains(&user_asset));

    service.set_resource_policy(true, 2, false);
    let yielded = wait_diagnostics(&service, |diagnostics| {
        diagnostics.resource_yields == 1 && diagnostics.running == 1 && diagnostics.queued == 1
    });
    assert_eq!(yielded.running_user, 1);
    assert_eq!(yielded.queued_user, 0);
    assert_eq!(yielded.cancellations, 0);
    assert_eq!(yielded.terminal_records.len(), 0);

    backend.release(1);
    let user_completed = wait_diagnostics(&service, |diagnostics| diagnostics.completions == 1);
    assert_eq!(user_completed.queued, 1);
    assert!(!user_completed.automatic_dispatch_enabled);
    assert_eq!(
        backend.started().iter().filter(|asset| **asset == user_asset).count(),
        1
    );

    service.set_resource_policy(true, 2, true);
    let resumed = backend.wait_started(3);
    assert_eq!(
        resumed.iter().filter(|asset| **asset == automatic_asset).count(),
        2
    );
    backend.release(1);
    let completed = wait_diagnostics(&service, |diagnostics| diagnostics.completions == 2);
    assert_eq!(completed.resource_yields, 1);
    assert_eq!(completed.cancellations, 0);
}

#[test]
fn running_resource_yield_promoted_to_user_requeues_with_user_evidence() {
    let backend = FakeBackend::new();
    backend.defer_cancellation_return();
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut files = TestFiles(Vec::new());
    let asset_id = AssetId::new();
    let path = files.create(104);

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
    assert_eq!(backend.wait_started(1), vec![asset_id]);

    service.set_resource_policy(true, 1, false);
    let yielding = wait_diagnostics(&service, |diagnostics| diagnostics.yielding == 1);
    assert_eq!(yielding.running_user, 0);
    assert_eq!(
        service.request(
            asset_id,
            path,
            config(),
            color(),
            ProxyGenerationOrigin::User,
        ),
        ProxyGenerationRequestOutcome::Deduplicated { promoted: true }
    );
    let promoted = service.diagnostics();
    assert_eq!(promoted.promotions, 1);
    assert_eq!(promoted.running_user, 1);
    assert_eq!(promoted.yielding, 1);

    backend.allow_cancellation_return();
    assert_eq!(backend.wait_started(2), vec![asset_id, asset_id]);
    let resumed = service.diagnostics();
    assert_eq!(resumed.resource_yields, 1);
    assert_eq!(resumed.running_user, 1);
    assert_eq!(resumed.queued_user, 0);
    assert!(!resumed.automatic_dispatch_enabled);

    backend.release(1);
    let completed = wait_diagnostics(&service, |diagnostics| diagnostics.completions == 1);
    let terminal = completed.terminal_records.last().expect("completion terminal");
    assert_eq!(terminal.origin, ProxyGenerationOrigin::User);
    assert_eq!(
        terminal.evidence.priority,
        mondrian_core::ExecutionPriority::UserInitiated
    );
    assert_eq!(
        terminal.evidence.disposition,
        ExecutionTerminalDisposition::Completed
    );
}

#[test]
fn running_user_promotion_is_not_yielded_by_later_automatic_pause() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut files = TestFiles(Vec::new());
    let asset_id = AssetId::new();
    let path = files.create(105);

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
    assert_eq!(backend.wait_started(1), vec![asset_id]);
    assert_eq!(
        service.request(
            asset_id,
            path,
            config(),
            color(),
            ProxyGenerationOrigin::User,
        ),
        ProxyGenerationRequestOutcome::Deduplicated { promoted: true }
    );

    service.set_resource_policy(true, 1, false);
    std::thread::sleep(Duration::from_millis(20));
    let running = service.diagnostics();
    assert_eq!(running.running, 1);
    assert_eq!(running.running_user, 1);
    assert_eq!(running.yielding, 0);
    assert_eq!(running.resource_yields, 0);

    backend.release(1);
    let completed = wait_diagnostics(&service, |diagnostics| diagnostics.completions == 1);
    assert_eq!(backend.started(), vec![asset_id]);
    assert_eq!(
        completed.terminal_records.last().map(|terminal| terminal.origin),
        Some(ProxyGenerationOrigin::User)
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
fn terminal_delta_reports_bounded_retention_gap_without_replaying_consumed_records() {
    let mut state = ProxyGenerationState::default();
    for _ in 0..=PROXY_TERMINAL_CAPACITY {
        record_immediate_failure(
            &mut state,
            None,
            AssetId::new(),
            MediaFileFingerprint::default(),
            ProxyGenerationOrigin::Import,
            ProxyGenerationFailure::new(
                ProxyGenerationFailureReason::GenerationFailed,
                "bounded terminal fixture",
            ),
        );
    }

    let expected_latest = (PROXY_TERMINAL_CAPACITY + 1) as u64;
    let diagnostics = diagnostics_snapshot(&state);
    assert_eq!(diagnostics.terminal_records.len(), PROXY_TERMINAL_CAPACITY);
    assert_eq!(diagnostics.oldest_terminal_sequence, Some(2));
    assert_eq!(diagnostics.latest_terminal_sequence, expected_latest);

    let initial = terminal_delta_snapshot(&state, 0);
    assert!(initial.retention_gap);
    assert_eq!(initial.next_cursor, expected_latest);
    assert_eq!(initial.records.len(), PROXY_TERMINAL_CAPACITY);
    assert_eq!(
        initial.records.first().map(|record| record.terminal_sequence),
        Some(2)
    );
    assert_eq!(
        initial.records.last().map(|record| record.terminal_sequence),
        Some(expected_latest)
    );

    let consumed = terminal_delta_snapshot(&state, initial.next_cursor);
    assert!(!consumed.retention_gap);
    assert!(consumed.records.is_empty());
    assert_eq!(consumed.next_cursor, expected_latest);
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
fn deduplication_is_diagnostic_but_priority_promotion_changes_the_model() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(1, backend);
    service.bind_project(Some(ProjectId::new()));
    assert!(service.poll_finished());
    assert!(!service.poll_finished());
    service.set_resource_policy(false, 1, false);
    assert!(!service.poll_finished());

    let mut files = TestFiles(Vec::new());
    let asset_id = AssetId::new();
    let source_path = files.create(111);
    assert!(matches!(
        service.request(
            asset_id,
            source_path.clone(),
            config(),
            color(),
            ProxyGenerationOrigin::Import,
        ),
        ProxyGenerationRequestOutcome::Admitted { .. }
    ));
    assert!(service.poll_finished());
    assert!(!service.poll_finished());

    let admitted_revision = service.diagnostics().revision;
    assert_eq!(
        service.request(
            asset_id,
            source_path.clone(),
            config(),
            color(),
            ProxyGenerationOrigin::Import,
        ),
        ProxyGenerationRequestOutcome::Deduplicated { promoted: false }
    );
    let deduplicated = service.diagnostics();
    assert_ne!(deduplicated.revision, admitted_revision);
    assert_eq!(deduplicated.deduplications, 1);
    assert_eq!(deduplicated.promotions, 0);
    assert!(!service.poll_finished());

    assert_eq!(
        service.request(
            asset_id,
            source_path,
            config(),
            color(),
            ProxyGenerationOrigin::User,
        ),
        ProxyGenerationRequestOutcome::Deduplicated { promoted: true }
    );
    let promoted = service.diagnostics();
    assert_eq!(promoted.deduplications, 2);
    assert_eq!(promoted.promotions, 1);
    assert_eq!(promoted.queued_user, 1);
    assert!(service.poll_finished());
    assert!(!service.poll_finished());
}

#[test]
fn queued_user_promotion_wakes_dispatch_while_automatic_work_is_paused() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    service.set_resource_policy(true, 1, false);
    let mut files = TestFiles(Vec::new());
    let asset_id = AssetId::new();
    let path = files.create(3);

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
    std::thread::sleep(Duration::from_millis(20));
    assert!(backend.started().is_empty());

    assert_eq!(
        service.request(
            asset_id,
            path,
            config(),
            color(),
            ProxyGenerationOrigin::User,
        ),
        ProxyGenerationRequestOutcome::Deduplicated { promoted: true }
    );
    assert_eq!(backend.wait_started(1), vec![asset_id]);
    let running = service.diagnostics();
    assert_eq!(running.promotions, 1);
    assert_eq!(running.running_user, 1);
    assert_eq!(running.queued, 0);

    backend.release(1);
    let completed = wait_diagnostics(&service, |diagnostics| diagnostics.completions == 1);
    assert_eq!(
        completed.terminal_records.last().map(|terminal| terminal.origin),
        Some(ProxyGenerationOrigin::User)
    );
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
    let project_id = ProjectId::new();
    service.bind_project(Some(project_id));
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

    service.bind_project(Some(project_id));
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

fn publication_failure(kind: ProxyPublicationFailureKind) -> ProxyGenerationFailure {
    ProxyGenerationFailure::publication(ProxyPublicationFailure {
        phase: ProxyPublicationPhase::Media,
        kind,
        target_path: std::env::temp_dir().join("proxy-publication.mp4"),
        retained_new_path: None,
        detail: format!("synthetic {kind:?}"),
    })
}

#[test]
fn before_namespace_publication_failure_allows_exact_automatic_retry() {
    let backend = FakeBackend::new();
    backend.push_outcome(Err(publication_failure(
        ProxyPublicationFailureKind::BeforeNamespace,
    )));
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut files = TestFiles(Vec::new());
    let path = files.create(51);
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
    backend.release(1);
    let failed = wait_diagnostics(&service, |diagnostics| diagnostics.failures == 1);
    let terminal = failed.terminal_records.last().expect("failure terminal");
    assert_eq!(
        terminal.publication_failure.as_ref().map(|failure| failure.kind),
        Some(ProxyPublicationFailureKind::BeforeNamespace)
    );

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
    backend.release(1);
}

#[test]
fn unknown_publication_state_is_quarantined_until_exact_freshness_revalidation() {
    let backend = FakeBackend::new();
    backend.push_outcome(Err(publication_failure(
        ProxyPublicationFailureKind::NamespaceIndeterminate,
    )));
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut files = TestFiles(Vec::new());
    let path = files.create(52);
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
    service.bind_project(Some(ProjectId::new()));

    let quarantined = service.request(
        asset_id,
        path.clone(),
        config(),
        color(),
        ProxyGenerationOrigin::Import,
    );
    assert!(matches!(
        &quarantined,
        ProxyGenerationRequestOutcome::RetainedFailure(failure)
            if matches!(
                failure.publication.as_deref(),
                Some(ProxyPublicationFailure {
                kind: ProxyPublicationFailureKind::NamespaceIndeterminate,
                ..
                })
            )
    ));
    let ProxyGenerationRequestOutcome::RetainedFailure(failure) = quarantined else {
        panic!("expected quarantined publication failure");
    };
    assert!(failure.detail.contains("quarantined"));
    assert!(!failure.detail.contains("may be retried"));
    assert!(matches!(
        service.request(
            asset_id,
            path.clone(),
            config(),
            color(),
            ProxyGenerationOrigin::User,
        ),
        ProxyGenerationRequestOutcome::RetainedFailure(_)
    ));

    *backend.status.lock() = ProxyStatus::Fresh;
    assert!(matches!(
        service.request(
            asset_id,
            path,
            config(),
            color(),
            ProxyGenerationOrigin::User,
        ),
        ProxyGenerationRequestOutcome::AlreadyFresh
    ));
    assert_eq!(service.diagnostics().retained_failures, 0);
}

#[test]
fn durability_unconfirmed_publication_is_not_cleared_by_user_priority() {
    let backend = FakeBackend::new();
    backend.push_outcome(Err(publication_failure(
        ProxyPublicationFailureKind::DurabilityUnconfirmed,
    )));
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut files = TestFiles(Vec::new());
    let path = files.create(54);
    let asset_id = AssetId::new();

    service.request(
        asset_id,
        path.clone(),
        config(),
        color(),
        ProxyGenerationOrigin::Import,
    );
    backend.release(1);
    let failed = wait_diagnostics(&service, |diagnostics| diagnostics.failures == 1);
    assert_eq!(
        failed
            .terminal_records
            .last()
            .and_then(|terminal| terminal.publication_failure.as_ref())
            .map(|failure| failure.kind),
        Some(ProxyPublicationFailureKind::DurabilityUnconfirmed)
    );

    let retained = service.request(
        asset_id,
        path,
        config(),
        color(),
        ProxyGenerationOrigin::User,
    );
    assert!(matches!(
        retained,
        ProxyGenerationRequestOutcome::RetainedFailure(failure)
            if matches!(
                failure.publication.as_deref(),
                Some(ProxyPublicationFailure {
                kind: ProxyPublicationFailureKind::DurabilityUnconfirmed,
                ..
                })
            )
    ));
    assert_eq!(backend.started().len(), 1);
}

#[test]
fn service_admission_freezes_relative_cache_root_as_absolute() {
    let backend = FakeBackend::new();
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut files = TestFiles(Vec::new());
    let mut relative = config();
    relative.cache_dir = PathBuf::from("target").join("proxy-admission-cache");

    let outcome = service.request(
        AssetId::new(),
        files.create(53),
        relative,
        color(),
        ProxyGenerationOrigin::Import,
    );

    assert!(matches!(
        outcome,
        ProxyGenerationRequestOutcome::Admitted { .. }
    ));
    let cache_root = backend.last_cache_root().expect("observed cache root");
    assert!(cache_root.is_absolute());
    assert!(cache_root.ends_with(PathBuf::from("target").join("proxy-admission-cache")));
    backend.release(1);
}

#[test]
fn internal_request_identity_rejects_unfrozen_relative_cache_root() {
    let unfrozen = ProxyConfig {
        cache_dir: PathBuf::from("relative-proxy-cache"),
        ..ProxyConfig::default()
    };

    let error = ProxyGenerationRequest::new(
        AssetId::new(),
        PathBuf::from("source.mov"),
        MediaFileFingerprint::default(),
        unfrozen,
        color(),
    )
    .expect_err("request identity must not retain process-relative cache state");

    assert_eq!(
        error.reason,
        ProxyGenerationFailureReason::InvalidProxyContract
    );
    assert!(error.detail.contains("absolute frozen cache root"));
}

#[test]
fn model_revision_is_consumed_once_by_event_loop_poll() {
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

#[test]
fn app_poll_does_not_report_policy_diagnostics_as_a_model_change() {
    let backend = FakeBackend::new();
    backend.push_outcome(Err(ProxyGenerationFailure::new(
        ProxyGenerationFailureReason::GenerationFailed,
        "single terminal failure",
    )));
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut state = AppState::new();
    state.proxy_generation = service;
    let mut files = TestFiles(Vec::new());

    state.request_proxy_generation(
        AssetId::new(),
        files.create(91),
        config(),
        color(),
        ProxyGenerationOrigin::Import,
    );
    backend.release(1);
    wait_diagnostics(&state.proxy_generation, |diagnostics| {
        diagnostics.failures == 1
    });

    assert!(state.poll_proxy_generation());
    let first_log_len = state.status_log.len();
    assert_eq!(first_log_len, 1);
    let terminal_cursor = state.proxy_generation_diagnostics().latest_terminal_sequence;
    assert_eq!(state.proxy_terminal_observed_sequence, terminal_cursor);

    state.proxy_generation.set_resource_policy(false, 1, false);
    assert!(!state.poll_proxy_generation());
    assert!(!state.poll_proxy_generation());
    assert!(!state.poll_proxy_generation());
    assert_eq!(state.status_log.len(), first_log_len);
    assert_eq!(
        state.proxy_generation_diagnostics().latest_terminal_sequence,
        terminal_cursor
    );
    assert_eq!(state.proxy_terminal_observed_sequence, terminal_cursor);
}

#[test]
fn app_poll_does_not_project_unconsumed_failure_from_retired_generation() {
    let backend = FakeBackend::new();
    backend.push_outcome(Err(ProxyGenerationFailure::new(
        ProxyGenerationFailureReason::GenerationFailed,
        "retired generation failure",
    )));
    let service = ProxyGenerationService::with_backend(1, backend.clone());
    service.bind_project(Some(ProjectId::new()));
    let mut state = AppState::new();
    state.proxy_generation = service;
    let mut files = TestFiles(Vec::new());

    state.request_proxy_generation(
        AssetId::new(),
        files.create(92),
        config(),
        color(),
        ProxyGenerationOrigin::Import,
    );
    backend.release(1);
    wait_diagnostics(&state.proxy_generation, |diagnostics| {
        diagnostics.failures == 1
    });
    state.proxy_generation.bind_project(Some(ProjectId::new()));

    assert!(state.poll_proxy_generation());
    assert!(state.status_hint.is_none());
    assert!(state.status_log.is_empty());
    assert_eq!(
        state.proxy_terminal_observed_sequence,
        state.proxy_generation_diagnostics().latest_terminal_sequence
    );
}
