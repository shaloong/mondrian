//! Low-frequency external-dependency observation for immutable visual programs.
//!
//! Timeline evaluation only binds immutable [`PreparedVisualProgram`] values.
//! This Adapter moves filesystem/plugin revalidation off the UI and frame
//! execution domains, then reports exact program instances that must be
//! evicted. It never interprets Timeline or Effect author state.

use mondrian_core::{SequenceId, SequenceRevision};
use mondrian_renderer::PreparedVisualProgram;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::preview_runtime::PreviewOwnedWorkerShutdown;
use super::preview_work_notification::PreviewWorkNotifier;

const OBSERVED_PROGRAM_CAPACITY: usize = 128;
const OBSERVATION_COMMAND_CAPACITY: usize = 128;
const REFRESH_RESULT_CAPACITY: usize = 32;
const INITIAL_CHECK_DELAY: Duration = Duration::from_millis(750);
const STABLE_CHECK_INTERVAL: Duration = Duration::from_secs(5);
const RESULT_BACKPRESSURE_RETRY: Duration = Duration::from_millis(250);
const WORKER_SHUTDOWN_POLL: Duration = Duration::from_millis(250);
const WORKER_JOIN_GRACE: Duration = WORKER_SHUTDOWN_POLL;

/// Exact immutable visual program that a Preview consumer must evict.
#[derive(Debug, Clone)]
pub(crate) struct PreviewVisualDependencyRefresh {
    pub(crate) sequence_id: SequenceId,
    pub(crate) sequence_revision: SequenceRevision,
    pub(crate) effect_registry_revision: u64,
    pub(crate) reason: Arc<str>,
}

enum ObservationCommand {
    Observe(Arc<PreparedVisualProgram>),
    Forget(SequenceId),
    Shutdown,
}

struct ObservationEntry {
    program: Arc<PreparedVisualProgram>,
    next_check: Instant,
    pending_refresh_reason: Option<Arc<str>>,
}

#[derive(Debug, Clone, Copy)]
struct ObservationTiming {
    initial_delay: Duration,
    stable_interval: Duration,
    result_retry: Duration,
    shutdown_poll: Duration,
}

impl Default for ObservationTiming {
    fn default() -> Self {
        Self {
            initial_delay: INITIAL_CHECK_DELAY,
            stable_interval: STABLE_CHECK_INTERVAL,
            result_retry: RESULT_BACKPRESSURE_RETRY,
            shutdown_poll: WORKER_SHUTDOWN_POLL,
        }
    }
}

/// Consumer-owned observer for the exact visual programs used by Preview.
///
/// `observe` and `poll_refreshes` are non-blocking. The worker retains at most
/// [`OBSERVED_PROGRAM_CAPACITY`] latest Sequence programs and performs all
/// resource reads away from the UI and render hot paths.
pub(crate) struct PreviewVisualDependencyObserver {
    command_tx: mpsc::SyncSender<ObservationCommand>,
    result_rx: RefCell<mpsc::Receiver<DependencyRefreshResult>>,
    observed: RefCell<HashMap<SequenceId, Arc<PreparedVisualProgram>>>,
    pending: RefCell<HashMap<SequenceId, Arc<PreparedVisualProgram>>>,
    recency: RefCell<VecDeque<SequenceId>>,
    shutdown: Arc<AtomicBool>,
    healthy: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

struct DependencyRefreshResult {
    program: Arc<PreparedVisualProgram>,
    reason: Arc<str>,
}

enum DueObservationOutcome {
    Retained,
    Published,
    ReceiverDisconnected,
}

impl PreviewVisualDependencyObserver {
    /// Start an observer publishing refresh readiness into a shared work watch.
    pub(crate) fn new_with_notifier(work_notifier: PreviewWorkNotifier) -> Self {
        Self::with_configuration(
            ObservationTiming::default(),
            OBSERVATION_COMMAND_CAPACITY,
            REFRESH_RESULT_CAPACITY,
            work_notifier,
        )
    }

