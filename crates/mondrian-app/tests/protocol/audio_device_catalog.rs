use super::*;
use std::time::{Duration, Instant};

fn poll_terminal(adapter: &mut AudioOutputDeviceCatalogAdapter) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while adapter.worker.is_some() {
        adapter.poll_finished();
        assert!(
            Instant::now() < deadline,
            "discovery worker did not terminate"
        );
        std::thread::yield_now();
    }
}

#[test]
fn catalog_retains_all_refresh_joins_and_rejects_work_after_shutdown() {
    let mut adapter = AudioOutputDeviceCatalogAdapter::prepare();
    adapter.discover = fixture_discovery;
    for _ in 0..3 {
        assert!(adapter.request_refresh());
        poll_terminal(&mut adapter);
    }
    let receipt = adapter.shutdown_until(Instant::now() + Duration::from_secs(5));
    assert_eq!(receipt.workers_started, 3);
    assert_eq!(receipt.workers_joined, 3);
    assert!(receipt.all_resources_released(), "{receipt:?}");
    assert!(!adapter.request_refresh());
    assert!(!adapter.poll_finished());
    assert_eq!(receipt, adapter.shutdown_until(Instant::now()));
}

#[test]
fn catalog_deadline_is_immutable_after_late_native_return() {
    let mut adapter = AudioOutputDeviceCatalogAdapter::prepare();
    let (release, released) = mpsc::channel();
    let (finished, finish) = mpsc::channel();
    assert!(adapter.start_discovery(move || {
        let _ = released.recv_timeout(Duration::from_secs(5));
        let _ = finished.send(());
        fixture_discovery()
    }));
    let before = Instant::now();
    assert!(!adapter.poll_finished());
    assert!(before.elapsed() < Duration::from_millis(100));
    let receipt = adapter.shutdown_until(Instant::now());
    let _ = release.send(());
    finish.recv_timeout(Duration::from_secs(5)).expect("late native return");
    assert_eq!(receipt.deadline_detachments, 1);
    assert!(!receipt.all_resources_released());
    assert_eq!(
        receipt,
        adapter.shutdown_until(Instant::now() + Duration::from_secs(5))
    );
}

#[test]
fn catalog_retains_polled_panic_and_never_destroys_opaque_payload() {
    struct HostilePayload;
    impl Drop for HostilePayload {
        fn drop(&mut self) {
            panic!("opaque payload destructor");
        }
    }
    let mut adapter = AudioOutputDeviceCatalogAdapter::prepare();
    assert!(adapter.start_discovery(|| std::panic::panic_any(HostilePayload)));
    poll_terminal(&mut adapter);
    // A later successful refresh cannot erase historical worker failure.
    assert!(adapter.start_discovery(fixture_discovery));
    poll_terminal(&mut adapter);
    let receipt = adapter.shutdown_until(Instant::now() + Duration::from_secs(5));
    assert_eq!(receipt.workers_started, 2);
    assert_eq!(receipt.workers_joined, 2);
    assert_eq!(receipt.worker_panics, 1);
    assert_eq!(receipt.panic_payloads_abandoned, 1);
    assert_eq!(receipt.results_missing, 1);
    assert!(!receipt.all_resources_released());
}

#[test]
fn catalog_requires_attempted_production_discovery_and_exact_counts() {
    let mut adapter = AudioOutputDeviceCatalogAdapter::prepare();
    adapter.shutdown.initial_discovery_required = true;
    let receipt = adapter.shutdown_until(Instant::now());
    assert!(!receipt.all_resources_released());
    let mut adapter = AudioOutputDeviceCatalogAdapter::prepare();
    assert!(adapter.start_discovery(fixture_discovery));
    poll_terminal(&mut adapter);
    let receipt = adapter.shutdown_until(Instant::now());
    assert!(receipt.all_resources_released());
    let mut corrupted = receipt;
    corrupted.startup_attempts += 1;
    assert!(!corrupted.all_resources_released());
    let mut corrupted = receipt;
    corrupted.workers_joined = 0;
    assert!(!corrupted.all_resources_released());
}

#[test]
#[ignore = "explicitly exercises native audio-device enumeration"]
fn catalog_actual_native_discovery_has_a_joined_shutdown_receipt() {
    let mut adapter = AudioOutputDeviceCatalogAdapter::prepare();
    adapter.shutdown.initial_discovery_required = true;
    assert!(adapter.request_refresh());
    let receipt = adapter.shutdown_until(Instant::now() + Duration::from_secs(15));
    assert_eq!(receipt.workers_started, 1);
    assert!(receipt.all_resources_released(), "{receipt:?}");
}

fn fixture_discovery() -> DiscoveryResult {
    Ok(RealtimeAudioOutputDeviceCatalog {
        host_name: "fixture".to_owned(),
        devices: Vec::new(),
    })
}

#[test]
fn discovery_runs_off_thread_and_publishes_one_immutable_snapshot() {
    let mut adapter = AudioOutputDeviceCatalogAdapter::prepare();
    adapter.discover = fixture_discovery;
    assert!(adapter.request_refresh());
    assert!(
        !adapter.request_refresh(),
        "one attempt owns discovery authority"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while !adapter.poll_finished() {
        assert!(Instant::now() < deadline, "discovery worker did not finish");
        std::thread::yield_now();
    }
    assert_eq!(
        adapter.state(),
        &AudioOutputDeviceCatalogState::Ready(RealtimeAudioOutputDeviceCatalog {
            host_name: "fixture".to_owned(),
            devices: Vec::new(),
        })
    );
}
