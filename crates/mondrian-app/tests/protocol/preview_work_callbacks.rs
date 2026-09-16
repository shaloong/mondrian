use super::super::{
    preview_work_notification_channel, PreviewCallbackShutdownRejection, PreviewWorkWatch,
};
use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::time::Duration;

fn install(watch: &PreviewWorkWatch, callback: impl Fn() + Send + Sync + 'static) {
    watch
        .install_waker(callback)
        .unwrap_or_else(|failure| panic!("{}", failure.reason));
}

fn close(watch: &PreviewWorkWatch) -> PreviewWorkCallbackEvidence {
    watch.shutdown_until(Instant::now() + Duration::from_secs(5))
}

#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn wait(&self) {
        let mut open = lock_unpoisoned(&self.open);
        while !*open {
            open = self.changed.wait(open).unwrap_or_else(|error| error.into_inner());
        }
    }

    fn release(&self) {
        *lock_unpoisoned(&self.open) = true;
        self.changed.notify_all();
    }
}

struct ReleaseOnDrop(Arc<Gate>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(Instant::now() < deadline, "controlled owner did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}

struct CountDrop(Arc<AtomicUsize>);
impl CountDrop {
    fn touch(&self) {}
}
impl Drop for CountDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

struct HostileDrop(Arc<AtomicUsize>);
impl HostileDrop {
    fn touch(&self) {}
}
impl Drop for HostileDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
        panic!("foreign destructor must not run on a producer");
    }
}

struct BlockingDrop {
    entered: mpsc::Sender<()>,
    gate: Arc<Gate>,
}
impl BlockingDrop {
    fn touch(&self) {}
}
impl Drop for BlockingDrop {
    fn drop(&mut self) {
        let _ = self.entered.send(());
        self.gate.wait();
    }
}

#[test]
fn empty_owner_requires_explicit_close_without_starting_a_worker() {
    let (_notifier, watch) = preview_work_notification_channel();
    assert!(!watch.callback_evidence().all_resources_released());
    let receipt = watch.shutdown_until(Instant::now());
    assert!(receipt.all_resources_released());
    assert!(!receipt.worker_started);
    assert_eq!(receipt.worker, Some(PreviewOwnedWorkerShutdown::NotStarted));
}

#[test]
fn normal_callback_is_destroyed_once_by_the_retirement_owner() {
    let (_notifier, watch) = preview_work_notification_channel();
    let drops = Arc::new(AtomicUsize::new(0));
    let capture = CountDrop(drops.clone());
    install(&watch, move || capture.touch());
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    let receipt = close(&watch);
    assert!(receipt.all_resources_released());
    assert_eq!(receipt.registrations_accepted, 1);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(close(&watch), receipt);
}

#[test]
fn opaque_invocation_payload_and_hostile_capture_are_abandoned_without_drop() {
    let (_notifier, watch) = preview_work_notification_channel();
    let captures = Arc::new(AtomicUsize::new(0));
    let payloads = Arc::new(AtomicUsize::new(0));
    let capture = HostileDrop(captures.clone());
    let payload_drops = payloads.clone();
    let before = watch.revision();
    install(&watch, move || {
        capture.touch();
        std::panic::panic_any(HostileDrop(payload_drops.clone()));
    });
    assert_ne!(
        watch.revision(),
        before,
        "failure publishes its own revision-only edge"
    );
    let receipt = close(&watch);
    assert_eq!(receipt.invocation_panics, 1);
    assert_eq!(receipt.opaque_payloads_abandoned, 1);
    assert_eq!(receipt.registrations_abandoned, 1);
    assert_eq!(captures.load(Ordering::Relaxed), 0);
    assert_eq!(payloads.load(Ordering::Relaxed), 0);
    assert_eq!(receipt.worker, Some(PreviewOwnedWorkerShutdown::Terminated));
    assert!(!receipt.all_resources_released());
}

#[test]
fn destructor_panic_is_retained_separately_from_clean_worker_join() {
    let (_notifier, watch) = preview_work_notification_channel();
    let drops = Arc::new(AtomicUsize::new(0));
    let capture = HostileDrop(drops.clone());
    install(&watch, move || capture.touch());
    let before = watch.revision();
    let receipt = close(&watch);
    assert_ne!(watch.revision(), before);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(receipt.destructor_panics, 1);
    assert_eq!(receipt.registrations_abandoned, 1);
    assert_eq!(receipt.worker, Some(PreviewOwnedWorkerShutdown::Terminated));
    assert!(!receipt.all_resources_released());
}

