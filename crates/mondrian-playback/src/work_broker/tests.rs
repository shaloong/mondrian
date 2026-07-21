use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone)]
struct ManualRuntimeClock {
    now_nanos: Arc<AtomicU64>,
}

impl ManualRuntimeClock {
    fn at(duration: Duration) -> Self {
        Self {
            now_nanos: Arc::new(AtomicU64::new(duration_nanos(duration))),
        }
    }

    fn set(&self, duration: Duration) {
        self.now_nanos.store(duration_nanos(duration), Ordering::Release);
    }
}

impl MonotonicRuntimeClock for ManualRuntimeClock {
    fn now(&self) -> MonotonicTimestamp {
        MonotonicTimestamp::from_duration(Duration::from_nanos(
            self.now_nanos.load(Ordering::Acquire),
        ))
    }
}

fn duration_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn request(key: u64, generation: u64, class: FrameWorkClass) -> FrameWorkRequest<u64, u64, u64> {
    FrameWorkRequest {
        key,
        generation,
        priority: FrameWorkPriority::Current,
        work_class: class,
        demand_identity: None,
        deadline: None,
        payload: key,
    }
}

#[test]
fn submit_dequeue_and_completion_share_one_lifecycle() {
    let broker = FrameWorkBroker::new(4, 4);
    let generation = broker.begin_generation();
    assert!(matches!(
        broker.submit(request(1, generation, FrameWorkClass::Playback)),
        FrameWorkSubmission::Queued { .. }
    ));
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    assert_eq!(broker.execution_cancellation(execution.id), None);
    let resolution = broker.resolve_execution(execution.id, true);
    assert_eq!(resolution.completion, FrameRequestCompletion::Current);
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.pending_requests, 0);
    assert_eq!(diagnostics.queued_work, 0);
    assert_eq!(diagnostics.in_flight_work, 0);
}

#[test]
fn bounded_receive_distinguishes_idle_from_closed() {
    let broker = FrameWorkBroker::<u64, u64, u64>::new(2, 2);

    assert!(matches!(
        broker.receive_timeout(FrameWorkerLane::Any, Duration::ZERO),
        FrameWorkReceiveWait::TimedOut
    ));
    broker.close();
    assert!(matches!(
        broker.receive_timeout(FrameWorkerLane::Any, Duration::from_secs(1)),
        FrameWorkReceiveWait::Closed
    ));
}

#[test]
fn lifecycle_interrupt_cannot_be_lost_before_bounded_receive() {
    let broker = FrameWorkBroker::<u64, u64, u64>::new(2, 2);
    let observed_revision = broker.worker_lifecycle_revision();
    let published_revision = broker.interrupt_worker_waits();

    assert_eq!(published_revision, observed_revision + 1);
    assert!(matches!(
        broker.receive_timeout_after_lifecycle_revision(
            FrameWorkerLane::Any,
            Duration::from_secs(1),
            observed_revision,
        ),
        FrameWorkReceiveWait::Interrupted { revision } if revision == published_revision
    ));
}

#[test]
fn lifecycle_interrupt_precedes_queued_work_admission() {
    let broker = FrameWorkBroker::<u64, u64, u64>::new(2, 2);
    let generation = broker.begin_generation();
    let observed_revision = broker.worker_lifecycle_revision();
    assert!(matches!(
        broker.submit(request(1, generation, FrameWorkClass::Playback)),
        FrameWorkSubmission::Queued { .. }
    ));
    let published_revision = broker.interrupt_worker_waits();

    assert!(matches!(
        broker.receive_timeout_after_lifecycle_revision(
            FrameWorkerLane::Playback,
            Duration::from_secs(1),
            observed_revision,
        ),
        FrameWorkReceiveWait::Interrupted { revision } if revision == published_revision
    ));
    assert!(matches!(
        broker.receive_timeout_after_lifecycle_revision(
            FrameWorkerLane::Playback,
            Duration::from_secs(1),
            published_revision,
        ),
        FrameWorkReceiveWait::Work(FrameWorkReceive::Ready(_))
    ));
}

