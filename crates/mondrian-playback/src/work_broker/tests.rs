use super::*;
use crate::{FrameExecutionCancellationEvidence, MediaWorkReservationIntent};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Barrier;

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
        resource_scope: FrameWorkResourceScope::Shared,
        demand_identity: None,
        deadline: None,
        in_flight_deadline_policy: FrameInFlightDeadlinePolicy::Cancel,
        execution_cancellation_budget: None,
        payload: key,
    }
}

fn binding_request(
    key: u64,
    generation: u64,
    class: FrameWorkClass,
) -> FrameWorkBindingRequest<u64, u64> {
    FrameWorkBindingRequest {
        key,
        generation,
        priority: FrameWorkPriority::Current,
        work_class: class,
        resource_scope: FrameWorkResourceScope::Shared,
        demand_identity: None,
        deadline: None,
        in_flight_deadline_policy: FrameInFlightDeadlinePolicy::Cancel,
        execution_cancellation_budget: None,
    }
}

#[derive(Debug)]
struct DropTrackedPayload {
    value: u64,
    drops: Arc<AtomicUsize>,
}

impl Drop for DropTrackedPayload {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::AcqRel);
    }
}

fn demand_identity(sequence: u64) -> FrameDemandIdentity {
    FrameDemandIdentity {
        epoch: crate::PlaybackEpoch(1),
        quality_revision: 0,
        sequence: crate::FrameDemandSequence(sequence),
        target_frame: 0,
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
fn exact_binding_owner_predicate_tracks_queue_execution_and_terminal_settlement() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    assert!(matches!(
        broker.submit(request(1, generation, FrameWorkClass::Playback)),
        FrameWorkSubmission::Queued { .. }
    ));
    assert!(broker.binding_has_execution_owner(
        &1,
        generation,
        FrameWorkClass::Playback,
        FrameWorkResourceScope::Shared,
        None,
    ));

    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    assert!(broker.binding_has_execution_owner(
        &1,
        generation,
        FrameWorkClass::Playback,
        FrameWorkResourceScope::Shared,
        None,
    ));

    assert_eq!(
        broker.resolve_execution(execution.id, true).completion,
        FrameRequestCompletion::Current
    );
    assert!(!broker.binding_has_execution_owner(
        &1,
        generation,
        FrameWorkClass::Playback,
        FrameWorkResourceScope::Shared,
        None,
    ));
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
fn exact_binding_rerequest_reuses_in_flight_without_a_fallback() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    broker.submit(request(1, generation, FrameWorkClass::Playback));
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    assert_eq!(
        broker.submit(request(1, generation, FrameWorkClass::Playback)),
        FrameWorkSubmission::ReusedInFlight
    );
    assert_eq!(broker.diagnostics().queued_work, 0);
    assert_eq!(
        broker.resolve_execution(execution.id, true).completion,
        FrameRequestCompletion::Current
    );
}

#[test]
fn bind_existing_updates_queued_metadata_without_replacing_or_dropping_payload() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let drops = Arc::new(AtomicUsize::new(0));
    let request = FrameWorkRequest {
        key: 1,
        generation,
        priority: FrameWorkPriority::Prefetch,
        work_class: FrameWorkClass::Playback,
        resource_scope: FrameWorkResourceScope::Shared,
        demand_identity: Some(demand_identity(1)),
        deadline: None,
        in_flight_deadline_policy: FrameInFlightDeadlinePolicy::Cancel,
        execution_cancellation_budget: None,
        payload: DropTrackedPayload { value: 41, drops: Arc::clone(&drops) },
    };
    assert!(matches!(
        broker.submit(request),
        FrameWorkSubmission::Queued { .. }
    ));
    let mut rebound = binding_request(1, generation, FrameWorkClass::Playback);
    rebound.demand_identity = Some(demand_identity(2));
    rebound.deadline = Some(FrameWorkDeadline::from_remaining(
        77,
        Duration::from_secs(1),
    ));

    assert_eq!(
        broker.bind_existing(rebound),
        FrameWorkBindingSubmission::UpdatedQueued {
            priority_promoted: true,
            work_class_changed: false,
            generation_changed: false,
        }
    );
    assert_eq!(drops.load(Ordering::Acquire), 0);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    assert_eq!(execution.payload.value, 41);
    assert_eq!(execution.demand_identity, Some(demand_identity(2)));
    assert_eq!(execution.deadline, Some(77));
    let execution_id = execution.id;
    drop(execution);
    assert_eq!(drops.load(Ordering::Acquire), 1);
    let resolution = broker.resolve_execution(execution_id, true);
    assert_eq!(resolution.completion, FrameRequestCompletion::Current);
    assert_eq!(
        resolution.binding.expect("rebound binding").resource_scope,
        FrameWorkResourceScope::Shared
    );
}

#[test]
fn promoted_current_queue_wait_starts_at_latest_binding() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let generation = broker.begin_generation();
    let mut prefetch = request(1, generation, FrameWorkClass::Playback);
    prefetch.priority = FrameWorkPriority::Prefetch;
    assert!(matches!(
        broker.submit(prefetch),
        FrameWorkSubmission::Queued { .. }
    ));

    clock.set(Duration::from_millis(110));
    assert!(matches!(
        broker.bind_existing(binding_request(1, generation, FrameWorkClass::Playback)),
        FrameWorkBindingSubmission::UpdatedQueued { priority_promoted: true, .. }
    ));
    clock.set(Duration::from_millis(117));

    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    assert_eq!(execution.queue_wait, Duration::from_millis(7));
}

#[test]
fn bind_existing_rebinds_compatible_in_flight_work() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut original = request(1, generation, FrameWorkClass::Playback);
    original.priority = FrameWorkPriority::Prefetch;
    original.demand_identity = Some(demand_identity(1));
    broker.submit(original);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    let mut rebound = binding_request(1, generation, FrameWorkClass::Playback);
    rebound.demand_identity = Some(demand_identity(1));
    rebound.deadline = Some(FrameWorkDeadline::from_remaining(
        88,
        Duration::from_secs(1),
    ));

    assert_eq!(
        broker.bind_existing(rebound),
        FrameWorkBindingSubmission::ReusedInFlight
    );
    let resolution = broker.resolve_execution(execution.id, true);
    let binding = resolution.binding.expect("current rebound binding");
    assert_eq!(resolution.completion, FrameRequestCompletion::Current);
    assert_eq!(binding.priority, FrameWorkPriority::Current);
    assert_eq!(binding.deadline, Some(88));
    assert_eq!(binding.resource_scope, FrameWorkResourceScope::Shared);
}