#[test]
fn opaque_destructor_payload_is_not_destroyed_by_the_retirement_worker() {
    struct OpaqueDrop(Arc<AtomicUsize>);
    impl OpaqueDrop {
        fn touch(&self) {}
    }
    impl Drop for OpaqueDrop {
        fn drop(&mut self) {
            std::panic::panic_any(HostileDrop(self.0.clone()));
        }
    }
    let (_notifier, watch) = preview_work_notification_channel();
    let drops = Arc::new(AtomicUsize::new(0));
    let capture = OpaqueDrop(drops.clone());
    install(&watch, move || capture.touch());
    let receipt = close(&watch);
    assert_eq!(receipt.destructor_panics, 1);
    assert_eq!(receipt.opaque_payloads_abandoned, 1);
    assert_eq!(receipt.registrations_abandoned, 1);
    assert_eq!(receipt.worker, Some(PreviewOwnedWorkerShutdown::Terminated));
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert!(!receipt.all_resources_released());
}

#[test]
fn blocked_invocation_is_retained_after_deadline_and_late_return_cannot_upgrade() {
    let (notifier, watch) = preview_work_notification_channel();
    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(gate.clone());
    let (entered, waiting) = mpsc::channel();
    let calls = AtomicUsize::new(0);
    let callback_gate = gate.clone();
    install(&watch, move || {
        if calls.fetch_add(1, Ordering::Relaxed) > 0 {
            entered.send(()).expect("in-flight");
            callback_gate.wait();
        }
    });
    let producer = std::thread::spawn(move || notifier.result_became_pollable());
    waiting.recv_timeout(Duration::from_secs(5)).expect("blocked invocation");
    let receipt = watch.shutdown_until(Instant::now());
    assert_eq!(receipt.invocations_in_flight, 1);
    assert_eq!(receipt.registrations_retained, 1);
    assert_eq!(
        receipt.worker,
        Some(PreviewOwnedWorkerShutdown::TimedOutDetached)
    );
    assert!(!receipt.all_resources_released());
    gate.release();
    producer.join().expect("producer returns");
    wait_until(|| lock_unpoisoned(&watch.shared.callbacks.retirement.state).completed_at.is_some());
    assert_eq!(close(&watch), receipt);
}

#[test]
fn blocking_destructor_cannot_block_deadline_or_upgrade_terminal_receipt() {
    let (_notifier, watch) = preview_work_notification_channel();
    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(gate.clone());
    let (entered, waiting) = mpsc::channel();
    let capture = BlockingDrop { entered, gate: gate.clone() };
    install(&watch, move || capture.touch());
    watch.begin_shutdown();
    waiting.recv_timeout(Duration::from_secs(5)).expect("destructor started");
    let started = Instant::now();
    let receipt = watch.shutdown_until(started);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(receipt.retirements_active, 1);
    assert_eq!(
        receipt.worker,
        Some(PreviewOwnedWorkerShutdown::TimedOutDetached)
    );
    assert!(!receipt.all_resources_released());
    gate.release();
    wait_until(|| lock_unpoisoned(&watch.shared.callbacks.retirement.state).completed_at.is_some());
    assert_eq!(
        close(&watch),
        receipt,
        "late return cannot repair the sealed receipt"
    );
}

#[test]
fn invocation_reentrant_unbounded_shutdown_rejects_without_consuming_handle() {
    let (_notifier, watch) = preview_work_notification_channel();
    let callback_watch = watch.clone();
    let (send, received) = mpsc::channel();
    install(&watch, move || {
        send.send(callback_watch.shutdown_and_wait()).expect("record rejection");
    });
    let rejected = received.recv_timeout(Duration::from_secs(1)).expect("no self-dependent join");
    assert_eq!(
        rejected.shutdown_rejection,
        Some(PreviewCallbackShutdownRejection::CurrentInvocation)
    );
    assert!(!rejected.all_resources_released());
    assert!(close(&watch).all_resources_released());
}

#[test]
fn destructor_reentrant_shutdown_leaves_the_handle_for_the_runtime_consumer() {
    struct ReentrantDrop(PreviewWorkWatch, mpsc::Sender<PreviewWorkCallbackEvidence>);
    impl ReentrantDrop {
        fn touch(&self) {}
    }
    impl Drop for ReentrantDrop {
        fn drop(&mut self) {
            self.1.send(self.0.shutdown_and_wait()).expect("record rejection");
        }
    }
    let (_notifier, watch) = preview_work_notification_channel();
    let (send, received) = mpsc::channel();
    let capture = ReentrantDrop(watch.clone(), send);
    install(&watch, move || capture.touch());
    watch.begin_shutdown();
    let rejected = received
        .recv_timeout(Duration::from_secs(5))
        .expect("destructor re-entry returned");
    assert_eq!(
        rejected.shutdown_rejection,
        Some(PreviewCallbackShutdownRejection::CurrentInvocation)
    );
    assert!(close(&watch).all_resources_released());
}

