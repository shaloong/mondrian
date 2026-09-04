use super::*;

#[test]
fn cancellation_returns_while_native_startup_is_blocked_and_reaps_late_child() {
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let release_rx = Mutex::new(release_rx);
    let mut decoder = PersistentFfmpegAudioWindowDecoder::with_capacity(1);
    decoder.native_spawn_for_test = Some(Arc::new(move |_| {
        let _ = entered_tx.send(());
        let _ = release_rx.lock().recv();
        let mut child = Command::new(std::env::current_exe()?);
        child
            .arg("shutdown_child_fixture")
            .env("MONDRIAN_AUDIO_SHUTDOWN_CHILD_FIXTURE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_child_window(&mut child);
        child.spawn()
    }));
    let decoder = Arc::new(decoder);
    let cancellation = ExecutionCancellationToken::new();
    let reader_decoder = Arc::clone(&decoder);
    let reader_cancellation = cancellation.clone();
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let source = tests::test_session_key(0).source;
        let result = reader_decoder.decode_window(
            &source,
            0,
            2048,
            48_000,
            AudioChannelLayout::Stereo,
            &reader_cancellation,
        );
        let canceled = result.is_err_and(|error| error.to_string().contains("canceled"));
        let _ = result_tx.send(canceled);
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    cancellation.cancel();
    let before_release = result_rx.recv_timeout(Duration::from_secs(2)).ok();
    let physical_while_blocked = decoder.session_permits.diagnostics().0;

    // Always unblock and consume real owners before checking the red/green signal.
    let _ = release_tx.send(());
    let reader_joined = reader.join().is_ok();
    let canceled = before_release.or_else(|| result_rx.recv_timeout(Duration::from_secs(2)).ok());
    let evidence = decoder.shutdown_sessions();
    assert!(
        entered,
        "test must reach the real native child-creation seam"
    );
    assert!(reader_joined);
    assert_eq!(canceled, Some(true));
    assert_eq!(physical_while_blocked, 1);
    assert_eq!(evidence.child_processes_observed, 1);
    assert_eq!(evidence.child_processes_terminated, 1);
    assert!(evidence.all_resources_released(), "{evidence:?}");
    assert_eq!(decoder.session_permits.diagnostics().0, 0);
    assert_eq!(evidence.startup.workers_started, 1);
    assert_eq!(evidence.startup.workers_joined, 1);
    assert_eq!(evidence.startup.requests_retired, 1);
    assert_eq!(
        before_release,
        Some(true),
        "cancellation must not wait for native spawn"
    );
}

#[test]
fn deadline_receipt_stays_dirty_after_late_native_startup_is_reaped() {
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let release_rx = Mutex::new(release_rx);
    let decoder = Arc::new(
        PersistentFfmpegAudioWindowDecoder::with_native_spawn_for_test(
            1,
            Arc::new(move |_| {
                let _ = entered_tx.send(());
                let _ = release_rx.lock().recv();
                fixture_child()
            }),
        ),
    );
    let lane = Arc::clone(&decoder.startup);
    let reading = Arc::clone(&decoder);
    let cache = crate::AudioSourceCache::with_decoder(48_000, 1, 1, 8192, 1, decoder);
    let reader = std::thread::spawn(move || {
        reading.decode_window(
            &tests::test_session_key(0).source,
            0,
            2048,
            48_000,
            AudioChannelLayout::Stereo,
            &ExecutionCancellationToken::new(),
        )
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    cache.begin_shutdown();
    let read = reader.join();
    let receipt = cache.shutdown_until(Instant::now() + Duration::from_millis(20));
    let frozen = receipt;
    let _ = release_tx.send(());
    let deadline = Instant::now() + Duration::from_secs(2);
    while !lane.join(None).all_resources_released() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(entered);
    assert!(read.is_ok_and(|result| result.is_err()));
    assert!(
        lane.join(None).all_resources_released(),
        "late startup really joined"
    );
    assert_eq!(receipt, frozen);
    assert_eq!(receipt.shutdown_coordinator_timeouts, 1);
    assert_eq!(receipt.shutdown_coordinator_detachments, 1);
    assert!(!receipt.shutdown_resource_facts_complete_at_deadline);
    assert!(receipt.shutdown_owner_lifetime_unresolved_at_deadline);
    assert!(!receipt.all_resources_released());
}

#[test]
fn queued_cancellation_never_starts_a_second_child_or_releases_inflight_capacity_early() {
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let release_rx = Mutex::new(release_rx);
    let decoder = Arc::new(
        PersistentFfmpegAudioWindowDecoder::with_native_spawn_for_test(
            2,
            Arc::new(move |_| {
                let _ = entered_tx.send(());
                let _ = release_rx.lock().recv_timeout(Duration::from_secs(10));
                fixture_child()
            }),
        ),
    );
    let first_cancel = ExecutionCancellationToken::new();
    let second_cancel = ExecutionCancellationToken::new();
    let (finished_tx, finished_rx) = mpsc::sync_channel(2);
    let launch = |index, token: ExecutionCancellationToken| {
        let reading = Arc::clone(&decoder);
        let finished = finished_tx.clone();
        std::thread::spawn(move || {
            let result = reading.decode_window(
                &tests::test_session_key(index).source,
                0,
                2048,
                48_000,
                AudioChannelLayout::Stereo,
                &token,
            );
            let _ = finished.send(result.is_err());
            result
        })
    };
    let first = launch(0, first_cancel.clone());
    let entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    let second = launch(1, second_cancel.clone());
    let deadline = Instant::now() + Duration::from_secs(2);
    while decoder.startup.join(None).queued_remaining != 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    let queued = decoder.startup.join(None).queued_remaining;
    first_cancel.cancel();
    second_cancel.cancel();
    let first_before_release = finished_rx.recv_timeout(Duration::from_secs(2)).ok();
    let second_before_release = finished_rx.recv_timeout(Duration::from_secs(2)).ok();
    let physical_before_release = decoder.session_permits.diagnostics().0;
    let _ = release_tx.send(());
    let first_result = first.join();
    let second_result = second.join();
    let evidence = decoder.shutdown_sessions();
    assert!(entered);
    assert_eq!(queued, 1);
    assert!(first_result.is_ok_and(|result| result.is_err()));
    assert!(second_result.is_ok_and(|result| result.is_err()));
    assert_eq!(physical_before_release, 2);
    assert_eq!(evidence.child_processes_observed, 1);
    assert_eq!(evidence.child_processes_terminated, 1);
    assert_eq!(evidence.startup.requests_admitted, 2);
    assert_eq!(evidence.startup.requests_retired, 2);
    assert_eq!(evidence.startup.canceled_before_spawn, 1);
    assert!(evidence.all_resources_released(), "{evidence:?}");
    assert_eq!(first_before_release, Some(true));
    assert_eq!(second_before_release, Some(true));
}

fn fixture_child() -> std::io::Result<Child> {
    let mut child = Command::new(std::env::current_exe()?);
    child
        .arg("shutdown_child_fixture")
        .env("MONDRIAN_AUDIO_SHUTDOWN_CHILD_FIXTURE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_child_window(&mut child);
    child.spawn()
}

#[test]
fn startup_worker_creation_failure_preserves_the_returned_teardown_owner() {
    let decoder = PersistentFfmpegAudioWindowDecoder::with_state_and_spawners(
        DecoderState::new(1),
        product_decoder_teardown_spawner(),
        Arc::new(|_| Err(std::io::ErrorKind::WouldBlock.into())),
    );
    let result = decoder.decode_window(
        &tests::test_session_key(0).source,
        0,
        2048,
        48_000,
        AudioChannelLayout::Stereo,
        &ExecutionCancellationToken::new(),
    );
    let evidence = decoder.shutdown_sessions();
    assert!(result.is_err());
    assert_eq!(evidence.shutdown_workers_started, 1);
    assert_eq!(evidence.shutdown_workers_terminated, 1);
    assert!(evidence.startup.required && evidence.startup.attempted);
    assert_eq!(evidence.startup.workers_started, 0);
    assert_eq!(evidence.startup.start_failures, 1);
    assert_eq!(evidence.startup.requests_admitted, 0);
    assert!(!evidence.all_resources_released());
}

#[test]
fn partial_native_result_is_transferred_to_actual_teardown() {
    let decoder = PersistentFfmpegAudioWindowDecoder::with_native_spawn_for_test(
        1,
        Arc::new(|_| {
            let mut child = Command::new(std::env::current_exe()?);
            child
                .arg("shutdown_child_fixture")
                .env("MONDRIAN_AUDIO_SHUTDOWN_CHILD_FIXTURE", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            hide_child_window(&mut child);
            child.spawn()
        }),
    );
    let result = decoder.decode_window(
        &tests::test_session_key(0).source,
        0,
        2048,
        48_000,
        AudioChannelLayout::Stereo,
        &ExecutionCancellationToken::new(),
    );
    let evidence = decoder.shutdown_sessions();
    assert!(result.is_err_and(|error| error.to_string().contains("did not expose stdout")));
    assert_eq!(evidence.child_processes_observed, 1);
    assert_eq!(evidence.child_processes_terminated, 1);
    assert_eq!(evidence.stdout_pump_threads_observed, 0);
    assert_eq!(evidence.startup.requests_claimed, 1);
    assert!(evidence.all_resources_released(), "{evidence:?}");
}

#[test]
fn ready_unclaimed_completion_keeps_teardown_open_until_actual_retirement() {
    let decoder = Arc::new(PersistentFfmpegAudioWindowDecoder::with_capacity(1));
    let key = tests::test_session_key(0);
    let cancellation = ExecutionCancellationToken::new();
    let permit = decoder
        .session_permits
        .acquire(
            &cancellation,
            &decoder.shutdown_signal,
            &decoder.teardown_faulted,
            &key.source.path,
        )
        .expect("physical permit");
    let completion = decoder
        .startup
        .wait(
            &key,
            0,
            permit,
            &cancellation,
            Some(Arc::new(|_| fixture_child())),
        )
        .expect("unclaimed startup completion");
    decoder.shutdown_signal.request();
    let shutting_down = Arc::clone(&decoder);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let shutdown = std::thread::spawn(move || {
        let evidence = shutting_down.shutdown_sessions();
        let _ = done_tx.send(evidence);
    });
    let early = done_rx.recv_timeout(Duration::from_millis(100)).ok();
    let queue_closed_early = decoder.teardown_queue.state.lock().closed;
    let physical_before_drop = decoder.session_permits.diagnostics().0;
    drop(completion);
    let joined = shutdown.join().is_ok();
    let evidence = early.unwrap_or_else(|| done_rx.recv().expect("terminal evidence"));
    assert!(joined);
    assert!(
        early.is_none(),
        "a live completion is still a teardown producer"
    );
    assert!(!queue_closed_early);
    assert_eq!(physical_before_drop, 1);
    assert_eq!(evidence.child_processes_observed, 1);
    assert_eq!(evidence.child_processes_terminated, 1);
    assert_eq!(evidence.startup.requests_retired, 1);
    assert!(evidence.all_resources_released(), "{evidence:?}");
}

#[test]
fn startup_panic_preserves_unverified_inventory_and_does_not_report_clean() {
    let mut decoder = PersistentFfmpegAudioWindowDecoder::with_capacity(1);
    decoder.native_spawn_for_test = Some(Arc::new(|_| panic!("native startup panic fixture")));
    let result = decoder.decode_window(
        &tests::test_session_key(0).source,
        0,
        2048,
        48_000,
        AudioChannelLayout::Stereo,
        &ExecutionCancellationToken::new(),
    );
    let evidence = decoder.shutdown_sessions();
    assert!(result.is_err());
    assert_eq!(evidence.startup.workers_joined, 1);
    assert_eq!(evidence.startup.panics, 1);
    assert_eq!(evidence.startup.unverified_native_owners, 1);
    assert_eq!(evidence.startup.in_flight_remaining, 1);
    assert!(!evidence.all_resources_released());
}

#[test]
fn shutdown_retires_other_child_while_native_startup_is_still_blocked() {
    shutdown_retires_independent_owner(false);
}

#[test]
fn shutdown_retires_installed_idle_session_while_native_startup_is_still_blocked() {
    shutdown_retires_independent_owner(true);
}

fn shutdown_retires_independent_owner(installed: bool) {
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let release_rx = Mutex::new(release_rx);
    let mut decoder = PersistentFfmpegAudioWindowDecoder::with_capacity(2);
    decoder.native_spawn_for_test = Some(Arc::new(move |_| {
        let _ = entered_tx.send(());
        let _ = release_rx.lock().recv();
        fixture_child()
    }));
    let decoder = Arc::new(decoder);
    let reading = Arc::clone(&decoder);
    let reader = std::thread::spawn(move || {
        reading.decode_window(
            &tests::test_session_key(0).source,
            0,
            2048,
            48_000,
            AudioChannelLayout::Stereo,
            &ExecutionCancellationToken::new(),
        )
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(2)).is_ok();
    let permit = decoder
        .session_permits
        .acquire(
            &ExecutionCancellationToken::new(),
            &decoder.shutdown_signal,
            &decoder.teardown_faulted,
            &tests::test_session_key(1).source.path,
        )
        .expect("second physical permit");
    let owner = if installed {
        let key = tests::test_session_key(1);
        let session = DecodeSession::spawn_with_factories(
            &key,
            0,
            permit,
            crate::ffmpeg_command,
            |_| fixture_child(),
            || false,
        )
        .unwrap_or_else(|_| panic!("independent complete Session"));
        decoder
            .state
            .lock()
            .entries
            .push_back(DecoderEntry { key, slot: Arc::new(Mutex::new(Some(session))) });
        None
    } else {
        Some(PartialDecodeSession::new(
            fixture_child().expect("independent child"),
            permit,
        ))
    };
    decoder.shutdown_signal.request();
    let accepted = owner.is_none_or(|owner| {
        decoder.enqueue_teardown(VecDeque::from([DecoderTeardownOwner::PartialSession(
            owner,
        )]))
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while decoder.session_permits.diagnostics().0 > 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    let physical_while_startup_blocked = decoder.session_permits.diagnostics().0;
    let _ = release_tx.send(());
    let read = reader.join();
    let evidence = decoder.shutdown_sessions();
    assert!(entered && accepted);
    assert!(read.is_ok_and(|result| result.is_err()));
    assert_eq!(evidence.child_processes_observed, 2);
    assert_eq!(evidence.child_processes_terminated, 2);
    assert!(evidence.all_resources_released(), "{evidence:?}");
    assert_eq!(
        physical_while_startup_blocked, 1,
        "startup cannot stall independent retirement"
    );
}