#[test]
fn bind_existing_different_resource_scope_needs_payload_without_mutation() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    broker.submit(request(1, generation, FrameWorkClass::Playback));
    let mut different_scope = binding_request(1, generation, FrameWorkClass::Playback);
    different_scope.resource_scope =
        FrameWorkResourceScope::Media(MediaWorkReservationIntent::Prefetch);

    assert_eq!(
        broker.bind_existing(different_scope),
        FrameWorkBindingSubmission::NeedsPayload
    );
    assert_eq!(broker.diagnostics().queued_work, 1);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    assert_eq!(execution.resource_scope, FrameWorkResourceScope::Shared);

    let mut different_scope = binding_request(1, generation, FrameWorkClass::Playback);
    different_scope.resource_scope =
        FrameWorkResourceScope::Media(MediaWorkReservationIntent::Prefetch);
    assert_eq!(
        broker.bind_existing(different_scope),
        FrameWorkBindingSubmission::NeedsPayload
    );
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.pending_requests, 1);
    assert_eq!(diagnostics.queued_work, 0);
    assert_eq!(diagnostics.in_flight_work, 1);
    assert_eq!(diagnostics.submitted_updated_queued, 0);
    assert_eq!(diagnostics.submitted_reused_in_flight, 0);
    assert_eq!(
        broker.resolve_execution(execution.id, true).completion,
        FrameRequestCompletion::Current
    );
}

#[test]
fn cross_binding_rerequest_queues_fallback_and_reusable_completion_consumes_it() {
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
        FrameWorkSubmission::Queued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(broker.diagnostics().queued_work, 1);
    let resolution = broker.resolve_execution(execution.id, true);
    assert_eq!(resolution.completion, FrameRequestCompletion::Current);
    assert_eq!(resolution.binding.expect("binding").deadline, Some(200));
    assert_eq!(broker.diagnostics().pending_requests, 0);
    assert_eq!(broker.diagnostics().queued_work, 0);
}

#[test]
fn changed_demand_queues_fallback_and_reusable_completion_consumes_it() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut original = request(1, generation, FrameWorkClass::Playback);
    original.demand_identity = Some(demand_identity(1));
    broker.submit(original);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    let mut rebound = request(1, generation, FrameWorkClass::Playback);
    rebound.demand_identity = Some(demand_identity(2));

    assert!(matches!(
        broker.submit(rebound),
        FrameWorkSubmission::Queued { .. }
    ));
    assert_eq!(broker.diagnostics().queued_work, 1);
    let resolution = broker.resolve_execution(execution.id, true);
    assert_eq!(resolution.completion, FrameRequestCompletion::Current);
    assert_eq!(
        resolution.binding.expect("rebound binding").demand_identity,
        Some(demand_identity(2))
    );
    assert_eq!(broker.diagnostics().pending_requests, 0);
    assert_eq!(broker.diagnostics().queued_work, 0);
}

#[test]
fn changed_demand_non_reusable_completion_preserves_fallback() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut original = request(1, generation, FrameWorkClass::Playback);
    original.demand_identity = Some(demand_identity(1));
    broker.submit(original);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    let mut rebound = request(1, generation, FrameWorkClass::Playback);
    rebound.demand_identity = Some(demand_identity(2));
    assert!(matches!(
        broker.submit(rebound),
        FrameWorkSubmission::Queued { .. }
    ));

    assert_eq!(
        broker.resolve_execution(execution.id, false).completion,
        FrameRequestCompletion::Stale
    );
    assert_eq!(broker.diagnostics().pending_requests, 1);
    assert_eq!(broker.diagnostics().queued_work, 1);
    let replacement = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected fallback receive: {other:?}"),
    };
    assert_eq!(
        broker.resolve_execution(replacement.id, true).completion,
        FrameRequestCompletion::Current
    );
}

#[test]
fn active_playback_demand_coalesces_only_older_unstarted_current_work() {
    let broker = FrameWorkBroker::new(8, 8);
    let generation = broker.begin_generation();
    for key in [1, 2] {
        let mut old = request(key, generation, FrameWorkClass::Playback);
        old.demand_identity = Some(demand_identity(1));
        assert!(matches!(
            broker.submit(old),
            FrameWorkSubmission::Queued { .. }
        ));
    }
    let mut prefetch = request(3, generation, FrameWorkClass::Playback);
    prefetch.priority = FrameWorkPriority::Prefetch;
    assert!(matches!(
        broker.submit(prefetch),
        FrameWorkSubmission::Queued { .. }
    ));
    let mut interactive = request(4, generation, FrameWorkClass::Interactive);
    interactive.demand_identity = Some(demand_identity(1));
    assert!(matches!(
        broker.submit(interactive),
        FrameWorkSubmission::Queued { .. }
    ));

    assert_eq!(
        broker.synchronize_playback_current_demand(demand_identity(2)),
        2
    );
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.active_playback_demand, Some(demand_identity(2)));
    assert_eq!(diagnostics.superseded_queued_playback_current, 2);
    assert_eq!(diagnostics.pending_requests, 2);
    assert_eq!(diagnostics.queued_work, 2);

    for key in [5, 6] {
        let mut current = request(key, generation, FrameWorkClass::Playback);
        current.demand_identity = Some(demand_identity(2));
        assert!(matches!(
            broker.submit(current),
            FrameWorkSubmission::Queued { .. }
        ));
    }
    assert_eq!(
        broker.synchronize_playback_current_demand(demand_identity(2)),
        0
    );
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.pending_requests, 4);
    assert_eq!(diagnostics.queued_work, 4);
    assert_eq!(diagnostics.queued_current, 3);
    assert_eq!(diagnostics.queued_prefetch, 1);
}

#[test]
fn superseded_in_flight_playback_current_finishes_for_locality_without_publication() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut old = request(1, generation, FrameWorkClass::Playback);
    old.demand_identity = Some(demand_identity(1));
    old.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(old);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected current receive: {other:?}"),
    };

    assert_eq!(
        broker.synchronize_playback_current_demand(demand_identity(2)),
        0
    );
    assert_eq!(broker.execution_cancellation(execution.id), None);
    let resolution = broker.resolve_execution(execution.id, true);
    assert_eq!(resolution.completion, FrameRequestCompletion::CacheOnly);
    assert!(resolution.binding.is_none());
    assert_eq!(broker.diagnostics().pending_requests, 0);
}

#[test]
fn superseded_playback_demand_with_old_queue_preserves_running_decoder_locality() {
    let broker = FrameWorkBroker::new(3, 3);
    let generation = broker.begin_generation();
    let mut running = request(1, generation, FrameWorkClass::Playback);
    running.demand_identity = Some(demand_identity(1));
    running.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(running);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected current receive: {other:?}"),
    };

    let mut old_queued = request(2, generation, FrameWorkClass::Playback);
    old_queued.demand_identity = Some(demand_identity(1));
    old_queued.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(old_queued);

    assert_eq!(
        broker.synchronize_playback_current_demand(demand_identity(2)),
        1
    );
    assert_eq!(broker.execution_cancellation(execution.id), None);
    let resolution = broker.resolve_execution(execution.id, true);
    assert_eq!(resolution.completion, FrameRequestCompletion::CacheOnly);
    assert!(resolution.binding.is_none());
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.queued_work, 0);
    assert_eq!(diagnostics.in_flight_work, 0);
}