#[test]
fn current_still_work_preserves_decoder_lane_affinity() {
    let queue = VecDeque::from([QueuedWork {
        request: request(1, 1, FrameWorkClass::Still),
        deadline_at: None,
    }]);

    assert_eq!(
        next_work_index(&queue, FrameWorkerLane::Playback, MonotonicTimestamp::ZERO),
        None
    );
    assert_eq!(
        next_work_index(
            &queue,
            FrameWorkerLane::Interactive,
            MonotonicTimestamp::ZERO
        ),
        None
    );
    assert_eq!(
        next_work_index(&queue, FrameWorkerLane::Still, MonotonicTimestamp::ZERO),
        Some(0)
    );
    assert_eq!(
        next_work_index(
            &queue,
            FrameWorkerLane::NonPlayback,
            MonotonicTimestamp::ZERO
        ),
        Some(0)
    );
    assert_eq!(
        next_work_index(&queue, FrameWorkerLane::Any, MonotonicTimestamp::ZERO),
        Some(0)
    );
}

#[test]
fn same_class_rerequest_reuses_in_flight_and_rebinds_completion() {
    let broker = FrameWorkBroker::new(2, 2);
    let first = broker.begin_generation();
    broker.submit(request(1, first, FrameWorkClass::Playback));
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    let latest = broker.begin_generation();
    let mut latest_request = request(1, latest, FrameWorkClass::Playback);
    latest_request.deadline = Some(FrameWorkDeadline::from_remaining(
        200,
        Duration::from_secs(1),
    ));
    assert_eq!(
        broker.submit(latest_request),
        FrameWorkSubmission::ReusedInFlight
    );
    let resolution = broker.resolve_execution(execution.id, true);
    assert_eq!(resolution.completion, FrameRequestCompletion::Current);
    assert_eq!(resolution.binding.expect("binding").deadline, Some(200));
}

#[test]
fn execution_cancellation_tracks_obsolescence_and_rebinding() {
    let broker = FrameWorkBroker::new(2, 2);
    let first = broker.begin_generation();
    broker.submit(request(1, first, FrameWorkClass::Playback));
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    assert_eq!(broker.execution_cancellation(execution.id), None);

    let latest = broker.begin_generation();
    assert!(matches!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::Superseded { age: Some(_) })
    ));
    assert_eq!(
        broker.submit(request(1, latest, FrameWorkClass::Playback)),
        FrameWorkSubmission::ReusedInFlight
    );
    assert_eq!(broker.execution_cancellation(execution.id), None);

    broker.cancel_all();
    assert!(matches!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::Superseded { age: Some(_) })
    ));
}

#[test]
fn injected_clock_controls_cancellation_age_and_reports_regression() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
    let generation = broker.begin_generation();
    broker.submit(request(1, generation, FrameWorkClass::Playback));
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    clock.set(Duration::from_millis(20));
    broker.begin_generation();
    clock.set(Duration::from_millis(27));
    assert_eq!(
        broker.execution_cancellation_evidence(execution.id),
        Some(crate::FrameExecutionCancellationEvidence {
            cancellation: FrameExecutionCancellation::Superseded {
                age: Some(Duration::from_millis(7)),
            },
            execution_age: Duration::from_millis(17),
        })
    );
    assert_eq!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::Superseded { age: Some(Duration::from_millis(7)) })
    );

    clock.set(Duration::from_millis(25));
    assert_eq!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::Superseded { age: Some(Duration::from_millis(7)) })
    );
    assert_eq!(broker.diagnostics().clock_regressions, 1);
}

#[test]
fn injected_clock_makes_realtime_expiration_exact() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(100));
    let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
    let generation = broker.begin_generation();
    broker.submit(request(1, generation, FrameWorkClass::Interactive));

    clock.set(Duration::from_millis(149));
    assert!(broker.expire_realtime_current_older_than(Duration::from_millis(50)).is_empty());
    clock.set(Duration::from_millis(150));
    let expired = broker.expire_realtime_current_older_than(Duration::from_millis(50));

    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].key, 1);
}

#[test]
fn preemption_request_cannot_predate_execution_lease() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(3, 3, clock.clone());
    let generation = broker.begin_generation();
    broker.submit(request(2, generation, FrameWorkClass::Interactive));
    let mut prefetch = request(1, generation, FrameWorkClass::Playback);
    prefetch.priority = FrameWorkPriority::Prefetch;
    broker.submit(prefetch);

    clock.set(Duration::from_millis(20));
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    clock.set(Duration::from_millis(25));

    assert_eq!(
        broker.execution_cancellation_evidence(execution.id),
        Some(crate::FrameExecutionCancellationEvidence {
            cancellation: FrameExecutionCancellation::PrefetchPreemptedByCurrent {
                request_age: Duration::from_millis(5),
            },
            execution_age: Duration::from_millis(5),
        })
    );
}