#[test]
fn concurrent_consumer_rejection_cannot_overwrite_the_accepted_receipt() {
    let (_notifier, watch) = preview_work_notification_channel();
    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(gate.clone());
    let (entered, waiting) = mpsc::channel();
    let capture = BlockingDrop { entered, gate: gate.clone() };
    install(&watch, move || capture.touch());
    let consumer_watch = watch.clone();
    let consumer = std::thread::spawn(move || consumer_watch.shutdown_and_wait());
    waiting.recv_timeout(Duration::from_secs(5)).expect("retirement is blocked");
    wait_until(|| lock_unpoisoned(&watch.shared.callbacks.worker).is_none());
    let rejected = watch.shutdown_until(Instant::now());
    assert_eq!(
        rejected.shutdown_rejection,
        Some(PreviewCallbackShutdownRejection::ConcurrentConsumer)
    );
    assert!(!rejected.all_resources_released());
    assert!(lock_unpoisoned(&watch.shared.callbacks.retirement.state).terminal.is_none());
    gate.release();
    let receipt = consumer.join().expect("accepted consumer returned");
    assert!(receipt.all_resources_released());
    assert_eq!(close(&watch), receipt);
}

#[test]
fn a_late_old_failure_does_not_detach_the_replacement() {
    let (notifier, watch) = preview_work_notification_channel();
    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(gate.clone());
    let (entered, waiting) = mpsc::channel();
    let calls = AtomicUsize::new(0);
    let callback_gate = gate.clone();
    install(&watch, move || {
        if calls.fetch_add(1, Ordering::Relaxed) > 0 {
            entered.send(()).expect("old invocation entered");
            callback_gate.wait();
            panic!("old callback fails late");
        }
    });
    let producer_notifier = notifier.clone();
    let producer = std::thread::spawn(move || producer_notifier.result_became_pollable());
    waiting.recv_timeout(Duration::from_secs(5)).expect("old callback in flight");
    let new_calls = Arc::new(AtomicUsize::new(0));
    let callback_calls = new_calls.clone();
    install(&watch, move || {
        callback_calls.fetch_add(1, Ordering::Relaxed);
    });
    gate.release();
    producer.join().expect("panic contained inside notifier");
    notifier.result_became_pollable();
    assert_eq!(new_calls.load(Ordering::Relaxed), 2);
    let receipt = close(&watch);
    assert_eq!(receipt.registrations_accepted, 2);
    assert_eq!(receipt.registrations_released, 1);
    assert_eq!(receipt.registrations_abandoned, 1);
}

#[test]
fn concurrent_panics_count_each_invocation_but_abandon_one_registration() {
    let (notifier, watch) = preview_work_notification_channel();
    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(gate.clone());
    let (entered, waiting) = mpsc::channel();
    let calls = AtomicUsize::new(0);
    let callback_gate = gate.clone();
    install(&watch, move || {
        if calls.fetch_add(1, Ordering::Relaxed) > 0 {
            entered.send(()).expect("admitted invocation");
            callback_gate.wait();
            panic!("concurrent callback failure");
        }
    });
    let mut producers = Vec::new();
    for _ in 0..3 {
        let notifier = notifier.clone();
        producers.push(std::thread::spawn(move || {
            notifier.result_became_pollable()
        }));
        waiting
            .recv_timeout(Duration::from_secs(5))
            .expect("concurrent invocation admitted");
    }
    gate.release();
    for producer in producers {
        producer.join().expect("producer returned");
    }
    let receipt = close(&watch);
    assert_eq!(receipt.invocation_panics, 3);
    assert_eq!(receipt.registrations_abandoned, 1);
    assert_eq!(receipt.invocations_in_flight, 0);
}

#[test]
fn startup_and_closed_rejection_return_unaccepted_callback_ownership() {
    let (_notifier, watch) = preview_work_notification_channel();
    let drops = Arc::new(AtomicUsize::new(0));
    let capture = CountDrop(drops.clone());
    let rejected = watch
        .shared
        .callbacks
        .install_with_spawn(
            &watch.shared,
            move || capture.touch(),
            |_| Err(std::io::Error::other("injected retirement start failure")),
        )
        .expect_err("worker start must reject");
    assert!(matches!(
        rejected.reason,
        RegistrationRejectionReason::WorkerStart(_)
    ));
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    drop(rejected.callback);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    let receipt = close(&watch);
    assert_eq!(receipt.worker_start_failures, 1);
    assert!(!receipt.all_resources_released());
    let capture = CountDrop(drops.clone());
    let rejected = watch.install_waker(move || capture.touch()).expect_err("closed registration");
    assert!(matches!(
        rejected.reason,
        RegistrationRejectionReason::Closed
    ));
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    drop(rejected.callback);
    assert_eq!(drops.load(Ordering::Relaxed), 2);
}