#[test]
fn spatial_generation_rotation_retains_playback_decode_without_publication() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut current = request(1, generation, FrameWorkClass::Playback);
    current.demand_identity = Some(demand_identity(1));
    current.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(current);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected current receive: {other:?}"),
    };

    let next = broker.begin_generation_preserving_playback_locality();
    assert!(next > generation);
    assert_eq!(broker.execution_cancellation(execution.id), None);
    let resolution = broker.resolve_execution(execution.id, true);
    assert_eq!(resolution.completion, FrameRequestCompletion::CacheOnly);
    assert!(resolution.binding.is_none());
}

#[test]
fn viewer_generation_rotation_retains_playback_prefetch_locality() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut prefetch = request(1, generation, FrameWorkClass::Playback);
    prefetch.priority = FrameWorkPriority::Prefetch;
    prefetch.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(prefetch);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected prefetch receive: {other:?}"),
    };

    broker.begin_generation_preserving_playback_locality();
    assert_eq!(broker.execution_cancellation(execution.id), None);
    let resolution = broker.resolve_execution(execution.id, true);
    assert_eq!(resolution.completion, FrameRequestCompletion::CacheOnly);
    assert!(resolution.binding.is_none());
}

#[test]
fn viewer_generation_rotation_rebinds_queued_playback_prefetch() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut prefetch = request(1, generation, FrameWorkClass::Playback);
    prefetch.priority = FrameWorkPriority::Prefetch;
    prefetch.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(prefetch);

    let next = broker.begin_generation_preserving_playback_locality();
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.latest_generation, next);
    assert_eq!(diagnostics.pending_requests, 1);
    assert_eq!(diagnostics.queued_work, 1);
    assert_eq!(diagnostics.pruned_queued, 0);

    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected rebound prefetch receive: {other:?}"),
    };
    assert_eq!(execution.generation, next);
    assert!(broker.mark_execution_completed(execution.id));
    let resolution = broker.resolve_execution(execution.id, true);
    assert_eq!(resolution.completion, FrameRequestCompletion::Current);
    assert_eq!(
        resolution.binding.map(|binding| binding.generation),
        Some(next)
    );
}

#[test]
fn representation_rotation_retains_running_playback_but_prunes_old_queue() {
    let broker = FrameWorkBroker::new(3, 3);
    let generation = broker.begin_generation();
    let mut running = request(1, generation, FrameWorkClass::Playback);
    running.demand_identity = Some(demand_identity(1));
    running.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(running);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected current receive: {other:?}"),
    };

    let mut queued = request(2, generation, FrameWorkClass::Playback);
    queued.priority = FrameWorkPriority::Prefetch;
    queued.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    assert!(matches!(
        broker.submit(queued),
        FrameWorkSubmission::Queued { .. }
    ));

    let next = broker.begin_generation_preserving_in_flight_playback_locality();
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.latest_generation, next);
    assert_eq!(diagnostics.queued_work, 0);
    assert_eq!(diagnostics.pending_requests, 0);
    assert_eq!(diagnostics.pruned_queued, 1);
    assert_eq!(diagnostics.in_flight_generation_invalidations, 0);
    assert_eq!(diagnostics.in_flight_binding_invalidations, 0);
    assert_eq!(broker.execution_cancellation(execution.id), None);

    let resolution = broker.resolve_execution(execution.id, true);
    assert_eq!(resolution.completion, FrameRequestCompletion::CacheOnly);
    assert!(resolution.binding.is_none());
    assert_eq!(broker.diagnostics().pending_requests, 0);
}

#[test]
fn semantic_generation_rotation_cancels_previously_retained_playback_decode() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut current = request(1, generation, FrameWorkClass::Playback);
    current.demand_identity = Some(demand_identity(1));
    current.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(current);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected current receive: {other:?}"),
    };

    broker.begin_generation_preserving_playback_locality();
    assert_eq!(broker.execution_cancellation(execution.id), None);
    broker.begin_generation();
    assert_eq!(broker.diagnostics().in_flight_generation_invalidations, 1);
    assert!(matches!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::Superseded { .. })
    ));
}

#[test]
fn same_key_execution_may_rebind_to_the_active_playback_demand() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut old = request(1, generation, FrameWorkClass::Playback);
    old.demand_identity = Some(demand_identity(1));
    old.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(old);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected old-demand receive: {other:?}"),
    };
    broker.synchronize_playback_current_demand(demand_identity(2));
    let mut current = request(1, generation, FrameWorkClass::Playback);
    current.demand_identity = Some(demand_identity(2));
    current.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    assert!(matches!(
        broker.submit(current),
        FrameWorkSubmission::Queued { .. }
    ));

    let resolution = broker.resolve_execution(execution.id, true);
    assert_eq!(resolution.completion, FrameRequestCompletion::Current);
    assert_eq!(
        resolution.binding.expect("active binding").demand_identity,
        Some(demand_identity(2))
    );
    assert_eq!(broker.diagnostics().queued_work, 0);
}

#[test]
fn reusable_winner_detaches_running_playback_fallback_without_canceling_decoder_locality() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut original = request(1, generation, FrameWorkClass::Playback);
    original.demand_identity = Some(demand_identity(1));
    original.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(original);
    let winner = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected original receive: {other:?}"),
    };

    let mut fallback = request(1, generation, FrameWorkClass::Playback);
    fallback.demand_identity = Some(demand_identity(2));
    fallback.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    assert!(matches!(
        broker.submit(fallback),
        FrameWorkSubmission::Queued { .. }
    ));
    let fallback = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected fallback receive: {other:?}"),
    };

    assert!(broker.mark_execution_completed(winner.id));
    let winner_resolution = broker.resolve_execution(winner.id, true);
    assert_eq!(
        winner_resolution.completion,
        FrameRequestCompletion::Current
    );
    assert_eq!(
        winner_resolution.binding.expect("latest binding").demand_identity,
        Some(demand_identity(2))
    );
    assert_eq!(broker.execution_cancellation(fallback.id), None);
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.in_flight_binding_invalidations, 0);
    assert_eq!(diagnostics.in_flight_binding_locality_detachments, 1);

    assert!(broker.mark_execution_completed(fallback.id));
    let fallback_resolution = broker.resolve_execution(fallback.id, true);
    assert_eq!(
        fallback_resolution.completion,
        FrameRequestCompletion::CacheOnly
    );
    assert_eq!(fallback_resolution.binding, None);
}

