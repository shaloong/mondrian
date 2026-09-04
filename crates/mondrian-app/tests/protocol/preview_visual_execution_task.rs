use super::*;
use mondrian_core::automation::{PropertyHost, PropertyMutation, PropertyValue};
use mondrian_core::{
    effect_data::{EffectNode, EffectType},
    FramePosition, Rational, TimelineTime, WorkingColorSpace, WorkingRgbaF32Frame,
};
use mondrian_effects::{
    EffectExecutionSessionConfig, EffectFrameExtent, EffectGraphExecutionBudget, EffectNodeExt,
    PreparedEffectProgram,
};
use mondrian_playback::PlaybackEngine;
use mondrian_renderer::{
    CpuColorFrame, HeterogeneousCpuPrefixBatchGrant, HeterogeneousCpuPrefixBatchItem,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

const WORKING_SPACE: WorkingColorSpace = WorkingColorSpace::LinearRec2020;

#[derive(Debug, Clone)]
struct ManualClock {
    nanoseconds: Arc<AtomicU64>,
}

impl ManualClock {
    fn new(value: MonotonicTimestamp) -> Self {
        Self {
            nanoseconds: Arc::new(AtomicU64::new(
                value.duration_since_origin().as_nanos().min(u128::from(u64::MAX)) as u64,
            )),
        }
    }

    fn set(&self, value: MonotonicTimestamp) {
        self.nanoseconds.store(
            value.duration_since_origin().as_nanos().min(u128::from(u64::MAX)) as u64,
            Ordering::Release,
        );
    }
}

impl MonotonicRuntimeClock for ManualClock {
    fn now(&self) -> MonotonicTimestamp {
        MonotonicTimestamp::from_duration(Duration::from_nanos(
            self.nanoseconds.load(Ordering::Acquire),
        ))
    }
}

fn timestamp(milliseconds: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_duration(Duration::from_millis(milliseconds))
}

fn epoch() -> PlaybackEpoch {
    PlaybackEngine::default().snapshot().epoch
}

fn demand_identity() -> FrameDemandIdentity {
    let frame_rate = Rational::new(1, 25);
    let mut engine = PlaybackEngine::new(frame_rate, mondrian_playback::PlaybackPolicy::default())
        .expect("playback engine");
    engine
        .play_timeline(
            mondrian_playback::PlaybackTimelineBinding::new(None, 0, frame_rate, 10)
                .expect("timeline binding"),
            FramePosition::new(0, frame_rate),
            MonotonicTimestamp::ZERO,
        )
        .expect("playback demand");
    engine.frame_demand().expect("frame demand").identity()
}

fn graph() -> std::sync::Arc<mondrian_effects::CompiledEffectGraph> {
    let blur = EffectNode::with_defaults(EffectType::GaussianBlur);
    let mut correction = EffectNode::with_defaults(EffectType::BasicCorrection);
    correction
        .apply_property_mutation(PropertyMutation::SetStaticValue {
            path: EffectType::BasicCorrection.property_path("exposure"),
            value: PropertyValue::Float(0.25),
        })
        .expect("set Basic Correction exposure");
    let mut grain = EffectNode::with_defaults(EffectType::Grain);
    grain
        .apply_property_mutation(PropertyMutation::SetStaticValue {
            path: EffectType::Grain.property_path("amount"),
            value: PropertyValue::Float(0.1),
        })
        .expect("set Grain amount");
    PreparedEffectProgram::prepare(&[blur, correction, grain], &[], WORKING_SPACE)
        .expect("prepare test graph")
        .evaluate(TimelineTime::ZERO)
        .expect("evaluate test graph")
}

fn grant() -> HeterogeneousCpuPrefixBatchGrant {
    HeterogeneousCpuPrefixBatchGrant::new(
        EffectExecutionSessionConfig::uncached(8 * 1024 * 1024),
        EffectGraphExecutionBudget::new(
            8 * 1024 * 1024,
            8 * 1024 * 1024,
            16 * 1024 * 1024,
            64,
            128,
        ),
        8,
        16 * 1024 * 1024,
    )
}

fn gpu_grant() -> HeterogeneousGpuResourceGrant {
    HeterogeneousGpuResourceGrant::new(8 * 1024 * 1024, 8 * 1024 * 1024, 64, 0)
}

fn item(address: u32) -> HeterogeneousCpuPrefixBatchItem {
    let grant = grant();
    let route = mondrian_renderer::PreparedHeterogeneousEffectRoute::prepare(
        graph(),
        EffectFrameExtent::new(2, 2),
        grant.graph_execution(),
    )
    .expect("prepare heterogeneous route");
    HeterogeneousCpuPrefixBatchItem::new(
        address,
        route,
        CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 2,
            height: 2,
            data: vec![
                [0.0, 0.1, 0.2, 1.0],
                [0.2, 0.3, 0.4, 1.0],
                [0.4, 0.5, 0.6, 1.0],
                [0.6, 0.7, 0.8, 1.0],
            ],
            color_space: WORKING_SPACE,
        }),
        WORKING_SPACE,
        17,
    )
}