#[test]
fn registration_capacity_includes_all_in_flight_replaced_owners() {
    let (notifier, watch) = preview_work_notification_channel();
    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(gate.clone());
    let (entered, waiting) = mpsc::channel();
    let mut producers = Vec::new();
    for _ in 0..REGISTRATION_CAPACITY {
        let calls = AtomicUsize::new(0);
        let entered = entered.clone();
        let callback_gate = gate.clone();
        install(&watch, move || {
            if calls.fetch_add(1, Ordering::Relaxed) > 0 {
                entered.send(()).expect("retain invocation");
                callback_gate.wait();
            }
        });
        let notifier = notifier.clone();
        producers.push(std::thread::spawn(move || {
            notifier.result_became_pollable()
        }));
        waiting.recv_timeout(Duration::from_secs(5)).expect("registration retained");
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let capture = CountDrop(drops.clone());
    let rejected = watch.install_waker(move || capture.touch()).expect_err("bounded capacity");
    assert!(matches!(
        rejected.reason,
        RegistrationRejectionReason::Capacity
    ));
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    drop(rejected.callback);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    watch.begin_shutdown();
    gate.release();
    for producer in producers {
        producer.join().expect("producer completed");
    }
    let receipt = close(&watch);
    assert!(receipt.all_resources_released());
    assert_eq!(receipt.registrations_accepted, REGISTRATION_CAPACITY as u64);
}

#[test]
fn clean_callback_receipt_has_strict_non_default_serialization() {
    let (_notifier, watch) = preview_work_notification_channel();
    let receipt = close(&watch);
    let value = serde_json::to_value(receipt).expect("raw receipt");
    let decoded: PreviewWorkCallbackEvidence =
        serde_json::from_value(value.clone()).expect("round trip");
    assert!(decoded.all_resources_released());
    for field in value.as_object().expect("object").keys() {
        let mut missing = value.clone();
        missing.as_object_mut().expect("object").remove(field);
        assert!(
            serde_json::from_value::<PreviewWorkCallbackEvidence>(missing).is_err(),
            "{field}"
        );
    }
    assert!(!PreviewWorkCallbackEvidence {
        worker_started: true,
        worker: Some(PreviewOwnedWorkerShutdown::Terminated),
        ..receipt
    }
    .all_resources_released());
    assert!(!PreviewWorkCallbackEvidence::default().all_resources_released());
}

#[test]
fn already_completed_worker_cannot_upgrade_an_expired_deadline() {
    let (_notifier, watch) = preview_work_notification_channel();
    install(&watch, || {});
    let deadline = Instant::now();
    watch.begin_shutdown();
    wait_until(|| {
        lock_unpoisoned(&watch.shared.callbacks.worker)
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
    });
    let receipt = watch.shutdown_until(deadline);
    assert_eq!(receipt.worker, Some(PreviewOwnedWorkerShutdown::Terminated));
    assert!(!receipt.deadline_met);
    assert!(!receipt.all_resources_released());
    assert_eq!(watch.shutdown_and_wait(), receipt);
}

#[test]
fn registration_capacity_includes_a_blocked_active_destructor() {
    let (notifier, watch) = preview_work_notification_channel();
    let destructor_gate = Arc::new(Gate::default());
    let _release_destructor = ReleaseOnDrop(destructor_gate.clone());
    let (entered_drop, dropping) = mpsc::channel();
    let capture = BlockingDrop {
        entered: entered_drop,
        gate: destructor_gate.clone(),
    };
    install(&watch, move || capture.touch());
    let invocation_gate = Arc::new(Gate::default());
    let _release_invocation = ReleaseOnDrop(invocation_gate.clone());
    let (entered, waiting) = mpsc::channel();
    let mut producers = Vec::new();
    for index in 0..REGISTRATION_CAPACITY - 1 {
        let calls = AtomicUsize::new(0);
        let entered = entered.clone();
        let callback_gate = invocation_gate.clone();
        install(&watch, move || {
            if calls.fetch_add(1, Ordering::Relaxed) > 0 {
                entered.send(()).expect("retain invocation");
                callback_gate.wait();
            }
        });
        if index == 0 {
            dropping.recv_timeout(Duration::from_secs(5)).expect("active destructor");
        }
        let notifier = notifier.clone();
        producers.push(std::thread::spawn(move || {
            notifier.result_became_pollable()
        }));
        waiting.recv_timeout(Duration::from_secs(5)).expect("retained owner");
    }
    let rejected = watch.install_waker(|| {}).expect_err("active Drop counts toward capacity");
    assert!(matches!(
        rejected.reason,
        RegistrationRejectionReason::Capacity
    ));
    assert_eq!(watch.callback_evidence().retirements_active, 1);
    watch.begin_shutdown();
    invocation_gate.release();
    destructor_gate.release();
    for producer in producers {
        producer.join().expect("producer completed");
    }
    assert!(close(&watch).all_resources_released());
}