#[test]
fn newer_generation_prunes_fallback_before_reusable_or_non_reusable_completion() {
    for reusable in [true, false] {
        let broker = FrameWorkBroker::new(2, 2);
        let first = broker.begin_generation();
        broker.submit(request(1, first, FrameWorkClass::Playback));
        let execution = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        let rebound = broker.begin_generation();
        assert!(matches!(
            broker.submit(request(1, rebound, FrameWorkClass::Playback)),
            FrameWorkSubmission::Queued { .. }
        ));

        broker.begin_generation();
        assert_eq!(broker.diagnostics().pending_requests, 0);
        assert_eq!(broker.diagnostics().queued_work, 0);
        assert_eq!(
            broker.resolve_execution(execution.id, reusable).completion,
            FrameRequestCompletion::Stale
        );
        assert_eq!(broker.diagnostics().pending_requests, 0);
        assert_eq!(broker.diagnostics().queued_work, 0);
    }
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
        FrameWorkSubmission::Queued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(broker.execution_cancellation(execution.id), None);
    assert_eq!(broker.diagnostics().queued_work, 1);

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
    broker.submit(request(1, generation, FrameWorkClass::Playback));

    clock.set(Duration::from_millis(149));
    assert!(broker.expire_playback_current_older_than(Duration::from_millis(50)).is_empty());
    clock.set(Duration::from_millis(150));
    let expired = broker.expire_playback_current_older_than(Duration::from_millis(50));

    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].key, 1);
    assert_eq!(expired[0].removed_queued_work, 1);
    assert_eq!(expired[0].retained_in_flight_work, 0);
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
fn explicit_capacity_preemption_marks_one_live_prefetch_without_releasing_it() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(4, 4, clock.clone());
    let generation = broker.begin_generation();
    let mut executions = Vec::new();
    for key in 1..=3 {
        let mut prefetch = request(key, generation, FrameWorkClass::Playback);
        prefetch.priority = FrameWorkPriority::Prefetch;
        broker.submit(prefetch);
        let execution = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        executions.push(execution);
    }
    assert!(broker.mark_execution_completed(executions[0].id));

    clock.set(Duration::from_millis(20));
    assert!(broker.request_one_in_flight_prefetch_preemption());
    assert_eq!(broker.execution_cancellation(executions[0].id), None);
    assert!(matches!(
        broker.execution_cancellation(executions[1].id),
        Some(FrameExecutionCancellation::PrefetchPreemptedByCurrent { .. })
    ));
    assert_eq!(broker.execution_cancellation(executions[2].id), None);
    let first = broker.diagnostics();
    assert_eq!(first.in_flight_work, 3);
    assert_eq!(first.in_flight_completed, 1);
    assert_eq!(first.in_flight_cancellation_requested, 1);

    assert!(broker.request_one_in_flight_prefetch_preemption());
    assert!(matches!(
        broker.execution_cancellation(executions[2].id),
        Some(FrameExecutionCancellation::PrefetchPreemptedByCurrent { .. })
    ));
    assert!(!broker.request_one_in_flight_prefetch_preemption());
    let exhausted = broker.diagnostics();
    assert_eq!(exhausted.in_flight_work, 3);
    assert_eq!(exhausted.in_flight_cancellation_requested, 2);

    let preempted_completion = broker.resolve_execution(executions[1].id, true);
    assert_eq!(
        preempted_completion.completion,
        FrameRequestCompletion::Stale
    );
    assert!(preempted_completion.binding.is_none());
    assert!(!broker.has_pending_key(&2));
    assert_eq!(broker.diagnostics().in_flight_work, 2);
}

#[test]
fn playback_current_does_not_preempt_playback_prefetch_session_locality() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut prefetch = request(1, generation, FrameWorkClass::Playback);
    prefetch.priority = FrameWorkPriority::Prefetch;
    prefetch.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(prefetch);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected prefetch receive: {other:?}"),
    };

    broker.submit(request(2, generation, FrameWorkClass::Playback));

    assert_eq!(broker.execution_cancellation(execution.id), None);
}

#[test]
fn interactive_current_still_preempts_playback_prefetch() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut prefetch = request(1, generation, FrameWorkClass::Playback);
    prefetch.priority = FrameWorkPriority::Prefetch;
    prefetch.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(prefetch);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected prefetch receive: {other:?}"),
    };

    broker.submit(request(2, generation, FrameWorkClass::Interactive));

    assert!(matches!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::PrefetchPreemptedByCurrent { .. })
    ));
}

#[test]
fn preempted_completion_preserves_same_key_fallback_dequeued_by_another_worker() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut prefetch = request(1, generation, FrameWorkClass::Playback);
    prefetch.priority = FrameWorkPriority::Prefetch;
    broker.submit(prefetch);
    let preempted = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected prefetch receive: {other:?}"),
    };
    assert!(broker.request_one_in_flight_prefetch_preemption());

    let mut fallback_request = request(1, generation, FrameWorkClass::Playback);
    fallback_request.demand_identity = Some(demand_identity(2));
    assert!(matches!(
        broker.submit(fallback_request),
        FrameWorkSubmission::Queued { .. }
    ));
    let fallback = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected fallback receive: {other:?}"),
    };
    assert_eq!(broker.diagnostics().queued_work, 0);
    assert_eq!(broker.diagnostics().in_flight_work, 2);

    let preempted_resolution = broker.resolve_execution(preempted.id, true);
    assert_eq!(
        preempted_resolution.completion,
        FrameRequestCompletion::Stale
    );
    assert!(preempted_resolution.binding.is_none());
    assert!(broker.has_pending_key(&1));
    assert_eq!(broker.diagnostics().pending_requests, 1);
    assert_eq!(broker.diagnostics().in_flight_work, 1);

    let fallback_resolution = broker.resolve_execution(fallback.id, true);
    assert_eq!(
        fallback_resolution.completion,
        FrameRequestCompletion::Current
    );
    assert_eq!(
        fallback_resolution.binding.expect("fallback binding").demand_identity,
        Some(demand_identity(2))
    );
    assert!(!broker.has_pending_key(&1));
}

#[test]
fn preempted_failure_preserves_same_key_queued_fallback() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut prefetch = request(1, generation, FrameWorkClass::Playback);
    prefetch.priority = FrameWorkPriority::Prefetch;
    broker.submit(prefetch);
    let preempted = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected prefetch receive: {other:?}"),
    };
    assert!(broker.request_one_in_flight_prefetch_preemption());
    assert!(matches!(
        broker.submit(request(1, generation, FrameWorkClass::Playback)),
        FrameWorkSubmission::Queued { .. }
    ));

    assert_eq!(broker.fail_execution(preempted.id), None);
    assert!(broker.has_pending_key(&1));
    assert_eq!(broker.diagnostics().queued_work, 1);
    let fallback = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected fallback receive: {other:?}"),
    };
    assert!(broker.fail_execution(fallback.id).is_some());
    assert!(!broker.has_pending_key(&1));
}