fn payload(addresses: &[u32]) -> VisualExecutionTaskPayload {
    payload_with_protections(addresses, Vec::new())
}

fn payload_with_protections(
    addresses: &[u32],
    media_residency_protections: Vec<MediaFrameProtectionLease>,
) -> VisualExecutionTaskPayload {
    VisualExecutionTaskPayload::heterogeneous_cpu_prefix_batch(
        epoch(),
        HeterogeneousCpuPrefixBatchRequest::new(
            grant(),
            addresses.iter().copied().map(item).collect::<Vec<_>>(),
        ),
        gpu_grant(),
        media_residency_protections,
    )
}

fn media_protection() -> MediaFrameProtectionLease {
    type Store = mondrian_playback::PreviewFrameStore<u64, Vec<u8>, u64, Vec<u8>, ()>;
    let mut store = Store::new(mondrian_playback::PreviewFrameStoreConfig::default());
    let demand = mondrian_playback::MediaWorkDemandId::for_preview_generation(epoch(), 1, 0);
    let work = match store.reserve_media_work(
        &1,
        mondrian_playback::MediaWorkReservationIntent::Current(demand),
        1,
        0,
    ) {
        mondrian_playback::MediaWorkReservationAdmission::Reserved(work) => work,
        admission => panic!("test media work reservation failed: {admission:?}"),
    };
    assert!(store.admit_media_frame(1, vec![1], work, 1, 0).is_admitted());
    let (_payload, resource, protection) = store
        .protected_media_frame(&1, demand)
        .expect("test media protection admitted")
        .expect("test media frame resident");
    drop(resource);
    drop(store);
    protection
}

fn admission(
    fingerprint: [u8; 32],
    generation: u64,
    deadline: Option<FrameWorkDeadline<MonotonicTimestamp>>,
    payload: VisualExecutionTaskPayload,
) -> VisualExecutionAdmission {
    admission_with_demand(fingerprint, generation, deadline, None, payload)
}

fn admission_with_demand(
    fingerprint: [u8; 32],
    generation: u64,
    deadline: Option<FrameWorkDeadline<MonotonicTimestamp>>,
    demand_identity: Option<FrameDemandIdentity>,
    payload: VisualExecutionTaskPayload,
) -> VisualExecutionAdmission {
    VisualExecutionAdmission::new(
        VisualExecutionTaskKey::from_complete_semantic_fingerprint(fingerprint),
        generation,
        FrameWorkPriority::Current,
        FrameWorkClass::Playback,
        demand_identity,
        deadline,
        payload,
    )
}

fn assert_queued(submission: FrameWorkSubmission<VisualExecutionTaskKey>) {
    assert!(
        matches!(submission, FrameWorkSubmission::Queued { .. }),
        "expected queued visual work, got {submission:?}"
    );
}