#[test]
fn closing_broker_cancels_every_in_flight_execution() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
    let generation = broker.begin_generation();
    broker.submit(request(1, generation, FrameWorkClass::Playback));
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    clock.set(Duration::from_millis(20));
    broker.close();
    clock.set(Duration::from_millis(27));

    assert_eq!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::BrokerClosed { age: Duration::from_millis(7) })
    );

    clock.set(Duration::from_millis(30));
    broker.close();
    assert_eq!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::BrokerClosed { age: Duration::from_millis(10) })
    );
}

#[test]
fn still_preemption_age_starts_at_competing_request_admission() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
    let generation = broker.begin_generation();
    broker.submit(request(1, generation, FrameWorkClass::Still));
    let execution = match broker.receive(FrameWorkerLane::Still) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    assert_eq!(broker.execution_cancellation(execution.id), None);

    clock.set(Duration::from_millis(30));
    broker.submit(request(2, generation, FrameWorkClass::Playback));
    clock.set(Duration::from_millis(35));
    assert_eq!(
        broker.execution_cancellation(execution.id),
        Some(
            FrameExecutionCancellation::StillPreemptedByRealtimeCurrent {
                request_age: Duration::from_millis(5)
            }
        )
    );
}

#[test]
fn speculative_rerequest_cannot_change_current_interactive_work_class() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    broker.submit(request(1, generation, FrameWorkClass::Interactive));

    let mut speculative = request(1, generation, FrameWorkClass::Playback);
    speculative.priority = FrameWorkPriority::Prefetch;
    assert!(matches!(
        broker.submit(speculative),
        FrameWorkSubmission::UpdatedQueued {
            priority_promoted: false,
            work_class_changed: false,
            generation_changed: false,
        }
    ));

    let execution = match broker.receive(FrameWorkerLane::Interactive) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    assert_eq!(execution.priority, FrameWorkPriority::Current);
    assert_eq!(execution.work_class, FrameWorkClass::Interactive);
}

#[test]
fn class_change_queues_replacement_and_invalidates_old_execution() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    broker.submit(request(1, generation, FrameWorkClass::Interactive));
    let old = match broker.receive(FrameWorkerLane::Interactive) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    assert!(matches!(
        broker.submit(request(1, generation, FrameWorkClass::Playback)),
        FrameWorkSubmission::Queued { .. }
    ));
    assert!(matches!(
        broker.execution_cancellation(old.id),
        Some(FrameExecutionCancellation::Superseded { .. })
    ));
    assert_eq!(
        broker.resolve_execution(old.id, true).completion,
        FrameRequestCompletion::CacheOnly
    );
    assert_eq!(broker.diagnostics().queued_work, 1);
}

#[test]
fn canceled_old_execution_leaves_new_binding_pending() {
    let broker = FrameWorkBroker::new(2, 2);
    let first = broker.begin_generation();
    broker.submit(request(1, first, FrameWorkClass::Playback));
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    let latest = broker.begin_generation();
    broker.submit(request(1, latest, FrameWorkClass::Playback));
    assert_eq!(
        broker.resolve_execution(execution.id, false).completion,
        FrameRequestCompletion::Stale
    );
    assert_eq!(broker.diagnostics().pending_requests, 1);
}

#[test]
fn playback_deadline_expires_at_dequeue_without_losing_lease_identity() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let generation = broker.begin_generation();
    let mut request = request(1, generation, FrameWorkClass::Playback);
    request.deadline = Some(FrameWorkDeadline::from_remaining(
        100,
        Duration::from_millis(20),
    ));
    broker.submit(request);
    clock.set(Duration::from_millis(29));
    assert_eq!(broker.diagnostics().queued_expired_work, 0);
    clock.set(Duration::from_millis(30));
    assert_eq!(broker.diagnostics().queued_expired_work, 1);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Expired(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    assert_eq!(execution.key, 1);
    assert_eq!(execution.deadline, Some(100));
    assert_eq!(broker.diagnostics().dropped_expired_work, 1);
    assert_eq!(broker.diagnostics().dropped_expired_playback_current, 1);
    assert_eq!(
        broker.resolve_execution(execution.id, false).completion,
        FrameRequestCompletion::Current
    );
}