#[test]
fn preempted_failure_preserves_same_key_fallback_dequeued_by_another_worker() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut prefetch = request(1, generation, FrameWorkClass::Playback);
    prefetch.priority = FrameWorkPriority::Prefetch;
    broker.submit(prefetch);
    let preempted = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected prefetch receive: {other:?}"),
    };
    assert!(broker.request_one_in_flight_prefetch_preemption());
    broker.submit(request(1, generation, FrameWorkClass::Playback));
    let fallback = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected fallback receive: {other:?}"),
    };

    assert_eq!(broker.fail_execution(preempted.id), None);
    assert!(broker.has_pending_key(&1));
    assert_eq!(broker.diagnostics().in_flight_work, 1);
    let binding = broker.fail_execution(fallback.id).expect("fallback binding");
    assert_eq!(binding.resource_scope, FrameWorkResourceScope::Shared);
    assert!(!broker.has_pending_key(&1));
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
fn non_reusable_old_execution_requeues_latest_binding_instead_of_orphaning_it() {
    let broker = FrameWorkBroker::new(2, 2);
    let first = broker.begin_generation();
    broker.submit(request(1, first, FrameWorkClass::Playback));
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    let latest = broker.begin_generation();
    assert!(matches!(
        broker.submit(request(1, latest, FrameWorkClass::Playback)),
        FrameWorkSubmission::Queued { .. }
    ));
    assert_eq!(
        broker.resolve_execution(execution.id, false).completion,
        FrameRequestCompletion::Stale
    );
    assert_eq!(broker.diagnostics().pending_requests, 1);
    assert_eq!(broker.diagnostics().queued_work, 1);
    assert_eq!(broker.diagnostics().in_flight_work, 0);

    let replacement = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected replacement receive: {other:?}"),
    };
    assert_eq!(
        broker.resolve_execution(replacement.id, true).completion,
        FrameRequestCompletion::Current
    );
    assert_eq!(broker.diagnostics().pending_requests, 0);
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
    request.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
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
fn in_flight_locality_policy_preserves_execution_but_not_deadline_evidence() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let generation = broker.begin_generation();
    let mut request = request(1, generation, FrameWorkClass::Playback);
    request.deadline = Some(FrameWorkDeadline::from_remaining(
        100,
        Duration::from_millis(20),
    ));
    request.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(request);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    clock.set(Duration::from_millis(35));
    assert_eq!(broker.execution_cancellation(execution.id), None);
    assert_eq!(
        broker.diagnostics().in_flight_cancellation_requested,
        0,
        "a presentation miss alone must not request locality-preserving execution cancellation"
    );
    assert!(broker.mark_execution_completed(execution.id));
    let resolution = broker.resolve_execution(execution.id, true);

    assert_eq!(resolution.completion, FrameRequestCompletion::Current);
    assert_eq!(
        resolution.deadline,
        FrameWorkDeadlineStatus::Missed { late_by: Duration::from_millis(5) }
    );
}

#[test]
fn stalled_binding_expiration_detaches_locality_execution_as_cache_only() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let generation = broker.begin_generation();
    let mut request = request(1, generation, FrameWorkClass::Playback);
    request.deadline = Some(FrameWorkDeadline::from_remaining(
        100,
        Duration::from_millis(20),
    ));
    request.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(request);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    clock.set(Duration::from_millis(60));
    let expired = broker.expire_playback_current_older_than(Duration::from_millis(50));

    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].removed_queued_work, 0);
    assert_eq!(expired[0].retained_in_flight_work, 1);
    assert!(!broker.has_pending_key(&1));
    assert_eq!(broker.execution_cancellation(execution.id), None);
    assert!(broker.mark_execution_completed(execution.id));
    let resolution = broker.resolve_execution(execution.id, true);
    assert_eq!(resolution.completion, FrameRequestCompletion::CacheOnly);
    assert_eq!(resolution.binding, None);
    assert_eq!(
        resolution.deadline,
        FrameWorkDeadlineStatus::Missed { late_by: Duration::from_millis(30) }
    );
}

#[test]
fn detached_locality_execution_cannot_bind_to_a_later_same_key_demand() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
    let generation = broker.begin_generation();
    let mut original = request(1, generation, FrameWorkClass::Playback);
    original.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(original);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    clock.set(Duration::from_millis(60));
    assert_eq!(
        broker.expire_playback_current_older_than(Duration::from_millis(50)).len(),
        1
    );

    assert!(matches!(
        broker.submit(request(1, generation, FrameWorkClass::Playback)),
        FrameWorkSubmission::Queued { .. }
    ));
    assert!(broker.mark_execution_completed(execution.id));
    let old_resolution = broker.resolve_execution(execution.id, true);
    assert_eq!(old_resolution.completion, FrameRequestCompletion::CacheOnly);
    assert_eq!(old_resolution.binding, None);
    assert!(broker.has_pending_key(&1));
    assert_eq!(broker.diagnostics().queued_work, 1);
}

#[test]
fn detached_locality_execution_does_not_mask_generation_supersession() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let generation = broker.begin_generation();
    let mut request = request(1, generation, FrameWorkClass::Playback);
    request.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(request);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    clock.set(Duration::from_millis(60));
    broker.expire_playback_current_older_than(Duration::from_millis(50));

    broker.begin_generation();
    assert!(matches!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::Superseded { .. })
    ));
    assert_eq!(
        broker.resolve_execution(execution.id, true).completion,
        FrameRequestCompletion::Stale
    );
}

#[test]
fn detached_locality_execution_does_not_mask_explicit_key_cancel() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let generation = broker.begin_generation();
    let mut request = request(1, generation, FrameWorkClass::Playback);
    request.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(request);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    clock.set(Duration::from_millis(60));
    broker.expire_playback_current_older_than(Duration::from_millis(50));

    assert_eq!(broker.cancel_key(&1), 0);
    assert!(matches!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::Superseded { .. })
    ));
}

#[test]
fn detached_locality_execution_does_not_mask_broker_close() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let generation = broker.begin_generation();
    let mut request = request(1, generation, FrameWorkClass::Playback);
    request.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(request);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    clock.set(Duration::from_millis(60));
    broker.expire_playback_current_older_than(Duration::from_millis(50));

    broker.close();
    assert!(matches!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::BrokerClosed { .. })
    ));
}