fn wait_for_result(task: &VisualExecutionTask) -> VisualExecutionTaskResult {
    let started = Instant::now();
    loop {
        match task.try_poll() {
            VisualExecutionTaskPoll::Result(result) => return *result,
            VisualExecutionTaskPoll::Empty => {
                assert!(
                    started.elapsed() < Duration::from_secs(10),
                    "visual task did not publish before test timeout"
                );
                thread::sleep(Duration::from_millis(1));
            }
            VisualExecutionTaskPoll::Disconnected => {
                panic!("visual worker result transport disconnected")
            }
        }
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool, detail: &str) {
    let started = Instant::now();
    while !predicate() {
        assert!(started.elapsed() < Duration::from_secs(10), "{detail}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn broker_cancellation_and_deadline_stop_without_completed_evidence() {
    let clock = ManualClock::new(MonotonicTimestamp::ZERO);
    let task = VisualExecutionTask::new(clock.clone(), VisualExecutionTaskConfig::default())
        .expect("visual task");

    let canceled_gate = VisualExecutionTestGate::new();
    let canceled_key = VisualExecutionTaskKey::from_complete_semantic_fingerprint([1; 32]);
    assert_queued(task.submit(admission(
        [1; 32],
        1,
        None,
        payload(&[1]).with_gate(canceled_gate.clone()),
    )));
    canceled_gate.wait_until_entered();
    task.cancel_key(&canceled_key);
    canceled_gate.release();

    let VisualExecutionTaskResult::Failed(canceled) = wait_for_result(&task) else {
        panic!("Broker cancellation cannot publish a CPU-prefix success")
    };
    assert!(matches!(
        canceled.failure(),
        VisualExecutionTaskFailure::BrokerCanceled {
            evidence: FrameExecutionCancellationEvidence {
                cancellation: FrameExecutionCancellation::Superseded { .. },
                ..
            }
        }
    ));
    assert_eq!(canceled.terminal_evidence(), None);

    let deadline_gate = VisualExecutionTestGate::new();
    assert_queued(task.submit(admission(
        [2; 32],
        2,
        Some(FrameWorkDeadline::from_remaining(
            timestamp(10),
            Duration::from_millis(10),
        )),
        payload(&[2]).with_gate(deadline_gate.clone()),
    )));
    deadline_gate.wait_until_entered();
    clock.set(timestamp(10));
    deadline_gate.release();

    let VisualExecutionTaskResult::Failed(expired) = wait_for_result(&task) else {
        panic!("expired Broker work cannot publish a CPU-prefix success")
    };
    assert!(matches!(
        expired.failure(),
        VisualExecutionTaskFailure::BrokerCanceled {
            evidence: FrameExecutionCancellationEvidence {
                cancellation: FrameExecutionCancellation::DeadlineExpired { age: Duration::ZERO },
                ..
            }
        }
    ));
    assert_eq!(expired.terminal_evidence(), None);
    assert_eq!(task.diagnostics().pending_requests, 0);
    assert_eq!(task.diagnostics().in_flight_work, 0);
}

fn direct_lease(
    broker: &VisualExecutionBroker,
    fingerprint: [u8; 32],
    generation: u64,
) -> VisualExecutionLease {
    direct_lease_for_admission(
        broker,
        admission(fingerprint, generation, None, payload(&[7])),
    )
}

fn direct_lease_for_admission(
    broker: &VisualExecutionBroker,
    admission: VisualExecutionAdmission,
) -> VisualExecutionLease {
    let generation = admission.generation();
    let request = admission.into_frame_work_request();
    broker.prune_before(generation);
    assert_queued(broker.submit(request));
    let execution = match broker.receive(FrameWorkerLane::Any) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("expected ready direct visual lease, got {other:?}"),
    };
    let identity = VisualExecutionIdentity {
        key: execution.key,
        execution_id: execution.id,
        epoch: execution.payload.epoch(),
        generation: execution.generation,
        work_class: execution.work_class,
    };
    VisualExecutionLease::new(broker.clone(), identity)
}

#[test]
fn gpu_finalization_distinguishes_current_cache_only_and_stale() {
    let current_broker = FrameWorkBroker::new_with_clock(2, 2, ManualClock::new(timestamp(1)));
    let current = direct_lease(&current_broker, [3; 32], 3).finalize_gpu(true);
    assert!(current.completion_recorded());
    assert_eq!(
        current.resolution().completion,
        FrameRequestCompletion::Current
    );
    assert!(current.may_publish_current());
    assert!(current.may_cache());

    let cache_broker = FrameWorkBroker::new_with_clock(2, 2, ManualClock::new(timestamp(1)));
    let cache_lease = direct_lease(&cache_broker, [4; 32], 4);
    cache_broker.cancel_key(&cache_lease.key());
    let cache_only = cache_lease.finalize_gpu(true);
    assert_eq!(
        cache_only.resolution().completion,
        FrameRequestCompletion::CacheOnly
    );
    assert!(!cache_only.may_publish_current());
    assert!(cache_only.may_cache());

    let stale_broker = FrameWorkBroker::new_with_clock(2, 2, ManualClock::new(timestamp(1)));
    let stale_lease = direct_lease(&stale_broker, [5; 32], 5);
    stale_broker.prune_before(6);
    let stale = stale_lease.finalize_gpu(true);
    assert_eq!(stale.resolution().completion, FrameRequestCompletion::Stale);
    assert!(!stale.may_publish_current());
    assert!(!stale.may_cache());
}

#[test]
fn deadline_missed_current_exposes_demand_but_cannot_publish_or_cache() {
    let clock = ManualClock::new(timestamp(1));
    let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
    let demand = demand_identity();
    let lease = direct_lease_for_admission(
        &broker,
        VisualExecutionAdmission::new(
            VisualExecutionTaskKey::from_complete_semantic_fingerprint([8; 32]),
            8,
            FrameWorkPriority::Current,
            FrameWorkClass::Playback,
            Some(demand),
            Some(FrameWorkDeadline::from_remaining(
                timestamp(11),
                Duration::from_millis(10),
            )),
            payload(&[8]),
        ),
    );
    clock.set(timestamp(12));

    let finalized = lease.finalize_gpu(true);
    assert_eq!(
        finalized.resolution().completion,
        FrameRequestCompletion::Current
    );
    assert!(finalized.deadline_status().is_missed());
    assert_eq!(finalized.work_class(), FrameWorkClass::Playback);
    assert_eq!(finalized.demand_identity(), Some(demand));
    assert!(!finalized.may_publish_current());
    assert!(!finalized.may_cache());
}

#[test]
fn successful_prefix_keeps_lease_open_until_gpu_completion() {
    let work_notifier = PreviewWorkNotifier::default();
    let work_watch = work_notifier.watch();
    let work_revision_before = work_watch.revision();
    let task = VisualExecutionTask::new_with_notifier(
        ManualClock::new(timestamp(1)),
        VisualExecutionTaskConfig::default(),
        work_notifier,
    )
    .expect("visual task");
    assert_queued(task.submit(admission(
        [6; 32],
        6,
        None,
        payload_with_protections(&[11, 12], vec![media_protection()]),
    )));

    let VisualExecutionTaskResult::PrefixReady(ready) = wait_for_result(&task) else {
        panic!("expected successful prefix")
    };
    assert_ne!(
        work_watch.wait_for_change(work_revision_before, Duration::from_secs(1)),
        work_revision_before
    );
    assert_eq!(ready.key().semantic_fingerprint(), [6; 32]);
    assert_eq!(ready.generation(), 6);
    let diagnostics = task.diagnostics();
    assert_eq!(diagnostics.pending_requests, 1);
    assert_eq!(diagnostics.in_flight_work, 1);
    assert_eq!(diagnostics.in_flight_completed, 0);

    let (output, lease) = ready.into_parts();
    let (output, frozen_gpu_grant, media_residency_protections) =
        output.into_heterogeneous_cpu_prefix_batch();
    assert_eq!(frozen_gpu_grant, gpu_grant());
    assert_eq!(media_residency_protections.len(), 1);
    let completions = output.into_completions();
    assert_eq!(
        completions
            .iter()
            .map(mondrian_renderer::HeterogeneousCpuPrefixBatchCompletion::address)
            .collect::<Vec<_>>(),
        vec![11, 12]
    );
    let finalized = lease.finalize_gpu(true);
    assert!(finalized.may_publish_current());
    assert_eq!(task.diagnostics().pending_requests, 0);
    assert_eq!(task.diagnostics().in_flight_work, 0);
}

#[test]
fn gpu_failure_returns_latest_exact_playback_terminal_authority_once() {
    let task = VisualExecutionTask::new(
        ManualClock::new(timestamp(1)),
        VisualExecutionTaskConfig::default(),
    )
    .expect("visual task");
    let demand = demand_identity();
    assert_queued(task.submit(admission_with_demand(
        [16; 32],
        1,
        None,
        Some(demand),
        payload(&[1]),
    )));

    let VisualExecutionTaskResult::PrefixReady(ready) = wait_for_result(&task) else {
        panic!("expected successful prefix")
    };
    let (_, lease) = ready.into_parts();
    let failure = lease.fail_gpu();
    assert_eq!(failure.generation(), 1);
    assert_eq!(failure.work_class(), Some(FrameWorkClass::Playback));
    assert_eq!(failure.demand_identity(), Some(demand));
    assert_eq!(task.diagnostics().pending_requests, 0);
    assert_eq!(task.diagnostics().in_flight_work, 0);
}

#[test]
fn dropping_unfinished_lease_cleans_pending_and_in_flight_state() {
    let task = VisualExecutionTask::new(
        ManualClock::new(timestamp(1)),
        VisualExecutionTaskConfig::default(),
    )
    .expect("visual task");
    assert_queued(task.submit(admission([7; 32], 7, None, payload(&[21]))));
    let VisualExecutionTaskResult::PrefixReady(ready) = wait_for_result(&task) else {
        panic!("expected successful prefix")
    };
    drop(ready);
    assert_eq!(task.diagnostics().pending_requests, 0);
    assert_eq!(task.diagnostics().in_flight_work, 0);
}

#[test]
fn cross_binding_panic_preserves_fallback_and_worker_recovers() {
    let task = VisualExecutionTask::new(
        ManualClock::new(timestamp(1)),
        VisualExecutionTaskConfig::default(),
    )
    .expect("visual task");
    let gate = VisualExecutionTestGate::new();
    task.prune_before(8);
    assert_queued(task.submit(admission(
        [8; 32],
        8,
        None,
        payload(&[31]).with_gate(gate.clone()).with_panic_before_execution(),
    )));
    gate.wait_until_entered();
    assert_eq!(task.begin_generation(), 9);
    assert_queued(task.submit(admission([8; 32], 9, None, payload(&[31]))));
    gate.release();

    let VisualExecutionTaskResult::Failed(panicked) = wait_for_result(&task) else {
        panic!("expected isolated panic failure")
    };
    assert!(matches!(
        panicked.failure(),
        VisualExecutionTaskFailure::WorkerPanicked { detail }
            if detail.contains("before renderer execution")
    ));
    assert_eq!(
        panicked.terminal_evidence(),
        None,
        "an old failed attempt cannot terminate the queued replacement binding"
    );
    let VisualExecutionTaskResult::PrefixReady(replacement) = wait_for_result(&task) else {
        panic!("queued replacement did not execute after the old attempt failed")
    };
    assert_eq!(
        replacement.into_parts().1.finalize_gpu(true).resolution().completion,
        FrameRequestCompletion::Current
    );
    assert_eq!(task.diagnostics().pending_requests, 0);
    assert_eq!(task.diagnostics().in_flight_work, 0);

    assert_queued(task.submit(admission([9; 32], 10, None, payload(&[41]))));
    let VisualExecutionTaskResult::PrefixReady(recovered) = wait_for_result(&task) else {
        panic!("worker did not recover after isolated panic")
    };
    recovered.into_parts().1.finalize_gpu(true);
}

#[test]
fn result_disconnect_releases_blocked_and_queued_leases() {
    let task = VisualExecutionTask::new(
        ManualClock::new(timestamp(1)),
        VisualExecutionTaskConfig::new(4, 4, 1),
    )
    .expect("visual task");
    let broker = task.broker.clone();
    assert_queued(task.submit(admission([10; 32], 11, None, payload(&[51]))));
    assert_queued(task.submit(admission([11; 32], 11, None, payload(&[52]))));
    wait_until(
        || broker.diagnostics().in_flight_work >= 2,
        "worker never reached bounded result backpressure",
    );

    // Ordinary Drop deliberately detaches an active worker. This assertion
    // needs the consuming join seam, not a race against eventual cleanup.
    assert_eq!(
        task.shutdown_and_wait(),
        PreviewOwnedWorkerShutdown::Terminated
    );

    let diagnostics = broker.diagnostics();
    assert!(diagnostics.closed);
    assert_eq!(diagnostics.pending_requests, 0);
    assert_eq!(diagnostics.queued_work, 0);
    assert_eq!(diagnostics.in_flight_work, 0);
}

#[test]
fn ordinary_drop_closes_admission_without_waiting_for_active_execution() {
    struct ReleaseGateOnDrop(VisualExecutionTestGate);
    impl Drop for ReleaseGateOnDrop {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    let task = VisualExecutionTask::new(
        ManualClock::new(timestamp(1)),
        VisualExecutionTaskConfig::new(4, 4, 1),
    )
    .expect("visual task");
    let broker = task.broker.clone();
    let gate = VisualExecutionTestGate::new();
    // Also release the worker if submission, setup, or a pre-release
    // assertion fails. Dropping arbitrary gate clones must not release it.
    let _release_gate = ReleaseGateOnDrop(gate.clone());
    assert_queued(task.submit(admission(
        [10; 32],
        11,
        None,
        payload(&[51]).with_gate(gate.clone()),
    )));
    gate.wait_until_entered();
    assert_queued(task.submit(admission([11; 32], 11, None, payload(&[52]))));
    let (closed_tx, closed_rx) = mpsc::channel();
    let closing_broker = broker.clone();
    let dropper = thread::spawn(move || {
        drop(task);
        closed_tx.send(closing_broker.diagnostics()).expect("observe Drop return");
    });
    let closed = closed_rx.recv_timeout(Duration::from_secs(2));
    // Release the real worker even if Drop incorrectly waited for it.
    gate.release();
    dropper.join().expect("Drop caller returned");
    let diagnostics = closed.expect("ordinary Drop must not wait for active execution");
    assert!(diagnostics.closed);
    assert_eq!(diagnostics.pending_requests, 0);
    assert_eq!(diagnostics.queued_work, 0);
    assert_eq!(
        diagnostics.in_flight_work, 1,
        "physical execution is still gated"
    );
    wait_until(
        || broker.diagnostics().in_flight_work == 0,
        "detached execution did not release its lease after the gate opened",
    );
}