    #[cfg(test)]
    fn with_timing(timing: ObservationTiming) -> Self {
        Self::with_timing_and_notifier(timing, PreviewWorkNotifier::default())
    }

    #[cfg(test)]
    fn with_timing_and_notifier(
        timing: ObservationTiming,
        work_notifier: PreviewWorkNotifier,
    ) -> Self {
        Self::with_configuration(
            timing,
            OBSERVATION_COMMAND_CAPACITY,
            REFRESH_RESULT_CAPACITY,
            work_notifier,
        )
    }

    fn with_configuration(
        timing: ObservationTiming,
        command_capacity: usize,
        result_capacity: usize,
        work_notifier: PreviewWorkNotifier,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::sync_channel::<ObservationCommand>(command_capacity);
        let (result_tx, result_rx) = mpsc::sync_channel::<DependencyRefreshResult>(result_capacity);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let healthy = Arc::new(AtomicBool::new(true));
        let worker_health = Arc::clone(&healthy);
        let worker = std::thread::Builder::new()
            .name("mondrian-preview-visual-dependencies".to_owned())
            .spawn(move || {
                let _health_guard = WorkerHealthGuard(worker_health);
                dependency_observer_worker(
                    command_rx,
                    result_tx,
                    work_notifier,
                    worker_shutdown,
                    timing,
                );
            })
            .ok();
        if worker.is_none() {
            healthy.store(false, Ordering::Release);
            tracing::warn!(
                "failed to start Preview visual dependency observer; dependent Preview execution will fail closed"
            );
        }
        Self {
            command_tx,
            result_rx: RefCell::new(result_rx),
            observed: RefCell::new(HashMap::new()),
            pending: RefCell::new(HashMap::new()),
            recency: RefCell::new(VecDeque::new()),
            shutdown,
            healthy,
            worker,
        }
    }

    /// Whether the background dependency observer can still provide
    /// fail-closed invalidation evidence.
    pub(crate) fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    /// Close observation admission before any Preview owner starts joining.
    pub(crate) fn begin_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = self.command_tx.try_send(ObservationCommand::Shutdown);
    }

    /// Consume the actual handle with the caller's unchanged absolute deadline.
    pub(crate) fn shutdown_until(&mut self, deadline: Instant) -> PreviewOwnedWorkerShutdown {
        self.begin_shutdown();
        self.worker.take().map_or(PreviewOwnedWorkerShutdown::NotStarted, |worker| {
            PreviewOwnedWorkerShutdown::join_until(worker, deadline)
        })
    }

    /// Join the actual worker for the explicitly unbounded shutdown interface.
    pub(crate) fn shutdown_and_wait(&mut self) -> PreviewOwnedWorkerShutdown {
        self.begin_shutdown();
        self.worker.take().map_or(
            PreviewOwnedWorkerShutdown::NotStarted,
            PreviewOwnedWorkerShutdown::join,
        )
    }

    /// Observe the exact immutable program used for one Sequence evaluation.
    ///
    /// Repeated frame evaluations of the same `Arc` are deduplicated without
    /// touching the worker queue.
    pub(crate) fn observe(&self, program: Arc<PreparedVisualProgram>) {
        let sequence_id = program.sequence_id();
        if self
            .observed
            .borrow()
            .get(&sequence_id)
            .is_some_and(|current| Arc::ptr_eq(current, &program))
        {
            return;
        }

        if !self.observed.borrow().contains_key(&sequence_id)
            && self.observed.borrow().len() >= OBSERVED_PROGRAM_CAPACITY
        {
            self.evict_oldest_observation();
        }
        self.observed.borrow_mut().insert(sequence_id, Arc::clone(&program));
        self.pending.borrow_mut().insert(sequence_id, program);
        self.touch(sequence_id);
        self.flush_pending();
    }