#[test]
fn in_flight_locality_policy_does_not_mask_generation_supersession() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let generation = broker.begin_generation();
    let mut request = request(1, generation, FrameWorkClass::Playback);
    request.deadline = Some(FrameWorkDeadline::from_remaining(
        100,
        Duration::from_millis(20),
    ));
    request.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(request);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    clock.set(Duration::from_millis(35));
    broker.begin_generation();
    assert!(matches!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::Superseded { .. })
    ));
}

#[test]
fn in_flight_locality_policy_does_not_mask_broker_close() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let generation = broker.begin_generation();
    let mut request = request(1, generation, FrameWorkClass::Playback);
    request.deadline = Some(FrameWorkDeadline::from_remaining(
        100,
        Duration::from_millis(20),
    ));
    request.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(request);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    clock.set(Duration::from_millis(35));
    broker.close();
    assert!(matches!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::BrokerClosed { .. })
    ));
}

#[test]
fn in_flight_locality_policy_does_not_mask_explicit_preemption() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
    let generation = broker.begin_generation();
    let mut speculative = request(1, generation, FrameWorkClass::Playback);
    speculative.priority = FrameWorkPriority::Prefetch;
    speculative.deadline = Some(FrameWorkDeadline::from_remaining(
        100,
        Duration::from_millis(20),
    ));
    speculative.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    broker.submit(speculative);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    clock.set(Duration::from_millis(35));
    // A same-class Playback current admission preserves the prefetch lease's
    // session locality instead of preempting it (only an Interactive current
    // preempts Playback prefetch work); the locality policy does not turn a
    // same-class current admission into a hidden cancellation.
    broker.submit(request(2, generation, FrameWorkClass::Playback));
    assert_eq!(broker.execution_cancellation(execution.id), None);
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
fn execution_cancellation_budget_starts_at_dequeue_and_has_exact_five_ms_boundary() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let generation = broker.begin_generation();
    let mut request = request(1, generation, FrameWorkClass::Playback);
    request.in_flight_deadline_policy = FrameInFlightDeadlinePolicy::FinishForLocality;
    request.execution_cancellation_budget = Some(Duration::from_millis(5));
    broker.submit(request);

    clock.set(Duration::from_millis(20));
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    clock.set(Duration::from_micros(24_999));
    assert_eq!(broker.execution_cancellation(execution.id), None);
    clock.set(Duration::from_micros(25_001));
    assert_eq!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::ExecutionBudgetExpired { age: Duration::from_micros(1) })
    );
}

#[test]
fn execution_terminal_wait_reports_timeout_cancellation_completion_and_missing() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(20));
    let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
    let generation = broker.begin_generation();
    let mut canceling = request(1, generation, FrameWorkClass::Playback);
    canceling.execution_cancellation_budget = Some(Duration::from_millis(5));
    broker.submit(canceling);
    let canceling = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    assert_eq!(
        broker.wait_for_execution_terminal_state(canceling.id, Duration::ZERO),
        FrameExecutionWaitStatus::Timeout
    );
    clock.set(Duration::from_micros(25_001));
    assert_eq!(
        broker.wait_for_execution_terminal_state(canceling.id, Duration::ZERO),
        FrameExecutionWaitStatus::Canceled(FrameExecutionCancellationEvidence {
            cancellation: FrameExecutionCancellation::ExecutionBudgetExpired {
                age: Duration::from_micros(1),
            },
            execution_age: Duration::from_micros(5_001),
        })
    );

    broker.submit(request(2, generation, FrameWorkClass::Playback));
    let completed = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    let barrier = Arc::new(Barrier::new(2));
    let waiter_broker = broker.clone();
    let waiter_barrier = Arc::clone(&barrier);
    let waiter = std::thread::spawn(move || {
        waiter_barrier.wait();
        waiter_broker.wait_for_execution_terminal_state(completed.id, Duration::from_secs(1))
    });
    barrier.wait();
    assert!(broker.mark_execution_completed(completed.id));
    assert_eq!(
        waiter.join().expect("terminal waiter"),
        FrameExecutionWaitStatus::Completed
    );
    broker.resolve_execution(completed.id, true);
    assert_eq!(
        broker.wait_for_execution_terminal_state(completed.id, Duration::ZERO),
        FrameExecutionWaitStatus::Missing
    );

    broker.submit(request(3, generation, FrameWorkClass::Playback));
    let closing = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected closing receive: {other:?}"),
    };
    let barrier = Arc::new(Barrier::new(2));
    let waiter_broker = broker.clone();
    let waiter_barrier = Arc::clone(&barrier);
    let waiter = std::thread::spawn(move || {
        waiter_barrier.wait();
        waiter_broker.wait_for_execution_terminal_state(closing.id, Duration::from_secs(1))
    });
    barrier.wait();
    broker.close();
    assert!(matches!(
        waiter.join().expect("close waiter"),
        FrameExecutionWaitStatus::Canceled(FrameExecutionCancellationEvidence {
            cancellation: FrameExecutionCancellation::BrokerClosed { .. },
            ..
        })
    ));
}

#[test]
fn execution_budget_rebind_is_measured_from_original_dequeue() {
    let clock = ManualRuntimeClock::at(Duration::from_millis(10));
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let generation = broker.begin_generation();
    broker.submit(request(1, generation, FrameWorkClass::Playback));
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    clock.set(Duration::from_millis(20));
    let mut rebound = binding_request(1, generation, FrameWorkClass::Playback);
    rebound.execution_cancellation_budget = Some(Duration::from_millis(15));
    assert_eq!(
        broker.bind_existing(rebound),
        FrameWorkBindingSubmission::ReusedInFlight
    );
    clock.set(Duration::from_micros(24_999));
    assert_eq!(broker.execution_cancellation(execution.id), None);
    clock.set(Duration::from_micros(25_001));
    assert_eq!(
        broker.execution_cancellation(execution.id),
        Some(FrameExecutionCancellation::ExecutionBudgetExpired { age: Duration::from_micros(1) })
    );
}

#[test]
fn success_after_execution_budget_cancellation_cannot_publish_current() {
    let clock = ManualRuntimeClock::at(Duration::ZERO);
    let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
    let generation = broker.begin_generation();
    let mut request = request(1, generation, FrameWorkClass::Playback);
    request.execution_cancellation_budget = Some(Duration::from_millis(5));
    broker.submit(request);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    clock.set(Duration::from_micros(5_001));
    assert!(broker.mark_execution_completed(execution.id));
    assert_ne!(
        broker.resolve_execution(execution.id, true).completion,
        FrameRequestCompletion::Current
    );
}