#[test]
fn expired_prefetch_uses_generic_deadline_evidence() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let generation = broker.begin_generation();
    let mut request = request(1, generation, FrameWorkClass::Playback);
    request.priority = FrameWorkPriority::Prefetch;
    request.deadline = Some(FrameWorkDeadline::from_remaining(
        100,
        Duration::from_millis(20),
    ));
    broker.submit(request);
    clock.set(Duration::from_millis(30));

    assert!(matches!(
        broker.receive(FrameWorkerLane::Playback),
        Some(FrameWorkReceive::Expired(_))
    ));
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.dropped_expired_work, 1);
    assert_eq!(diagnostics.dropped_expired_playback_current, 0);
}

#[test]
fn worker_completion_stamp_prevents_poll_delay_false_miss() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let generation = broker.begin_generation();
    let mut request = request(1, generation, FrameWorkClass::Playback);
    request.deadline = Some(FrameWorkDeadline::from_remaining(
        100,
        Duration::from_millis(20),
    ));
    broker.submit(request);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    clock.set(Duration::from_millis(29));
    assert!(broker.mark_execution_completed(execution.id));
    clock.set(Duration::from_millis(40));
    let resolution = broker.resolve_execution(execution.id, true);

    assert_eq!(resolution.deadline, FrameWorkDeadlineStatus::OnTime);
}

#[test]
fn in_flight_deadline_rebind_uses_latest_binding_and_exact_age() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let first = broker.begin_generation();
    let mut first_request = request(1, first, FrameWorkClass::Playback);
    first_request.deadline = Some(FrameWorkDeadline::from_remaining(
        100,
        Duration::from_millis(40),
    ));
    broker.submit(first_request);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    clock.set(Duration::from_millis(20));
    let latest = broker.begin_generation();
    let mut rebound = request(1, latest, FrameWorkClass::Playback);
    rebound.deadline = Some(FrameWorkDeadline::from_remaining(
        200,
        Duration::from_millis(100),
    ));
    assert_eq!(broker.submit(rebound), FrameWorkSubmission::ReusedInFlight);

    clock.set(Duration::from_millis(50));
    assert_eq!(broker.execution_cancellation(execution.id), None);
    clock.set(Duration::from_millis(125));
    assert_eq!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::DeadlineExpired { age: Duration::from_millis(5) })
    );
    let resolution = broker.resolve_execution(execution.id, true);
    assert_eq!(
        resolution.binding.expect("latest binding").deadline,
        Some(200)
    );
}

#[test]
fn earliest_deadline_wins_over_later_preemption_request() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
    let generation = broker.begin_generation();
    let mut prefetch = request(1, generation, FrameWorkClass::Playback);
    prefetch.priority = FrameWorkPriority::Prefetch;
    prefetch.deadline = Some(FrameWorkDeadline::from_remaining(
        100,
        Duration::from_millis(20),
    ));
    broker.submit(prefetch);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    clock.set(Duration::from_millis(35));
    broker.submit(request(2, generation, FrameWorkClass::Playback));
    clock.set(Duration::from_millis(40));

    assert_eq!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::DeadlineExpired { age: Duration::from_millis(10) })
    );
}

#[test]
fn earliest_preemption_survives_later_generation_invalidation() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
    let generation = broker.begin_generation();
    let mut prefetch = request(1, generation, FrameWorkClass::Playback);
    prefetch.priority = FrameWorkPriority::Prefetch;
    broker.submit(prefetch);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    clock.set(Duration::from_millis(20));
    broker.submit(request(2, generation, FrameWorkClass::Playback));
    clock.set(Duration::from_millis(30));
    broker.begin_generation();
    clock.set(Duration::from_millis(40));

    assert_eq!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::PrefetchPreemptedByCurrent {
            request_age: Duration::from_millis(20),
        })
    );
}

#[test]
fn cancellation_and_pruning_cover_pending_and_queue_atomically() {
    let broker = FrameWorkBroker::new(4, 4);
    let first = broker.begin_generation();
    broker.submit(request(1, first, FrameWorkClass::Playback));
    let latest = broker.begin_generation();
    assert_eq!(broker.prune_obsolete(), 1);
    broker.submit(request(2, latest, FrameWorkClass::Playback));
    assert_eq!(broker.cancel_key(&2), 1);
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.pending_requests, 0);
    assert_eq!(diagnostics.queued_work, 0);
}

