use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Instant;

fn install(watch: &PreviewWorkWatch, callback: impl Fn() + Send + Sync + 'static) {
    watch
        .install_waker(callback)
        .unwrap_or_else(|failure| panic!("{}", failure.reason));
}

#[test]
fn publication_before_wait_is_observed_without_sleeping() {
    let (notifier, watch) = preview_work_notification_channel();
    let before = watch.revision();
    let published = notifier.result_became_pollable();

    assert_ne!(published, before);
    assert_eq!(
        watch.wait_for_change(before, Duration::from_secs(1)),
        published
    );
}

#[test]
fn racing_publication_cannot_be_lost_by_bounded_wait() {
    let (notifier, watch) = preview_work_notification_channel();
    let before = watch.revision();
    let worker = thread::spawn(move || notifier.result_became_pollable());

    let observed = watch.wait_for_change(before, Duration::from_secs(1));
    let published = worker.join().expect("notification worker");
    assert_eq!(observed, published);
}

#[test]
fn publication_wakes_a_bounded_wait_before_its_timeout() {
    let (notifier, watch) = preview_work_notification_channel();
    let before = watch.revision();
    let worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        notifier.result_became_pollable()
    });
    let started = Instant::now();

    let observed = watch.wait_for_change(before, Duration::from_millis(500));

    let published = worker.join().expect("notification worker");
    assert_eq!(observed, published);
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "publication should wake the bounded wait instead of paying its timeout"
    );
}

#[test]
fn waker_is_payload_free_and_does_not_advance_revision() {
    let (notifier, watch) = preview_work_notification_channel();
    let wakes = Arc::new(AtomicUsize::new(0));
    let callback_wakes = Arc::clone(&wakes);
    install(&watch, move || {
        callback_wakes.fetch_add(1, Ordering::AcqRel);
    });

    assert_eq!(watch.revision(), PreviewWorkRevision::default());
    assert_eq!(wakes.load(Ordering::Acquire), 1);
    notifier.result_became_pollable();
    assert_eq!(wakes.load(Ordering::Acquire), 2);
    assert_ne!(watch.revision(), PreviewWorkRevision::default());
}

#[test]
fn external_completion_waker_advances_the_shared_preview_revision() {
    let (_notifier, watch) = preview_work_notification_channel();
    let before = watch.revision();
    let completion_waker = watch.completion_waker();

    completion_waker();

    assert_ne!(watch.revision(), before);
}

#[test]
fn panicking_waker_isolated_and_detached_from_worker_publication() {
    let (notifier, watch) = preview_work_notification_channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let callback_calls = Arc::clone(&calls);
    install(&watch, move || {
        callback_calls.fetch_add(1, Ordering::AcqRel);
        panic!("test wake Adapter panic");
    });
    assert_eq!(calls.load(Ordering::Acquire), 1);

    let before = watch.revision();
    let published = notifier.result_became_pollable();
    assert_ne!(published, before);
    assert_eq!(calls.load(Ordering::Acquire), 1);
}

#[test]
fn worker_exit_during_unwind_publishes_terminal_progress() {
    let (notifier, watch) = preview_work_notification_channel();
    let before = watch.revision();
    let worker = thread::spawn(move || {
        let _exit_notification = notifier.worker_exit_notification();
        panic!("test worker failure");
    });

    assert!(worker.join().is_err());
    assert_ne!(watch.revision(), before);
}