#[test]
fn cancellation_blocked_playback_lane_allows_one_current_non_playback_failover() {
    let clock = ManualRuntimeClock::at(Duration::ZERO);
    let broker = FrameWorkBroker::new_with_clock(4, 4, clock.clone());
    let generation = broker.begin_generation();
    let mut old_request = request(1, generation, FrameWorkClass::Playback);
    old_request.demand_identity = Some(demand_identity(1));
    old_request.execution_cancellation_budget = Some(Duration::from_millis(5));
    broker.submit(old_request);
    broker.synchronize_playback_current_demand(demand_identity(1));
    let old = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected old receive: {other:?}"),
    };

    let mut current_request = request(2, generation, FrameWorkClass::Playback);
    current_request.demand_identity = Some(demand_identity(2));
    broker.submit(current_request);
    broker.synchronize_playback_current_demand(demand_identity(2));
    assert_eq!(
        next_non_playback_current_failover_wait(
            &lock_state(&broker.shared.state),
            FrameWorkerLane::NonPlayback,
            MonotonicTimestamp::ZERO,
        ),
        Some(Duration::from_millis(5))
    );
    assert!(matches!(
        broker.receive_timeout(FrameWorkerLane::NonPlayback, Duration::ZERO),
        FrameWorkReceiveWait::TimedOut
    ));

    clock.set(Duration::from_micros(5_001));
    let current = match broker.receive_timeout(FrameWorkerLane::NonPlayback, Duration::ZERO) {
        FrameWorkReceiveWait::Work(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected failover receive: {other:?}"),
    };
    assert_eq!(current.key, 2);
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.in_flight_work, 2);
    assert_eq!(diagnostics.in_flight_cross_lane_current, 1);

    let mut next_request = request(3, generation, FrameWorkClass::Playback);
    next_request.demand_identity = Some(demand_identity(2));
    broker.submit(next_request);
    assert!(matches!(
        broker.receive_timeout(FrameWorkerLane::NonPlayback, Duration::ZERO),
        FrameWorkReceiveWait::TimedOut
    ));
    assert_eq!(broker.diagnostics().in_flight_work, 2);

    assert!(broker.mark_execution_completed(old.id));
    assert_ne!(
        broker.resolve_execution(old.id, true).completion,
        FrameRequestCompletion::Current
    );
    assert!(broker.mark_execution_completed(current.id));
    assert_eq!(
        broker.resolve_execution(current.id, true).completion,
        FrameRequestCompletion::Current
    );
}

#[test]
fn non_playback_receiver_wakes_at_failover_deadline_before_its_outer_timeout() {
    let broker = FrameWorkBroker::new(4, 4);
    let generation = broker.begin_generation();
    let mut blocked = request(1, generation, FrameWorkClass::Playback);
    blocked.demand_identity = Some(demand_identity(1));
    blocked.execution_cancellation_budget = Some(Duration::from_millis(10));
    broker.submit(blocked);
    broker.synchronize_playback_current_demand(demand_identity(1));
    assert!(matches!(
        broker.receive(FrameWorkerLane::Playback),
        Some(FrameWorkReceive::Ready(_))
    ));

    let mut replacement = request(2, generation, FrameWorkClass::Playback);
    replacement.demand_identity = Some(demand_identity(2));
    broker.submit(replacement);
    broker.synchronize_playback_current_demand(demand_identity(2));

    let started = Instant::now();
    let received = broker.receive_timeout(FrameWorkerLane::NonPlayback, Duration::from_secs(2));
    assert!(matches!(
        received,
        FrameWorkReceiveWait::Work(FrameWorkReceive::Ready(FrameWorkExecution {
            work_class: FrameWorkClass::Playback,
            priority: FrameWorkPriority::Current,
            ..
        }))
    ));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "receiver slept for its outer timeout instead of the known failover deadline"
    );
}

#[test]
fn authorized_playback_failover_precedes_sustained_non_playback_current_work() {
    let clock = ManualRuntimeClock::at(Duration::ZERO);
    let broker = FrameWorkBroker::new_with_clock(16, 16, clock.clone());
    let generation = broker.begin_generation();
    let mut blocked_request = request(1, generation, FrameWorkClass::Playback);
    blocked_request.demand_identity = Some(demand_identity(1));
    blocked_request.execution_cancellation_budget = Some(Duration::from_millis(5));
    broker.submit(blocked_request);
    broker.synchronize_playback_current_demand(demand_identity(1));
    let _blocked = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected blocked receive: {other:?}"),
    };

    for key in 10..14 {
        broker.submit(request(key, generation, FrameWorkClass::Interactive));
    }
    broker.submit(request(20, generation, FrameWorkClass::Still));
    let mut replacement = request(2, generation, FrameWorkClass::Playback);
    replacement.demand_identity = Some(demand_identity(2));
    broker.submit(replacement);
    broker.synchronize_playback_current_demand(demand_identity(2));

    clock.set(Duration::from_micros(5_001));
    let failover = match broker.receive_timeout(FrameWorkerLane::NonPlayback, Duration::ZERO) {
        FrameWorkReceiveWait::Work(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("playback failover must outrank non-playback backlog: {other:?}"),
    };
    assert_eq!(failover.key, 2);
    assert_eq!(failover.work_class, FrameWorkClass::Playback);
    assert_eq!(failover.priority, FrameWorkPriority::Current);
    assert_eq!(broker.diagnostics().in_flight_cross_lane_current, 1);

    let mut next_playback = request(3, generation, FrameWorkClass::Playback);
    next_playback.demand_identity = Some(demand_identity(2));
    broker.submit(next_playback);
    let normal = match broker.receive_timeout(FrameWorkerLane::NonPlayback, Duration::ZERO) {
        FrameWorkReceiveWait::Work(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("normal lane work should remain serviceable: {other:?}"),
    };
    assert_eq!(normal.work_class, FrameWorkClass::Interactive);
    assert_eq!(
        broker.diagnostics().in_flight_cross_lane_current,
        1,
        "one live failover lease must bound cross-lane playback concurrency"
    );
}

#[test]
fn non_playback_failover_never_takes_playback_prefetch() {
    let clock = ManualRuntimeClock::at(Duration::ZERO);
    let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
    let generation = broker.begin_generation();
    let mut current = request(1, generation, FrameWorkClass::Playback);
    current.execution_cancellation_budget = Some(Duration::from_millis(5));
    broker.submit(current);
    let _execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    let mut prefetch = request(2, generation, FrameWorkClass::Playback);
    prefetch.priority = FrameWorkPriority::Prefetch;
    broker.submit(prefetch);

    clock.set(Duration::from_micros(5_001));
    assert!(matches!(
        broker.receive_timeout(FrameWorkerLane::NonPlayback, Duration::ZERO),
        FrameWorkReceiveWait::TimedOut
    ));
    assert_eq!(broker.diagnostics().queued_prefetch, 1);
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
    assert_eq!(
        broker.submit(rebound),
        FrameWorkSubmission::Queued { evicted_prefetch: None, evicted_still: None }
    );

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
    assert_eq!(broker.diagnostics().queued_work, 0);
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
    // An Interactive current admission preempts the Playback prefetch lease;
    // the later generation invalidation must not erase that earliest
    // preemption record. (A same-class Playback current would instead preserve
    // the prefetch session locality and never preempt it.)
    broker.submit(request(2, generation, FrameWorkClass::Interactive));
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
    assert_eq!(broker.prune_obsolete(), 0);
    assert_eq!(broker.diagnostics().pruned_queued, 1);
    broker.submit(request(2, latest, FrameWorkClass::Playback));
    assert_eq!(broker.cancel_key(&2), 1);
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.pending_requests, 0);
    assert_eq!(diagnostics.queued_work, 0);
}