#[test]
fn diagnostics_derive_worker_lane_residency_from_execution_leases() {
    let broker = FrameWorkBroker::new(5, 5);
    let generation = broker.begin_generation();
    let cases = [
        (1, FrameWorkClass::Playback, FrameWorkerLane::Playback),
        (2, FrameWorkClass::Interactive, FrameWorkerLane::Interactive),
        (3, FrameWorkClass::Still, FrameWorkerLane::Still),
        (4, FrameWorkClass::Interactive, FrameWorkerLane::NonPlayback),
        (5, FrameWorkClass::Playback, FrameWorkerLane::Any),
    ];
    let mut execution_ids = Vec::with_capacity(cases.len());

    for (key, work_class, worker_lane) in cases {
        assert!(matches!(
            broker.submit(request(key, generation, work_class)),
            FrameWorkSubmission::Queued { .. }
        ));
        let execution = match broker.receive(worker_lane) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive for {worker_lane:?}: {other:?}"),
        };
        execution_ids.push(execution.id);
    }

    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.in_flight_work, 5);
    assert_eq!(diagnostics.in_flight_current, 5);
    assert_eq!(diagnostics.in_flight_playback, 2);
    assert_eq!(diagnostics.in_flight_interactive, 2);
    assert_eq!(diagnostics.in_flight_still, 1);
    assert_eq!(diagnostics.in_flight_any_lane, 1);
    assert_eq!(diagnostics.in_flight_playback_lane, 1);
    assert_eq!(diagnostics.in_flight_interactive_lane, 1);
    assert_eq!(diagnostics.in_flight_still_lane, 1);
    assert_eq!(diagnostics.in_flight_non_playback_lane, 1);
    assert_eq!(diagnostics.in_flight_cross_lane_current, 0);

    for execution_id in execution_ids {
        assert!(broker.abandon_execution(execution_id));
    }
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.in_flight_work, 0);
    assert_eq!(diagnostics.in_flight_any_lane, 0);
    assert_eq!(diagnostics.in_flight_playback_lane, 0);
    assert_eq!(diagnostics.in_flight_interactive_lane, 0);
    assert_eq!(diagnostics.in_flight_still_lane, 0);
    assert_eq!(diagnostics.in_flight_non_playback_lane, 0);
    assert_eq!(diagnostics.in_flight_cross_lane_current, 0);
}

#[test]
fn failed_dual_capacity_admission_preserves_every_existing_binding() {
    let broker = FrameWorkBroker::new(2, 1);
    let generation = broker.begin_generation();
    let mut in_flight_prefetch = request(1, generation, FrameWorkClass::Playback);
    in_flight_prefetch.priority = FrameWorkPriority::Prefetch;
    broker.submit(in_flight_prefetch);
    let prefetch_execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    broker.submit(request(2, generation, FrameWorkClass::Playback));

    assert_eq!(
        broker.submit(request(3, generation, FrameWorkClass::Playback)),
        FrameWorkSubmission::DroppedBackpressure
    );
    assert!(broker.has_pending_key(&1));
    assert!(broker.has_pending_key(&2));
    assert!(matches!(
        broker.execution_cancellation(prefetch_execution.id),
        Some(FrameExecutionCancellation::PrefetchPreemptedByCurrent { .. })
    ));
    let queued = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    assert_eq!(queued.key, 2);
}

#[test]
fn dual_capacity_admission_prefers_one_queued_eviction_that_opens_both_windows() {
    let broker = FrameWorkBroker::new(2, 1);
    let generation = broker.begin_generation();
    broker.submit(request(1, generation, FrameWorkClass::Playback));
    let _current = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    let mut queued_prefetch = request(2, generation, FrameWorkClass::Playback);
    queued_prefetch.priority = FrameWorkPriority::Prefetch;
    broker.submit(queued_prefetch);

    assert_eq!(
        broker.submit(request(3, generation, FrameWorkClass::Playback)),
        FrameWorkSubmission::Queued { evicted_prefetch: Some(2), evicted_still: None }
    );
    assert!(!broker.has_pending_key(&2));
    assert!(broker.has_pending_key(&3));
}

#[test]
fn failed_class_change_keeps_original_in_flight_binding_current() {
    let broker = FrameWorkBroker::new(2, 1);
    let generation = broker.begin_generation();
    broker.submit(request(1, generation, FrameWorkClass::Playback));
    let original = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    broker.submit(request(2, generation, FrameWorkClass::Playback));

    assert_eq!(
        broker.submit(request(1, generation, FrameWorkClass::Interactive)),
        FrameWorkSubmission::DroppedBackpressure
    );
    assert_eq!(broker.execution_cancellation(original.id), None);
    assert_eq!(broker.diagnostics().pending_requests, 2);
}