    /// Drain exact refresh evidence produced by the background observer.
    ///
    /// Results for superseded program instances are discarded. A caller can
    /// therefore invalidate a Sequence cache without racing a newer author
    /// revision or a newly prepared external-resource snapshot.
    pub(crate) fn poll_refreshes(&self) -> Vec<PreviewVisualDependencyRefresh> {
        self.flush_pending();
        let mut refreshes = Vec::new();
        loop {
            let result = match self.result_rx.borrow_mut().try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            };
            let sequence_id = result.program.sequence_id();
            let is_current = self
                .observed
                .borrow()
                .get(&sequence_id)
                .is_some_and(|current| Arc::ptr_eq(current, &result.program));
            if !is_current {
                continue;
            }
            self.observed.borrow_mut().remove(&sequence_id);
            self.pending.borrow_mut().remove(&sequence_id);
            self.recency.borrow_mut().retain(|candidate| *candidate != sequence_id);
            refreshes.push(PreviewVisualDependencyRefresh {
                sequence_id,
                sequence_revision: result.program.sequence_revision(),
                effect_registry_revision: result.program.effect_registry_revision(),
                reason: result.reason,
            });
        }
        refreshes
    }

    fn flush_pending(&self) {
        loop {
            let Some((sequence_id, program)) = self
                .pending
                .borrow()
                .iter()
                .next()
                .map(|(sequence_id, program)| (*sequence_id, Arc::clone(program)))
            else {
                break;
            };
            match self.command_tx.try_send(ObservationCommand::Observe(Arc::clone(&program))) {
                Ok(()) => {
                    let mut pending = self.pending.borrow_mut();
                    if pending
                        .get(&sequence_id)
                        .is_some_and(|candidate| Arc::ptr_eq(candidate, &program))
                    {
                        pending.remove(&sequence_id);
                    }
                }
                Err(mpsc::TrySendError::Full(_)) => break,
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    self.healthy.store(false, Ordering::Release);
                    self.pending.borrow_mut().clear();
                    break;
                }
            }
        }
    }

    fn evict_oldest_observation(&self) {
        let Some(sequence_id) = self.recency.borrow_mut().pop_front() else {
            return;
        };
        self.observed.borrow_mut().remove(&sequence_id);
        self.pending.borrow_mut().remove(&sequence_id);
        let _ = self.command_tx.try_send(ObservationCommand::Forget(sequence_id));
    }

    fn touch(&self, sequence_id: SequenceId) {
        let mut recency = self.recency.borrow_mut();
        recency.retain(|candidate| *candidate != sequence_id);
        recency.push_back(sequence_id);
    }
}

struct WorkerHealthGuard(Arc<AtomicBool>);

impl Drop for WorkerHealthGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Drop for PreviewVisualDependencyObserver {
    fn drop(&mut self) {
        // Resource checks may be blocked in an operating-system filesystem
        // call. Give an ordinary worker a small bounded opportunity to finish,
        // but never make application shutdown depend on unbounded filesystem
        // latency.
        match self.shutdown_until(Instant::now() + WORKER_JOIN_GRACE) {
            PreviewOwnedWorkerShutdown::NotStarted | PreviewOwnedWorkerShutdown::Terminated => {}
            outcome => tracing::warn!(
                ?outcome,
                "Preview visual dependency observer did not return cleanly"
            ),
        }
    }
}