#[test]
fn obsolete_submission_is_distinct_from_capacity_backpressure() {
    let broker = FrameWorkBroker::new(1, 1);
    let obsolete = broker.begin_generation();
    broker.begin_generation();

    assert_eq!(
        broker.submit(request(1, obsolete, FrameWorkClass::Playback)),
        FrameWorkSubmission::DroppedObsoleteGeneration
    );
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.pending_requests, 0);
    assert_eq!(diagnostics.dropped_obsolete_generation, 1);
    assert_eq!(diagnostics.dropped_backpressure, 0);
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
    assert_eq!(diagnostics.pending_requests, 0);
    assert_eq!(diagnostics.in_flight_work, 0);
    assert_eq!(diagnostics.in_flight_any_lane, 0);
    assert_eq!(diagnostics.in_flight_playback_lane, 0);
    assert_eq!(diagnostics.in_flight_interactive_lane, 0);
    assert_eq!(diagnostics.in_flight_still_lane, 0);
    assert_eq!(diagnostics.in_flight_non_playback_lane, 0);
    assert_eq!(diagnostics.in_flight_cross_lane_current, 0);
}

#[test]
fn abandoning_old_execution_preserves_a_real_queued_fallback_owner() {
    let broker = FrameWorkBroker::new(2, 2);
    let first_generation = broker.begin_generation();
    broker.submit(request(7, first_generation, FrameWorkClass::Playback));
    let old_execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    let fallback_generation = broker.begin_generation();
    assert!(matches!(
        broker.submit(request(7, fallback_generation, FrameWorkClass::Interactive)),
        FrameWorkSubmission::Queued { .. }
    ));
    assert!(broker.abandon_execution(old_execution.id));
    let diagnostics = broker.diagnostics();
    assert_eq!(diagnostics.pending_requests, 1);
    assert_eq!(diagnostics.queued_work, 1);
    assert!(broker.binding_has_execution_owner(
        &7,
        fallback_generation,
        FrameWorkClass::Interactive,
        FrameWorkResourceScope::Shared,
        None,
    ));
}

#[test]
fn failed_execution_consumes_its_exact_binding_once() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    assert!(matches!(
        broker.submit(request(7, generation, FrameWorkClass::Playback)),
        FrameWorkSubmission::Queued { .. }
    ));
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    let binding = broker.fail_execution(execution.id).expect("exact failure binding");
    assert_eq!(binding.generation, generation);
    assert_eq!(binding.work_class, FrameWorkClass::Playback);
    assert_eq!(broker.fail_execution(execution.id), None);
    assert!(!broker.has_pending_key(&7));
    assert_eq!(broker.diagnostics().in_flight_work, 0);
}

#[test]
fn old_failure_preserves_cross_binding_fallback_work() {
    let broker = FrameWorkBroker::new(2, 2);
    let first_generation = broker.begin_generation();
    assert!(matches!(
        broker.submit(request(7, first_generation, FrameWorkClass::Playback)),
        FrameWorkSubmission::Queued { .. }
    ));
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };

    let rebound_generation = broker.begin_generation();
    assert_eq!(
        broker.submit(request(7, rebound_generation, FrameWorkClass::Playback)),
        FrameWorkSubmission::Queued { evicted_prefetch: None, evicted_still: None }
    );

    assert_eq!(broker.fail_execution(execution.id), None);
    assert!(broker.has_pending_key(&7));
    assert_eq!(broker.diagnostics().queued_work, 1);
    assert_eq!(broker.diagnostics().in_flight_work, 0);
    let replacement = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected fallback receive: {other:?}"),
    };
    let binding = broker.fail_execution(replacement.id).expect("fallback failure binding");
    assert_eq!(binding.generation, rebound_generation);
    assert!(!broker.has_pending_key(&7));
}

#[test]
fn changed_demand_failure_preserves_fallback_work() {
    let broker = FrameWorkBroker::new(2, 2);
    let generation = broker.begin_generation();
    let mut original = request(7, generation, FrameWorkClass::Playback);
    original.demand_identity = Some(demand_identity(1));
    broker.submit(original);
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    let mut rebound = request(7, generation, FrameWorkClass::Playback);
    rebound.demand_identity = Some(demand_identity(2));
    assert!(matches!(
        broker.submit(rebound),
        FrameWorkSubmission::Queued { .. }
    ));

    assert_eq!(broker.fail_execution(execution.id), None);
    assert!(broker.has_pending_key(&7));
    assert_eq!(broker.diagnostics().queued_work, 1);
    let replacement = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected fallback receive: {other:?}"),
    };
    let binding = broker.fail_execution(replacement.id).expect("exact rebound binding");
    assert_eq!(binding.demand_identity, Some(demand_identity(2)));
    assert!(!broker.has_pending_key(&7));
}

#[test]
fn newer_generation_prunes_fallback_before_old_failure_returns() {
    let broker = FrameWorkBroker::new(2, 2);
    let first = broker.begin_generation();
    broker.submit(request(7, first, FrameWorkClass::Playback));
    let execution = match broker.receive(FrameWorkerLane::Playback) {
        Some(FrameWorkReceive::Ready(execution)) => execution,
        other => panic!("unexpected receive: {other:?}"),
    };
    let rebound = broker.begin_generation();
    assert!(matches!(
        broker.submit(request(7, rebound, FrameWorkClass::Playback)),
        FrameWorkSubmission::Queued { .. }
    ));

    broker.begin_generation();
    assert_eq!(broker.diagnostics().pending_requests, 0);
    assert_eq!(broker.diagnostics().queued_work, 0);
    assert_eq!(broker.fail_execution(execution.id), None);
    assert_eq!(broker.diagnostics().pending_requests, 0);
    assert_eq!(broker.diagnostics().queued_work, 0);
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
    // A same-class Playback current admission preserves the prefetch lease's
    // session locality instead of preempting it; the backpressure drop above
    // must leave every existing binding untouched either way.
    assert_eq!(broker.execution_cancellation(prefetch_execution.id), None);
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