fn dependency_observer_worker(
    command_rx: mpsc::Receiver<ObservationCommand>,
    result_tx: mpsc::SyncSender<DependencyRefreshResult>,
    work_notifier: PreviewWorkNotifier,
    shutdown: Arc<AtomicBool>,
    timing: ObservationTiming,
) {
    let mut entries = HashMap::<SequenceId, ObservationEntry>::new();
    let mut recency = VecDeque::<SequenceId>::new();
    while !shutdown.load(Ordering::Acquire) {
        let timeout = next_worker_timeout(&entries, timing.shutdown_poll);
        match command_rx.recv_timeout(timeout) {
            Ok(ObservationCommand::Observe(program)) => {
                let sequence_id = program.sequence_id();
                let changed = entries
                    .get(&sequence_id)
                    .is_none_or(|entry| !Arc::ptr_eq(&entry.program, &program));
                if changed {
                    if !entries.contains_key(&sequence_id)
                        && entries.len() >= OBSERVED_PROGRAM_CAPACITY
                        && let Some(stale) = recency.pop_front()
                    {
                        entries.remove(&stale);
                    }
                    entries.insert(
                        sequence_id,
                        ObservationEntry {
                            program,
                            next_check: Instant::now() + timing.initial_delay,
                            pending_refresh_reason: None,
                        },
                    );
                }
                touch_worker_recency(&mut recency, sequence_id);
            }
            Ok(ObservationCommand::Forget(sequence_id)) => {
                entries.remove(&sequence_id);
                recency.retain(|candidate| *candidate != sequence_id);
            }
            Ok(ObservationCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        let now = Instant::now();
        let due = entries
            .iter()
            .filter_map(|(sequence_id, entry)| (entry.next_check <= now).then_some(*sequence_id))
            .collect::<Vec<_>>();
        for sequence_id in due {
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            let outcome = {
                let Some(entry) = entries.get_mut(&sequence_id) else {
                    continue;
                };
                process_due_observation(entry, &result_tx, now, timing)
            };
            match outcome {
                DueObservationOutcome::Retained => {}
                DueObservationOutcome::Published => {
                    work_notifier.result_became_pollable();
                    entries.remove(&sequence_id);
                    recency.retain(|candidate| *candidate != sequence_id);
                }
                DueObservationOutcome::ReceiverDisconnected => return,
            }
        }
    }
}

fn process_due_observation(
    entry: &mut ObservationEntry,
    result_tx: &mpsc::SyncSender<DependencyRefreshResult>,
    now: Instant,
    timing: ObservationTiming,
) -> DueObservationOutcome {
    let refresh = entry.pending_refresh_reason.clone().or_else(|| {
        match entry.program.dependency_refresh_required() {
            Ok(true) => Some(Arc::<str>::from(
                "Effect definition or external resource identity changed",
            )),
            Ok(false) => None,
            Err(error) => Some(Arc::<str>::from(error.to_string())),
        }
    });
    let Some(reason) = refresh else {
        entry.next_check = now + timing.stable_interval;
        return DueObservationOutcome::Retained;
    };
    match result_tx.try_send(DependencyRefreshResult {
        program: Arc::clone(&entry.program),
        reason: Arc::clone(&reason),
    }) {
        Ok(()) => DueObservationOutcome::Published,
        Err(mpsc::TrySendError::Full(_)) => {
            entry.pending_refresh_reason = Some(reason);
            entry.next_check = now + timing.result_retry;
            DueObservationOutcome::Retained
        }
        Err(mpsc::TrySendError::Disconnected(_)) => DueObservationOutcome::ReceiverDisconnected,
    }
}

fn next_worker_timeout(
    entries: &HashMap<SequenceId, ObservationEntry>,
    shutdown_poll: Duration,
) -> Duration {
    let now = Instant::now();
    entries
        .values()
        .map(|entry| entry.next_check.saturating_duration_since(now))
        .min()
        .unwrap_or(shutdown_poll)
        .min(shutdown_poll)
}

fn touch_worker_recency(recency: &mut VecDeque<SequenceId>, sequence_id: SequenceId) {
    recency.retain(|candidate| *candidate != sequence_id);
    recency.push_back(sequence_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_observer_shutdown_consumes_worker_exactly_once() {
        let mut observer = PreviewVisualDependencyObserver::with_timing(test_timing());
        assert_eq!(
            observer.shutdown_until(Instant::now() + Duration::from_secs(5)),
            PreviewOwnedWorkerShutdown::Terminated
        );
        assert_eq!(
            observer.shutdown_until(Instant::now()),
            PreviewOwnedWorkerShutdown::NotStarted
        );
    }

    #[test]
    fn dependency_observer_shutdown_retains_panic_and_does_not_renew_expired_deadline() {
        let mut observer = PreviewVisualDependencyObserver::with_timing(test_timing());
        assert_eq!(
            observer.shutdown_and_wait(),
            PreviewOwnedWorkerShutdown::Terminated
        );
        observer.worker = Some(std::thread::spawn(|| {
            panic!("injected dependency worker panic")
        }));
        assert_eq!(
            observer.shutdown_until(Instant::now() + Duration::from_secs(5)),
            PreviewOwnedWorkerShutdown::Panicked
        );
        let (release, released) = mpsc::channel();
        let (finished, finish) = mpsc::channel();
        observer.worker = Some(std::thread::spawn(move || {
            released.recv().expect("release controlled worker");
            finished.send(()).expect("observe return");
        }));
        assert_eq!(
            observer.shutdown_until(Instant::now()),
            PreviewOwnedWorkerShutdown::TimedOutDetached
        );
        release.send(()).expect("release test thread");
        finish.recv_timeout(Duration::from_secs(5)).expect("test thread returned");
    }
    use mondrian_core::automation::{ParameterResourceReference, PropertyValue};
    use mondrian_core::{AssetId, Color, FramePosition, Rational, TimelineTime};
    use mondrian_effects::{EffectNode, EffectNodeExt, EffectType};
    use mondrian_renderer::PreparedVisualProgram;
    use mondrian_timeline::{Clip, Sequence, Track};

    fn test_timing() -> ObservationTiming {
        ObservationTiming {
            initial_delay: Duration::ZERO,
            stable_interval: Duration::from_millis(5),
            result_retry: Duration::from_millis(2),
            shutdown_poll: Duration::from_millis(5),
        }
    }

    fn timeline_time(frame: i64, rate: Rational) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, rate)).expect("valid test time")
    }

    fn single_solid_sequence(effect: Option<EffectNode>) -> Sequence {
        let mut sequence = Sequence::new("dependency observer");
        sequence.video_tracks.clear();
        let rate = sequence.time_base();
        let mut track = Track::new_video("V1");
        let mut clip = Clip::new_solid_color(
            AssetId::new(),
            Color::BLACK,
            timeline_time(0, rate),
            timeline_time(20, rate),
        )
        .expect("solid Clip");
        if let Some(effect) = effect {
            clip.add_effect_node(effect);
        }
        track.add_clip(clip).expect("add solid Clip");
        sequence.video_tracks.push(track);
        sequence
    }

    fn prepare_stable(sequence: &Sequence) -> Arc<PreparedVisualProgram> {
        for _ in 0..32 {
            match PreparedVisualProgram::prepare(sequence) {
                Ok(program) => return Arc::new(program),
                Err(mondrian_renderer::PreparedVisualProgramError::EffectRegistryChanged {
                    ..
                }) => {}
                Err(error) => panic!("visual preparation failed: {error}"),
            }
        }
        panic!("Effect registry did not stabilize during test")
    }

    fn lut_effect(path: std::path::PathBuf) -> EffectNode {
        let mut lut = EffectNode::with_defaults(EffectType::Lut3D);
        let processing_space_id = EffectType::Lut3D
            .parameter_id("processing_space")
            .expect("processing-space parameter ID");
        let path_id = EffectType::Lut3D.parameter_id("path").expect("path parameter ID");
        lut.set_static_value_by_parameter(
            &processing_space_id,
            PropertyValue::Enum("scene_linear".to_owned()),
        )
        .expect("set processing space");
        lut.set_static_value_by_parameter(
            &path_id,
            PropertyValue::Resource(ParameterResourceReference::ExternalFile { path }),
        )
        .expect("bind LUT");
        lut
    }

    fn wait_for_refresh(
        observer: &PreviewVisualDependencyObserver,
    ) -> Option<PreviewVisualDependencyRefresh> {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if let Some(refresh) = observer.poll_refreshes().into_iter().next() {
                return Some(refresh);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }

    #[test]
    fn identity_program_does_not_publish_false_refresh() {
        let observer = PreviewVisualDependencyObserver::with_timing(test_timing());
        observer.observe(prepare_stable(&single_solid_sequence(None)));
        std::thread::sleep(Duration::from_millis(25));
        assert!(observer.poll_refreshes().is_empty());
    }

    #[test]
    fn retryable_external_blocker_publishes_exact_program_refresh() {
        let missing_path =
            std::env::temp_dir().join(format!("mondrian-observer-missing-{}.cube", AssetId::new()));
        let _ = std::fs::remove_file(&missing_path);
        let program = prepare_stable(&single_solid_sequence(Some(lut_effect(missing_path))));
        let expected_sequence_id = program.sequence_id();
        let expected_revision = program.sequence_revision();
        let work_notifier = PreviewWorkNotifier::default();
        let work_watch = work_notifier.watch();
        let work_revision_before = work_watch.revision();
        let observer =
            PreviewVisualDependencyObserver::with_timing_and_notifier(test_timing(), work_notifier);
        observer.observe(program);

        let refresh = wait_for_refresh(&observer).expect("external blocker refresh");
        assert_ne!(
            work_watch.wait_for_change(work_revision_before, Duration::from_secs(1)),
            work_revision_before
        );
        assert_eq!(refresh.sequence_id, expected_sequence_id);
        assert_eq!(refresh.sequence_revision, expected_revision);
    }

    #[test]
    fn result_backpressure_retries_the_cached_refresh_without_rechecking_the_resource() {
        const IDENTITY_LUT: &str = "LUT_3D_SIZE 2\n\
0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        const CHANGED_LUT: &str = "LUT_3D_SIZE 2\n\
1 1 1\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n0 0 0\n";
        let unique = AssetId::new();
        let path =
            std::env::temp_dir().join(format!("mondrian-observer-backpressure-{unique}.cube"));
        std::fs::write(&path, IDENTITY_LUT).expect("write initial LUT");
        let changed_program =
            prepare_stable(&single_solid_sequence(Some(lut_effect(path.clone()))));
        let changed_sequence_id = changed_program.sequence_id();
        std::fs::write(&path, CHANGED_LUT).expect("change observed LUT");
        let timing = test_timing();
        let now = Instant::now();
        let mut entry = ObservationEntry {
            program: Arc::clone(&changed_program),
            next_check: now,
            pending_refresh_reason: None,
        };
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        result_tx
            .try_send(DependencyRefreshResult {
                program: Arc::clone(&changed_program),
                reason: Arc::from("occupy the bounded result slot"),
            })
            .expect("fill result slot");

        assert!(matches!(
            process_due_observation(&mut entry, &result_tx, now, timing),
            DueObservationOutcome::Retained
        ));
        assert!(
            entry.pending_refresh_reason.is_some(),
            "backpressure must cache already-proven refresh evidence"
        );
        let _ = result_rx.try_recv().expect("drain occupying result");
        std::fs::write(&path, IDENTITY_LUT).expect("restore LUT before result retry");

        assert!(matches!(
            process_due_observation(&mut entry, &result_tx, Instant::now(), timing),
            DueObservationOutcome::Published
        ));
        let refreshed = result_rx.try_recv().expect("cached refresh result");
        assert_eq!(refreshed.program.sequence_id(), changed_sequence_id);
        std::fs::remove_file(path).expect("remove LUT");
    }

    #[test]
    fn ordinary_drop_joins_the_observer_worker_within_the_bounded_grace() {
        let observer = PreviewVisualDependencyObserver::with_timing(test_timing());
        let healthy = Arc::clone(&observer.healthy);

        drop(observer);

        assert!(
            !healthy.load(Ordering::Acquire),
            "the joined worker health guard must publish terminal state"
        );
    }
}
