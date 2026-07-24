use super::*;

#[test]
fn cancellation_policy_owns_speculative_budget_and_session_deadline() {
    assert_eq!(
        media_preview_cancel_reason(
            None,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
            Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US - 1),
            false,
        ),
        None
    );
    assert_eq!(
        media_preview_cancel_reason(
            None,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
            Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US),
            false,
        ),
        Some(MediaPreviewCancelReason::PrefetchDeadline)
    );
    assert_eq!(
        media_preview_cancel_reason_at_checkpoint(
            None,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
            Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US * 4),
            Some(Instant::now() + Duration::from_millis(500)),
        ),
        None,
        "session deadline, not the steady-state speculative budget, owns startup preroll"
    );
}

#[test]
fn cancellation_policy_maps_atomic_broker_dispositions() {
    let cases = [
        (
            mondrian_playback::FrameExecutionCancellation::BrokerClosed { age: Duration::ZERO },
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            MediaPreviewCancelReason::Shutdown,
        ),
        (
            mondrian_playback::FrameExecutionCancellation::Superseded { age: Some(Duration::ZERO) },
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
            MediaPreviewCancelReason::Obsolete,
        ),
        (
            mondrian_playback::FrameExecutionCancellation::PrefetchPreemptedByCurrent {
                request_age: Duration::ZERO,
            },
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
            MediaPreviewCancelReason::PrefetchPreemptedByCurrent,
        ),
        (
            mondrian_playback::FrameExecutionCancellation::StillPreemptedByRealtimeCurrent {
                request_age: Duration::ZERO,
            },
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
            MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent,
        ),
        (
            mondrian_playback::FrameExecutionCancellation::DeadlineExpired { age: Duration::ZERO },
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            MediaPreviewCancelReason::PlaybackDeadline,
        ),
    ];
    for (disposition, priority, access_mode, expected) in cases {
        assert_eq!(
            media_preview_cancel_reason_from_execution(disposition, priority, access_mode),
            expected
        );
    }
    assert_eq!(
        MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent.playback_cause(),
        mondrian_playback::FrameCancellationCause::StillPreemptedByRealtimeCurrent
    );
}

#[test]
fn speculative_cancellation_latency_uses_budget_authority_instant() {
    let started_at = Instant::now();
    let observed_at =
        started_at + Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US + 7);

    assert_eq!(
        media_preview_cancel_request_to_observed_us(
            MediaPreviewCancelReason::PrefetchDeadline,
            None,
            started_at,
            observed_at,
        ),
        Some(7)
    );
}

fn test_media_key(source_frame: i64) -> MediaPreviewKey {
    MediaPreviewKey {
        asset_id: AssetId::new(),
        path: PathBuf::from(format!("E:/media/{source_frame}.mov")),
        fingerprint: None,
        source_time: TimelineTime::new(source_frame, 1).expect("exact source time"),
        target_width: 320,
        target_height: 180,
        source_width: 320,
        source_height: 180,
        input_color_space: ColorSpace::Rec709,
        input_video_range: mondrian_media::DecodedVideoRangeContract::Automatic {
            probed_range: mondrian_media::DecodedVideoRange::Limited,
        },
        native_surface_hint: None,
        source_has_alpha: false,
        alpha_interpretation: AlphaInterpretation::Straight,
        working_color_space: WorkingColorSpace::LinearRec709,
        input_tone_map: false,
        engine: ColorEngine::mondrian_standard(),
    }
}

fn test_media_job(key: MediaPreviewKey, priority: MediaPreviewRequestPriority) -> MediaPreviewJob {
    test_media_job_with_generation(key, 1, priority)
}

fn test_media_job_with_generation(
    key: MediaPreviewKey,
    generation: u64,
    priority: MediaPreviewRequestPriority,
) -> MediaPreviewJob {
    MediaPreviewJob {
        key,
        generation,
        priority,
        access_mode: test_access_mode_for_priority(priority),
        adaptive_hints: PreviewDecodeAdaptiveHints::default(),
        hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
        hardware_decode_device_selector: None,
        enqueued_at: Instant::now(),
        deadline_at: None,
        demand_identity: None,
        execution_id: None,
    }
}

#[test]
fn media_preview_key_does_not_collapse_distinct_exact_source_targets() {
    let mut exact_third = test_media_key(0);
    exact_third.source_time = TimelineTime::ONE_THIRD;
    let mut microsecond_approximation = exact_third.clone();
    microsecond_approximation.source_time =
        TimelineTime::new(333_333, 1_000_000).expect("exact approximation");

    assert_ne!(exact_third, microsecond_approximation);
}

fn test_access_mode_for_priority(priority: MediaPreviewRequestPriority) -> PreviewDecodeAccessMode {
    match priority {
        MediaPreviewRequestPriority::Current => PreviewDecodeAccessMode::ScrubCursor,
        MediaPreviewRequestPriority::Prefetch => PreviewDecodeAccessMode::PlaybackCursor,
    }
}

fn test_scheduler_request(
    scheduler: &MediaPreviewScheduler,
    key: MediaPreviewKey,
    generation: u64,
    priority: MediaPreviewRequestPriority,
) -> MediaPreviewRequestStatus {
    scheduler.request(
        key,
        generation,
        priority,
        test_access_mode_for_priority(priority),
    )
}

fn scheduled_request() -> MediaPreviewRequestStatus {
    MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
}

fn test_scheduler_should_decode(
    scheduler: &MediaPreviewScheduler,
    key: &MediaPreviewKey,
    priority: MediaPreviewRequestPriority,
) -> bool {
    scheduler.should_decode(key, test_access_mode_for_priority(priority))
}

fn test_scheduler_is_decode_current(
    scheduler: &MediaPreviewScheduler,
    key: &MediaPreviewKey,
    generation: u64,
    priority: MediaPreviewRequestPriority,
) -> bool {
    scheduler.is_decode_current(key, generation, test_access_mode_for_priority(priority))
}

fn test_scheduler_complete(
    scheduler: &MediaPreviewScheduler,
    key: &MediaPreviewKey,
    generation: u64,
    priority: MediaPreviewRequestPriority,
) -> bool {
    scheduler
        .complete(key, generation, test_access_mode_for_priority(priority))
        .is_current()
}

fn test_scheduler_completion(
    scheduler: &MediaPreviewScheduler,
    key: &MediaPreviewKey,
    generation: u64,
    priority: MediaPreviewRequestPriority,
) -> MediaPreviewCompletionStatus {
    scheduler.complete(key, generation, test_access_mode_for_priority(priority))
}

#[test]
fn media_preview_scheduler_rejects_non_playback_prefetch_requests() {
    let scheduler = MediaPreviewScheduler::default();
    let generation = scheduler.begin_generation();
    let scrub = test_media_key(1);
    let still = test_media_key(2);

    assert_eq!(
        scheduler.request(
            scrub,
            generation,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::ScrubCursor,
        ),
        MediaPreviewRequestStatus::DroppedInvalidAccessMode
    );
    assert_eq!(
        scheduler.request(
            still,
            generation,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ),
        MediaPreviewRequestStatus::DroppedInvalidAccessMode
    );

    let diagnostics = scheduler.diagnostics();
    assert_eq!(diagnostics.pending_requests, 0);
    assert_eq!(diagnostics.scheduled_requests, 0);
    assert_eq!(diagnostics.dropped_invalid_access_mode_requests, 2);
    assert_eq!(diagnostics.dropped_backpressure_requests, 0);
}

#[test]
fn media_preview_scheduler_skips_obsolete_generations() {
    let scheduler = MediaPreviewScheduler::default();
    let first_generation = scheduler.begin_generation();
    let key = test_media_key(1);
    assert_eq!(
        test_scheduler_request(
            &scheduler,
            key.clone(),
            first_generation,
            MediaPreviewRequestPriority::Current,
        ),
        scheduled_request()
    );

    scheduler.begin_generation();

    assert!(!test_scheduler_should_decode(
        &scheduler,
        &key,
        MediaPreviewRequestPriority::Current
    ));
    assert_eq!(scheduler.pending_len(), 0);
    let diagnostics = scheduler.diagnostics();
    assert_eq!(diagnostics.skipped_decode_obsolete_generation, 1);
    assert_eq!(diagnostics.skipped_decode_missing_pending, 0);
    assert_eq!(diagnostics.skipped_decode_access_mode_mismatch, 0);
}

#[test]
fn media_preview_scheduler_replaces_playback_prefetch_with_scrub_current() {
    let scheduler = MediaPreviewScheduler::default();
    let first_generation = scheduler.begin_generation();
    let key = test_media_key(1);
    assert_eq!(
        test_scheduler_request(
            &scheduler,
            key.clone(),
            first_generation,
            MediaPreviewRequestPriority::Prefetch,
        ),
        scheduled_request()
    );

    let second_generation = scheduler.begin_generation();
    assert_eq!(
        test_scheduler_request(
            &scheduler,
            key.clone(),
            second_generation,
            MediaPreviewRequestPriority::Current,
        ),
        MediaPreviewRequestStatus::AlreadyPending { access_mode_changed: true }
    );

    assert!(test_scheduler_should_decode(
        &scheduler,
        &key,
        MediaPreviewRequestPriority::Current
    ));
    assert!(!test_scheduler_complete(
        &scheduler,
        &key,
        first_generation,
        MediaPreviewRequestPriority::Prefetch
    ));
    let diagnostics = scheduler.diagnostics();
    assert_eq!(diagnostics.already_pending_access_mode_changes, 1);
    assert_eq!(diagnostics.completed_stale_access_mode_mismatch, 1);
    assert_eq!(scheduler.pending_len(), 1);
    assert!(test_scheduler_complete(
        &scheduler,
        &key,
        second_generation,
        MediaPreviewRequestPriority::Current
    ));
    assert_eq!(scheduler.pending_len(), 0);
}

#[test]
fn media_preview_scheduler_reports_other_pending_current_pressure() {
    let scheduler = MediaPreviewScheduler::default();
    let generation = scheduler.begin_generation();
    let prefetch = test_media_key(1);
    let current = test_media_key(2);

    assert_eq!(
        test_scheduler_request(
            &scheduler,
            prefetch.clone(),
            generation,
            MediaPreviewRequestPriority::Prefetch,
        ),
        scheduled_request()
    );
    assert!(!scheduler.has_pending_current_request_other_than(&prefetch));

    assert_eq!(
        test_scheduler_request(
            &scheduler,
            current.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
        ),
        scheduled_request()
    );

    assert!(scheduler.has_pending_current_request_other_than(&prefetch));
    assert!(!scheduler.has_pending_current_request_other_than(&current));
}

#[test]
fn media_preview_scheduler_reports_realtime_current_pressure_for_still_work() {
    let scheduler = MediaPreviewScheduler::with_max_pending(3);
    let generation = scheduler.begin_generation();
    let still = test_media_key(1);
    let other_still = test_media_key(2);
    let scrub = test_media_key(3);

    assert_eq!(
        scheduler.request(
            still.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ),
        scheduled_request()
    );
    assert_eq!(
        scheduler.request(
            other_still.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ),
        scheduled_request()
    );
    assert!(!scheduler.has_pending_realtime_current_request_other_than(&still));

    assert_eq!(
        scheduler.request(
            scrub.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
        ),
        MediaPreviewRequestStatus::Scheduled {
            evicted_prefetch: None,
            evicted_still: Some(Box::new(still.clone())),
        }
    );

    assert!(scheduler.has_pending_realtime_current_request_other_than(&still));
    assert!(!scheduler.has_pending_realtime_current_request_other_than(&scrub));
}

#[test]
fn media_preview_scheduler_expires_only_stalled_realtime_current_requests() {
    let scheduler = MediaPreviewScheduler::with_max_pending(3);
    let generation = scheduler.begin_generation();
    let scrub = test_media_key(1);
    let playback = test_media_key(2);
    let still = test_media_key(3);

    assert_eq!(
        scheduler.request(
            scrub.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
        ),
        scheduled_request()
    );
    assert_eq!(
        scheduler.request(
            playback.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
        ),
        scheduled_request()
    );
    assert_eq!(
        scheduler.request(
            still.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ),
        scheduled_request()
    );

    assert!(scheduler.expire_realtime_current_older_than(Duration::from_secs(60)).is_empty());
    assert_eq!(scheduler.pending_len(), 3);

    let expired = scheduler.expire_realtime_current_older_than(Duration::ZERO);
    assert_eq!(expired.len(), 2);
    assert!(expired.iter().any(|request| request.key == scrub));
    assert!(expired.iter().any(|request| request.key == playback));
    assert!(!expired.iter().any(|request| request.key == still));
    assert!(!scheduler.has_pending_key(&scrub));
    assert!(!scheduler.has_pending_key(&playback));
    assert!(scheduler.has_pending_key(&still));
    assert_eq!(scheduler.diagnostics().canceled_requests, 2);
}

#[test]
fn pending_playback_identity_tracks_latest_demand_for_same_media_key() {
    let scheduler = MediaPreviewScheduler::with_max_pending(1);
    let generation = scheduler.begin_generation();
    let key = test_media_key(4);
    let mut engine = mondrian_playback::PlaybackEngine::new(
        mondrian_core::Rational::new(1, 25),
        mondrian_playback::PlaybackPolicy::default(),
    )
    .expect("playback engine");
    engine
        .play_timeline(
            mondrian_playback::PlaybackTimelineBinding::new(
                None,
                0,
                mondrian_core::Rational::new(1, 25),
                10,
            )
            .expect("timeline binding"),
            mondrian_core::FramePosition::new(0, mondrian_core::Rational::new(1, 25)),
            mondrian_playback::MonotonicTimestamp::ZERO,
        )
        .expect("first demand");
    let first = engine.frame_demand().expect("first frame demand").identity();
    engine
        .play_timeline(
            mondrian_playback::PlaybackTimelineBinding::new(
                None,
                0,
                mondrian_core::Rational::new(1, 25),
                10,
            )
            .expect("timeline binding"),
            mondrian_core::FramePosition::new(0, mondrian_core::Rational::new(1, 25)),
            mondrian_playback::MonotonicTimestamp::ZERO,
        )
        .expect("second demand");
    let second = engine.frame_demand().expect("second frame demand").identity();
    assert_ne!(first, second);
    let first_deadline = Instant::now() + Duration::from_secs(1);
    let second_deadline = Instant::now() + Duration::from_secs(2);

    assert_eq!(
        scheduler.request_with_binding(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(first),
            Some(first_deadline),
        ),
        scheduled_request()
    );
    assert_eq!(
        scheduler.request_with_binding(
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::PlaybackCursor,
            Some(second),
            Some(second_deadline),
        ),
        MediaPreviewRequestStatus::AlreadyPending { access_mode_changed: false }
    );

    let resolution = scheduler.resolve_unleased(
        &key,
        generation,
        PreviewDecodeAccessMode::PlaybackCursor,
        Some(first),
        true,
    );
    assert_eq!(resolution.status, MediaPreviewCompletionStatus::Current);
    assert_eq!(resolution.demand_identity, Some(second));
    assert_eq!(
        resolution.deadline_status,
        mondrian_playback::FrameWorkDeadlineStatus::OnTime
    );
    assert_eq!(scheduler.pending_len(), 0);
}

#[test]
fn media_preview_scheduler_keeps_same_key_in_flight_decode_current_after_rerequest() {
    let scheduler = MediaPreviewScheduler::default();
    let first_generation = scheduler.begin_generation();
    let key = test_media_key(1);

    assert_eq!(
        test_scheduler_request(
            &scheduler,
            key.clone(),
            first_generation,
            MediaPreviewRequestPriority::Current,
        ),
        scheduled_request()
    );
    assert!(test_scheduler_is_decode_current(
        &scheduler,
        &key,
        first_generation,
        MediaPreviewRequestPriority::Current
    ));

    let second_generation = scheduler.begin_generation();
    assert_eq!(
        test_scheduler_request(
            &scheduler,
            key.clone(),
            second_generation,
            MediaPreviewRequestPriority::Current,
        ),
        MediaPreviewRequestStatus::AlreadyPending { access_mode_changed: false }
    );

    assert!(
        test_scheduler_is_decode_current(
            &scheduler,
            &key,
            first_generation,
            MediaPreviewRequestPriority::Current
        ),
        "same frame/key decode must survive UI generation refreshes"
    );
    assert!(test_scheduler_complete(
        &scheduler,
        &key,
        first_generation,
        MediaPreviewRequestPriority::Current
    ));
}

#[test]
fn media_preview_scheduler_prunes_obsolete_pending_requests() {
    let scheduler = MediaPreviewScheduler::default();
    let first_generation = scheduler.begin_generation();
    let first = test_media_key(1);
    let second = test_media_key(2);
    assert_eq!(
        test_scheduler_request(
            &scheduler,
            first.clone(),
            first_generation,
            MediaPreviewRequestPriority::Current,
        ),
        scheduled_request()
    );
    assert_eq!(
        test_scheduler_request(
            &scheduler,
            second.clone(),
            first_generation,
            MediaPreviewRequestPriority::Current,
        ),
        scheduled_request()
    );

    scheduler.begin_generation();
    scheduler.prune_obsolete();

    assert_eq!(scheduler.pending_len(), 0);
    assert!(!test_scheduler_should_decode(
        &scheduler,
        &first,
        MediaPreviewRequestPriority::Current
    ));
    assert!(!test_scheduler_should_decode(
        &scheduler,
        &second,
        MediaPreviewRequestPriority::Current
    ));
}

#[test]
fn media_preview_scheduler_drops_new_requests_when_pending_window_is_full() {
    let scheduler = MediaPreviewScheduler::with_max_pending(2);
    let generation = scheduler.begin_generation();
    let first = test_media_key(1);
    let second = test_media_key(2);
    let third = test_media_key(3);

    assert_eq!(
        test_scheduler_request(
            &scheduler,
            first,
            generation,
            MediaPreviewRequestPriority::Current
        ),
        scheduled_request()
    );
    assert_eq!(
        test_scheduler_request(
            &scheduler,
            second,
            generation,
            MediaPreviewRequestPriority::Current
        ),
        scheduled_request()
    );
    assert_eq!(
        test_scheduler_request(
            &scheduler,
            third,
            generation,
            MediaPreviewRequestPriority::Current
        ),
        MediaPreviewRequestStatus::DroppedBackpressure
    );

    assert_eq!(scheduler.pending_len(), 2);
}

#[test]
fn media_preview_scheduler_rejects_obsolete_generation_requests() {
    let scheduler = MediaPreviewScheduler::default();
    let first_generation = scheduler.begin_generation();
    scheduler.begin_generation();

    assert_eq!(
        test_scheduler_request(
            &scheduler,
            test_media_key(1),
            first_generation,
            MediaPreviewRequestPriority::Current,
        ),
        MediaPreviewRequestStatus::DroppedBackpressure
    );
    assert_eq!(scheduler.pending_len(), 0);
    let diagnostics = scheduler.diagnostics();
    assert_eq!(diagnostics.dropped_backpressure_requests, 1);
    assert_eq!(diagnostics.dropped_obsolete_generation_requests, 1);
    assert_eq!(diagnostics.dropped_pending_window_requests, 0);
}

#[test]
fn media_preview_scheduler_reports_request_and_drop_diagnostics() {
    let scheduler = MediaPreviewScheduler::with_max_pending(1);
    let generation = scheduler.begin_generation();
    let first = test_media_key(1);
    let second = test_media_key(2);

    assert_eq!(
        test_scheduler_request(
            &scheduler,
            first.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
        ),
        scheduled_request()
    );
    assert_eq!(
        test_scheduler_request(
            &scheduler,
            first.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
        ),
        MediaPreviewRequestStatus::AlreadyPending { access_mode_changed: false }
    );
    assert_eq!(
        test_scheduler_request(
            &scheduler,
            second.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
        ),
        MediaPreviewRequestStatus::DroppedBackpressure
    );

    scheduler.begin_generation();
    scheduler.prune_obsolete();
    assert!(!test_scheduler_should_decode(
        &scheduler,
        &first,
        MediaPreviewRequestPriority::Current
    ));
    assert!(!test_scheduler_complete(
        &scheduler,
        &second,
        generation,
        MediaPreviewRequestPriority::Current
    ));

    let diagnostics = scheduler.diagnostics();
    assert_eq!(diagnostics.latest_generation, generation + 1);
    assert_eq!(diagnostics.pending_requests, 0);
    assert_eq!(diagnostics.scheduled_requests, 1);
    assert_eq!(diagnostics.already_pending_requests, 1);
    assert_eq!(diagnostics.already_pending_access_mode_changes, 0);
    assert_eq!(diagnostics.dropped_backpressure_requests, 1);
    assert_eq!(diagnostics.dropped_obsolete_generation_requests, 0);
    assert_eq!(diagnostics.dropped_pending_window_requests, 1);
    assert_eq!(diagnostics.pruned_obsolete_requests, 1);
    assert_eq!(diagnostics.skipped_decode_jobs, 1);
    assert_eq!(diagnostics.skipped_decode_missing_pending, 1);
    assert_eq!(diagnostics.skipped_decode_access_mode_mismatch, 0);
    assert_eq!(diagnostics.skipped_decode_obsolete_generation, 0);
    assert_eq!(diagnostics.completed_current_results, 0);
    assert_eq!(diagnostics.completed_stale_results, 1);
    assert_eq!(diagnostics.completed_stale_missing_pending, 1);
    assert_eq!(diagnostics.completed_stale_access_mode_mismatch, 0);
    assert_eq!(diagnostics.completed_stale_obsolete_generation, 0);
}

#[test]
fn media_preview_scheduler_reports_obsolete_completion_reason() {
    let scheduler = MediaPreviewScheduler::default();
    let generation = scheduler.begin_generation();
    let key = test_media_key(1);
    assert_eq!(
        test_scheduler_request(
            &scheduler,
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
        ),
        scheduled_request()
    );

    scheduler.begin_generation();

    assert!(!test_scheduler_complete(
        &scheduler,
        &key,
        generation,
        MediaPreviewRequestPriority::Current
    ));
    let diagnostics = scheduler.diagnostics();
    assert_eq!(diagnostics.completed_stale_results, 1);
    assert_eq!(diagnostics.completed_stale_obsolete_generation, 1);
    assert_eq!(diagnostics.completed_stale_missing_pending, 0);
    assert_eq!(diagnostics.completed_stale_access_mode_mismatch, 0);
}

#[test]
fn media_preview_scheduler_cancel_all_obsoletes_in_flight_decode() {
    let scheduler = MediaPreviewScheduler::with_max_pending(4);
    let key = test_media_key(1);
    let generation = scheduler.begin_generation();

    assert_eq!(
        test_scheduler_request(
            &scheduler,
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current
        ),
        scheduled_request()
    );
    assert!(test_scheduler_is_decode_current(
        &scheduler,
        &key,
        generation,
        MediaPreviewRequestPriority::Current
    ));

    let (canceled_generation, _) = scheduler.cancel_all();

    assert!(!test_scheduler_is_decode_current(
        &scheduler,
        &key,
        generation,
        MediaPreviewRequestPriority::Current
    ));
    assert_eq!(
        canceled_generation,
        scheduler.diagnostics().latest_generation
    );
    assert!(canceled_generation > generation);
    assert_eq!(scheduler.pending_len(), 0);
    assert_eq!(scheduler.diagnostics().canceled_requests, 1);
}

#[test]
fn media_preview_scheduler_canceled_same_generation_completion_is_cache_only() {
    let scheduler = MediaPreviewScheduler::with_max_pending(4);
    let key = test_media_key(1);
    let generation = scheduler.begin_generation();

    assert_eq!(
        test_scheduler_request(
            &scheduler,
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Prefetch,
        ),
        scheduled_request()
    );

    scheduler.cancel(&key);

    assert_eq!(
        test_scheduler_completion(
            &scheduler,
            &key,
            generation,
            MediaPreviewRequestPriority::Prefetch
        ),
        MediaPreviewCompletionStatus::CacheOnly
    );
    let diagnostics = scheduler.diagnostics();
    assert_eq!(diagnostics.completed_current_results, 0);
    assert_eq!(diagnostics.completed_cache_only_results, 1);
    assert_eq!(diagnostics.completed_cache_only_missing_pending, 1);
    assert_eq!(diagnostics.completed_stale_results, 0);
}

#[test]
fn media_preview_scheduler_access_mode_mismatch_is_cache_only_when_latest() {
    let scheduler = MediaPreviewScheduler::with_max_pending(4);
    let key = test_media_key(1);
    let generation = scheduler.begin_generation();

    assert_eq!(
        test_scheduler_request(
            &scheduler,
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Prefetch,
        ),
        scheduled_request()
    );
    assert_eq!(
        test_scheduler_request(
            &scheduler,
            key.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
        ),
        MediaPreviewRequestStatus::AlreadyPending { access_mode_changed: true }
    );

    assert_eq!(
        test_scheduler_completion(
            &scheduler,
            &key,
            generation,
            MediaPreviewRequestPriority::Prefetch,
        ),
        MediaPreviewCompletionStatus::CacheOnly
    );

    let diagnostics = scheduler.diagnostics();
    assert_eq!(diagnostics.completed_current_results, 0);
    assert_eq!(diagnostics.completed_cache_only_results, 1);
    assert_eq!(diagnostics.completed_cache_only_access_mode_mismatch, 1);
    assert_eq!(diagnostics.completed_stale_access_mode_mismatch, 0);
    assert_eq!(scheduler.pending_len(), 1);
}

#[test]
fn media_preview_scheduler_current_request_evicts_prefetch_when_window_is_full() {
    let scheduler = MediaPreviewScheduler::with_max_pending(1);
    let generation = scheduler.begin_generation();
    let prefetch = test_media_key(1);
    let current = test_media_key(2);

    assert_eq!(
        test_scheduler_request(
            &scheduler,
            prefetch.clone(),
            generation,
            MediaPreviewRequestPriority::Prefetch,
        ),
        scheduled_request()
    );
    assert_eq!(
        test_scheduler_request(
            &scheduler,
            current.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
        ),
        MediaPreviewRequestStatus::Scheduled {
            evicted_prefetch: Some(Box::new(prefetch.clone())),
            evicted_still: None,
        }
    );

    assert_eq!(scheduler.pending_len(), 1);
    assert!(!test_scheduler_should_decode(
        &scheduler,
        &prefetch,
        MediaPreviewRequestPriority::Prefetch
    ));
    assert!(test_scheduler_should_decode(
        &scheduler,
        &current,
        MediaPreviewRequestPriority::Current
    ));
    let diagnostics = scheduler.diagnostics();
    assert_eq!(diagnostics.evicted_prefetch_requests, 1);
    assert_eq!(diagnostics.dropped_backpressure_requests, 0);
}

#[test]
fn media_preview_scheduler_prefetch_does_not_evict_current_request() {
    let scheduler = MediaPreviewScheduler::with_max_pending(1);
    let generation = scheduler.begin_generation();
    let current = test_media_key(1);
    let prefetch = test_media_key(2);

    assert_eq!(
        test_scheduler_request(
            &scheduler,
            current.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
        ),
        scheduled_request()
    );
    assert_eq!(
        test_scheduler_request(
            &scheduler,
            prefetch,
            generation,
            MediaPreviewRequestPriority::Prefetch
        ),
        MediaPreviewRequestStatus::DroppedBackpressure
    );

    assert_eq!(scheduler.pending_len(), 1);
    assert!(test_scheduler_should_decode(
        &scheduler,
        &current,
        MediaPreviewRequestPriority::Current
    ));
    let diagnostics = scheduler.diagnostics();
    assert_eq!(diagnostics.evicted_prefetch_requests, 0);
    assert_eq!(diagnostics.dropped_backpressure_requests, 1);
}

#[test]
fn media_preview_scheduler_realtime_current_evicts_pending_still_when_full() {
    let scheduler = MediaPreviewScheduler::with_max_pending(1);
    let generation = scheduler.begin_generation();
    let still = test_media_key(1);
    let scrub = test_media_key(2);

    assert_eq!(
        scheduler.request(
            still.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ),
        scheduled_request()
    );
    assert_eq!(
        scheduler.request(
            scrub.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
        ),
        MediaPreviewRequestStatus::Scheduled {
            evicted_prefetch: None,
            evicted_still: Some(Box::new(still.clone())),
        }
    );

    assert_eq!(scheduler.pending_len(), 1);
    assert!(!scheduler.should_decode(&still, PreviewDecodeAccessMode::RandomAccessStillFrame));
    assert!(scheduler.should_decode(&scrub, PreviewDecodeAccessMode::ScrubCursor));
    let diagnostics = scheduler.diagnostics();
    assert_eq!(diagnostics.evicted_still_requests, 1);
    assert_eq!(diagnostics.dropped_backpressure_requests, 0);
}

#[test]
fn media_preview_scheduler_realtime_current_preempts_pending_still_before_window_is_full() {
    let scheduler = MediaPreviewScheduler::with_max_pending(4);
    let generation = scheduler.begin_generation();
    let still = test_media_key(1);
    let scrub = test_media_key(2);

    assert_eq!(
        scheduler.request(
            still.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ),
        scheduled_request()
    );
    assert_eq!(
        scheduler.request(
            scrub.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
        ),
        MediaPreviewRequestStatus::Scheduled {
            evicted_prefetch: None,
            evicted_still: Some(Box::new(still.clone())),
        }
    );

    assert!(scheduler.pending_still_for_realtime_current(&scrub).is_empty());
    assert!(!scheduler.cancel_preempted_still_for_realtime_current(&still));
    assert_eq!(scheduler.pending_len(), 1);
    assert!(!scheduler.should_decode(&still, PreviewDecodeAccessMode::RandomAccessStillFrame));
    assert!(scheduler.should_decode(&scrub, PreviewDecodeAccessMode::ScrubCursor));
    let diagnostics = scheduler.diagnostics();
    assert_eq!(diagnostics.evicted_still_requests, 1);
    assert_eq!(diagnostics.dropped_backpressure_requests, 0);
}

#[test]
fn media_preview_scheduler_still_does_not_evict_realtime_current_when_full() {
    let scheduler = MediaPreviewScheduler::with_max_pending(1);
    let generation = scheduler.begin_generation();
    let scrub = test_media_key(1);
    let still = test_media_key(2);

    assert_eq!(
        scheduler.request(
            scrub.clone(),
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
        ),
        scheduled_request()
    );
    assert_eq!(
        scheduler.request(
            still,
            generation,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::RandomAccessStillFrame,
        ),
        MediaPreviewRequestStatus::DroppedBackpressure
    );

    assert_eq!(scheduler.pending_len(), 1);
    assert!(scheduler.should_decode(&scrub, PreviewDecodeAccessMode::ScrubCursor));
    let diagnostics = scheduler.diagnostics();
    assert_eq!(diagnostics.evicted_still_requests, 0);
    assert_eq!(diagnostics.dropped_pending_window_requests, 1);
}

#[test]
fn media_preview_job_queue_current_request_evicts_prefetch_when_full() {
    let (sender, receiver) = media_preview_job_queue(1);
    let prefetch = test_media_key(1);
    let current = test_media_key(2);
    let prefetch_job = test_media_job(prefetch.clone(), MediaPreviewRequestPriority::Prefetch);
    let current_job = test_media_job(current.clone(), MediaPreviewRequestPriority::Current);

    assert_eq!(
        sender.enqueue(prefetch_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(current_job),
        MediaPreviewJobEnqueueStatus::Enqueued {
            evicted_prefetch: Some(Box::new(prefetch)),
            evicted_still: None,
        }
    );

    let next = receiver.recv().expect("queued current job");
    assert_eq!(next.key, current);
}

#[test]
fn media_preview_job_queue_realtime_current_evicts_still_when_full() {
    let (sender, receiver) = media_preview_job_queue(1);
    let still = test_media_key(1);
    let scrub = test_media_key(2);
    let mut still_job = test_media_job(still.clone(), MediaPreviewRequestPriority::Current);
    still_job.access_mode = PreviewDecodeAccessMode::RandomAccessStillFrame;
    let mut scrub_job = test_media_job(scrub.clone(), MediaPreviewRequestPriority::Current);
    scrub_job.access_mode = PreviewDecodeAccessMode::ScrubCursor;

    assert_eq!(
        sender.enqueue(still_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(scrub_job),
        MediaPreviewJobEnqueueStatus::Enqueued {
            evicted_prefetch: None,
            evicted_still: Some(Box::new(still)),
        }
    );

    let next = receiver.recv().expect("queued scrub job");
    assert_eq!(next.key, scrub);
    assert_eq!(next.access_mode, PreviewDecodeAccessMode::ScrubCursor);
}

#[test]
fn media_preview_job_queue_prioritizes_scrub_before_still_on_shared_lane() {
    let (sender, receiver) = media_preview_job_queue(2);
    let still = test_media_key(1);
    let scrub = test_media_key(2);
    let mut still_job = test_media_job(still.clone(), MediaPreviewRequestPriority::Current);
    still_job.access_mode = PreviewDecodeAccessMode::RandomAccessStillFrame;
    let mut scrub_job = test_media_job(scrub.clone(), MediaPreviewRequestPriority::Current);
    scrub_job.access_mode = PreviewDecodeAccessMode::ScrubCursor;

    assert_eq!(
        sender.enqueue(scrub_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(still_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    let scrub_job = receiver
        .recv_for_worker(MediaPreviewWorkerLane::NonPlayback)
        .expect("interactive lane should prefer scrub over still");
    assert_eq!(scrub_job.key, scrub);
    assert_eq!(scrub_job.access_mode, PreviewDecodeAccessMode::ScrubCursor);
    assert_eq!(receiver.recv().expect("still job").key, still);
}

#[test]
fn media_preview_job_queue_rejects_non_playback_prefetch_jobs() {
    let (sender, receiver) = media_preview_job_queue(2);
    let key = test_media_key(1);
    let mut job = test_media_job(key, MediaPreviewRequestPriority::Prefetch);
    job.access_mode = PreviewDecodeAccessMode::RandomAccessStillFrame;

    assert_eq!(
        sender.enqueue(job),
        MediaPreviewJobEnqueueStatus::DroppedInvalidAccessMode
    );

    sender.close();
    assert!(receiver.recv().is_none());
}

#[test]
fn media_preview_job_queue_pops_current_before_prefetch() {
    let (sender, receiver) = media_preview_job_queue(3);
    let first_prefetch = test_media_key(1);
    let current = test_media_key(2);
    let second_prefetch = test_media_key(3);

    assert_eq!(
        sender.enqueue(test_media_job(
            first_prefetch.clone(),
            MediaPreviewRequestPriority::Prefetch,
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(test_media_job(
            current.clone(),
            MediaPreviewRequestPriority::Current
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(test_media_job(
            second_prefetch.clone(),
            MediaPreviewRequestPriority::Prefetch,
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    assert_eq!(receiver.recv().expect("current").key, current);
    assert_eq!(receiver.recv().expect("first prefetch").key, first_prefetch);
    assert_eq!(
        receiver.recv().expect("second prefetch").key,
        second_prefetch
    );
}

#[test]
fn media_preview_job_queue_keeps_playback_cursor_on_playback_worker() {
    let (sender, receiver) = media_preview_job_queue(2);
    let playback = test_media_key(1);
    let still = test_media_key(2);
    let mut playback_job = test_media_job(playback.clone(), MediaPreviewRequestPriority::Current);
    playback_job.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
    let mut still_job = test_media_job(still.clone(), MediaPreviewRequestPriority::Current);
    still_job.access_mode = PreviewDecodeAccessMode::RandomAccessStillFrame;

    assert_eq!(
        sender.enqueue(playback_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(still_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    let interactive_job = receiver
        .recv_for_worker(MediaPreviewWorkerLane::NonPlayback)
        .expect("non-playback worker should skip playback cursor work");
    assert_eq!(interactive_job.key, still);

    let playback_job = receiver
        .recv_for_worker(MediaPreviewWorkerLane::Playback)
        .expect("playback worker should retain playback cursor work");
    assert_eq!(playback_job.key, playback);

    let diagnostics = sender.diagnostics();
    assert_eq!(diagnostics.in_flight_jobs, 2);
    assert_eq!(diagnostics.in_flight_playback_cursor_jobs, 1);
    assert_eq!(diagnostics.in_flight_random_access_still_jobs, 1);
    assert_eq!(diagnostics.in_flight_playback_lane_jobs, 1);
    assert_eq!(diagnostics.in_flight_non_playback_lane_jobs, 1);
    assert_eq!(diagnostics.in_flight_cross_lane_current_jobs, 0);
}

#[test]
fn media_preview_job_queue_current_scrub_keeps_interactive_session_affinity() {
    let (sender, receiver) = media_preview_job_queue(2);
    let scrub = test_media_key(1);
    let playback_prefetch = test_media_key(2);
    let scrub_job = test_media_job(scrub.clone(), MediaPreviewRequestPriority::Current);
    let mut playback_job = test_media_job(
        playback_prefetch.clone(),
        MediaPreviewRequestPriority::Prefetch,
    );
    playback_job.access_mode = PreviewDecodeAccessMode::PlaybackCursor;

    assert_eq!(
        sender.enqueue(scrub_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(playback_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    let playback_job = receiver
        .recv_for_worker(MediaPreviewWorkerLane::Playback)
        .expect("playback lane should preserve interactive decoder affinity");
    assert_eq!(playback_job.key, playback_prefetch);
    assert_eq!(
        playback_job.access_mode,
        PreviewDecodeAccessMode::PlaybackCursor
    );

    let scrub_job = receiver
        .recv_for_worker(MediaPreviewWorkerLane::NonPlayback)
        .expect("interactive lane should own current scrub work");
    assert_eq!(scrub_job.key, scrub);
    assert_eq!(scrub_job.access_mode, PreviewDecodeAccessMode::ScrubCursor);
}

#[test]
fn media_preview_job_queue_drops_expired_playback_current_at_dequeue() {
    let (sender, receiver) = media_preview_job_queue(2);
    let expired_playback = test_media_key(1);
    let fresh_scrub = test_media_key(2);
    let mut expired_playback_job = test_media_job(
        expired_playback.clone(),
        MediaPreviewRequestPriority::Current,
    );
    expired_playback_job.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
    expired_playback_job.deadline_at = Some(Instant::now() - std::time::Duration::from_millis(1));
    let mut fresh_scrub_job =
        test_media_job(fresh_scrub.clone(), MediaPreviewRequestPriority::Current);
    fresh_scrub_job.access_mode = PreviewDecodeAccessMode::ScrubCursor;

    assert_eq!(
        sender.enqueue(expired_playback_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(fresh_scrub_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    match receiver
        .recv_for_worker_outcome(MediaPreviewWorkerLane::Playback)
        .expect("expired playback job should produce a structured queue outcome")
    {
        MediaPreviewJobQueueReceive::DroppedExpired(expired_job) => {
            assert_eq!(expired_job.key, expired_playback);
            assert_eq!(
                expired_job.access_mode,
                PreviewDecodeAccessMode::PlaybackCursor
            );
        }
        MediaPreviewJobQueueReceive::Job(job) => {
            panic!("expired playback job must not be dispatched for decode: {job:?}");
        }
    }
    let scrub_job = receiver
        .recv_for_worker(MediaPreviewWorkerLane::NonPlayback)
        .expect("fresh current scrub should retain interactive lane affinity");
    assert_eq!(scrub_job.key, fresh_scrub);
    assert_eq!(scrub_job.access_mode, PreviewDecodeAccessMode::ScrubCursor);
    let diagnostics = sender.diagnostics();
    assert_eq!(diagnostics.queued_expired_playback_current_jobs, 0);
    assert_eq!(diagnostics.queued_expired_jobs, 0);
    assert_eq!(diagnostics.dropped_expired_playback_current_jobs, 1);
    assert_eq!(diagnostics.dropped_expired_jobs, 1);
}

#[test]
fn media_preview_job_queue_diagnostics_counts_expired_playback_current_jobs() {
    let (sender, _receiver) = media_preview_job_queue(4);
    let expired = test_media_key(1);
    let fresh = test_media_key(2);
    let scrub = test_media_key(3);
    let mut expired_job = test_media_job(expired, MediaPreviewRequestPriority::Current);
    expired_job.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
    expired_job.deadline_at = Some(Instant::now() - std::time::Duration::from_millis(1));
    let mut fresh_job = test_media_job(fresh, MediaPreviewRequestPriority::Current);
    fresh_job.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
    fresh_job.deadline_at = Some(Instant::now() + std::time::Duration::from_secs(1));
    let mut scrub_job = test_media_job(scrub, MediaPreviewRequestPriority::Current);
    scrub_job.access_mode = PreviewDecodeAccessMode::ScrubCursor;

    assert_eq!(
        sender.enqueue(expired_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(fresh_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(scrub_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    let diagnostics = sender.diagnostics();

    assert_eq!(diagnostics.queued_current_jobs, 3);
    assert_eq!(diagnostics.queued_playback_cursor_jobs, 2);
    assert_eq!(diagnostics.queued_expired_playback_current_jobs, 1);
    assert_eq!(diagnostics.queued_expired_jobs, 1);
}

#[test]
fn media_preview_job_queue_routes_scrub_and_still_through_shared_non_playback_lane() {
    let (sender, receiver) = media_preview_job_queue(2);
    let still = test_media_key(1);
    let scrub = test_media_key(2);
    let mut still_job = test_media_job(still.clone(), MediaPreviewRequestPriority::Current);
    still_job.access_mode = PreviewDecodeAccessMode::RandomAccessStillFrame;
    let mut scrub_job = test_media_job(scrub.clone(), MediaPreviewRequestPriority::Current);
    scrub_job.access_mode = PreviewDecodeAccessMode::ScrubCursor;

    assert_eq!(
        sender.enqueue(scrub_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(still_job),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    let scrub_job = receiver
        .recv_for_worker(MediaPreviewWorkerLane::NonPlayback)
        .expect("non-playback lane should accept scrub work");
    assert_eq!(scrub_job.key, scrub);
    assert_eq!(scrub_job.access_mode, PreviewDecodeAccessMode::ScrubCursor);

    let still_job = receiver
        .recv_for_worker(MediaPreviewWorkerLane::NonPlayback)
        .expect("non-playback lane should also accept still work");
    assert_eq!(still_job.key, still);
    assert_eq!(
        still_job.access_mode,
        PreviewDecodeAccessMode::RandomAccessStillFrame
    );

    let diagnostics = sender.diagnostics();
    assert_eq!(diagnostics.in_flight_jobs, 2);
    assert_eq!(diagnostics.in_flight_scrub_cursor_jobs, 1);
    assert_eq!(diagnostics.in_flight_random_access_still_jobs, 1);
    assert_eq!(diagnostics.in_flight_scrub_lane_jobs, 0);
    assert_eq!(diagnostics.in_flight_still_lane_jobs, 0);
    assert_eq!(diagnostics.in_flight_non_playback_lane_jobs, 2);
    assert_eq!(diagnostics.in_flight_cross_lane_current_jobs, 0);
}

#[test]
fn media_preview_job_queue_promotes_existing_prefetch_to_current() {
    let (sender, receiver) = media_preview_job_queue(2);
    let promoted = test_media_key(1);
    let other_prefetch = test_media_key(2);

    assert_eq!(
        sender.enqueue(test_media_job(
            promoted.clone(),
            MediaPreviewRequestPriority::Prefetch
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(test_media_job(
            other_prefetch.clone(),
            MediaPreviewRequestPriority::Prefetch,
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    let promoted_at = Instant::now();
    let mut playback_engine = mondrian_playback::PlaybackEngine::new(
        mondrian_core::Rational::new(1, 25),
        mondrian_playback::PlaybackPolicy::default(),
    )
    .expect("playback engine");
    playback_engine
        .play_timeline(
            mondrian_playback::PlaybackTimelineBinding::new(
                None,
                0,
                mondrian_core::Rational::new(1, 25),
                10,
            )
            .expect("timeline binding"),
            mondrian_core::FramePosition::new(0, mondrian_core::Rational::new(1, 25)),
            mondrian_playback::MonotonicTimestamp::ZERO,
        )
        .expect("playback demand");
    let demand_identity = playback_engine.frame_demand().expect("frame demand").identity();
    let status = sender.promote(
        &promoted,
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::ScrubCursor,
        7,
        promoted_at,
        None,
        Some(demand_identity),
        PreviewDecodeAdaptiveHints::default(),
        PreviewHardwareDecodeRequest::PreferGpuResident,
        None,
    );
    assert_eq!(
        status,
        MediaPreviewJobPromoteStatus {
            updated: true,
            priority_promoted: true,
            access_mode_changed: true,
            generation_changed: true,
        }
    );
    let promoted_job = receiver.recv().expect("promoted current");
    assert_eq!(promoted_job.key, promoted);
    assert_eq!(promoted_job.priority, MediaPreviewRequestPriority::Current);
    assert_eq!(promoted_job.generation, 7);
    assert_eq!(promoted_job.key.source_time, promoted.source_time);
    assert_eq!(promoted_job.enqueued_at, promoted_at);
    assert_eq!(promoted_job.deadline_at, None);
    assert_eq!(promoted_job.demand_identity, Some(demand_identity));
    assert_eq!(
        promoted_job.access_mode,
        PreviewDecodeAccessMode::ScrubCursor
    );
    assert_eq!(
        promoted_job.hardware_decode_request,
        PreviewHardwareDecodeRequest::PreferGpuResident
    );
    assert_eq!(
        receiver.recv().expect("remaining prefetch").key,
        other_prefetch
    );
}

#[test]
fn media_preview_job_queue_promote_refreshes_current_generation_without_priority_metric() {
    let (sender, receiver) = media_preview_job_queue(1);
    let key = test_media_key(1);

    assert_eq!(
        sender.enqueue(test_media_job_with_generation(
            key.clone(),
            2,
            MediaPreviewRequestPriority::Current,
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    let refreshed_at = Instant::now();
    let refreshed_deadline = Some(refreshed_at + Duration::from_secs(1));
    let status = sender.promote(
        &key,
        MediaPreviewRequestPriority::Current,
        PreviewDecodeAccessMode::ScrubCursor,
        5,
        refreshed_at,
        refreshed_deadline,
        None,
        PreviewDecodeAdaptiveHints::default(),
        PreviewHardwareDecodeRequest::Auto,
        None,
    );

    assert_eq!(
        status,
        MediaPreviewJobPromoteStatus {
            updated: true,
            priority_promoted: false,
            access_mode_changed: false,
            generation_changed: true,
        }
    );
    let job = receiver.recv().expect("refreshed current job");
    assert_eq!(job.key, key);
    assert_eq!(job.priority, MediaPreviewRequestPriority::Current);
    assert_eq!(job.access_mode, PreviewDecodeAccessMode::ScrubCursor);
    assert_eq!(job.generation, 5);
    assert_eq!(job.enqueued_at, refreshed_at);
    assert_eq!(job.deadline_at, refreshed_deadline);
}

#[test]
fn media_preview_job_queue_prunes_obsolete_jobs_before_current_work() {
    let (sender, receiver) = media_preview_job_queue(3);
    let old_current = test_media_key(1);
    let old_prefetch = test_media_key(2);
    let fresh_prefetch = test_media_key(3);
    let current = test_media_key(4);

    assert_eq!(
        sender.enqueue(test_media_job_with_generation(
            old_current.clone(),
            1,
            MediaPreviewRequestPriority::Current,
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(test_media_job_with_generation(
            old_prefetch.clone(),
            1,
            MediaPreviewRequestPriority::Prefetch,
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(test_media_job_with_generation(
            fresh_prefetch.clone(),
            3,
            MediaPreviewRequestPriority::Prefetch,
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    assert_eq!(sender.prune_obsolete_jobs(3), 2);
    assert_eq!(
        sender.enqueue(test_media_job_with_generation(
            current.clone(),
            3,
            MediaPreviewRequestPriority::Current,
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    assert_eq!(receiver.recv().expect("current").key, current);
    assert_eq!(receiver.recv().expect("fresh prefetch").key, fresh_prefetch);
}

#[test]
fn media_preview_job_queue_cancels_all_queued_jobs_for_key() {
    let (sender, receiver) = media_preview_job_queue(4);
    let canceled = test_media_key(1);
    let retained = test_media_key(2);

    assert_eq!(
        sender.enqueue(test_media_job(
            canceled.clone(),
            MediaPreviewRequestPriority::Prefetch
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(test_media_job(
            retained.clone(),
            MediaPreviewRequestPriority::Prefetch
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(test_media_job(
            canceled.clone(),
            MediaPreviewRequestPriority::Current
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    assert_eq!(
        sender.cancel_key(&canceled),
        1,
        "one semantic key owns at most one queued payload after atomic promotion"
    );
    assert_eq!(receiver.recv().expect("retained job").key, retained);
    sender.close();
    assert!(receiver.recv().is_none());
}

#[test]
fn media_preview_job_queue_clear_removes_all_queued_jobs() {
    let (sender, receiver) = media_preview_job_queue(4);
    let first = test_media_key(1);
    let second = test_media_key(2);

    assert_eq!(
        sender.enqueue(test_media_job(first, MediaPreviewRequestPriority::Current,)),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(test_media_job(
            second,
            MediaPreviewRequestPriority::Prefetch,
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    assert_eq!(sender.clear(), 2);
    assert_eq!(sender.diagnostics().queued_jobs, 0);
    sender.close();
    assert!(receiver.recv().is_none());
}

#[test]
fn media_preview_job_queue_diagnostics_break_down_current_depth_by_access_mode() {
    let (sender, _receiver) = media_preview_job_queue(4);
    let playback = test_media_key(1);
    let scrub = test_media_key(2);
    let still = test_media_key(3);

    assert_eq!(
        sender.enqueue(test_media_job(
            playback,
            MediaPreviewRequestPriority::Prefetch
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(test_media_job(scrub, MediaPreviewRequestPriority::Current)),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(MediaPreviewJob {
            key: still,
            generation: 1,
            priority: MediaPreviewRequestPriority::Current,
            access_mode: PreviewDecodeAccessMode::RandomAccessStillFrame,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            enqueued_at: Instant::now(),
            deadline_at: None,
            demand_identity: None,
            execution_id: None,
        }),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    assert_eq!(
        sender.diagnostics(),
        MediaPreviewJobQueueDiagnostics {
            queued_jobs: 3,
            in_flight_jobs: 0,
            in_flight_completed_jobs: 0,
            in_flight_cancellation_requested_jobs: 0,
            in_flight_max_age_us: 0,
            in_flight_cancellation_max_age_us: 0,
            queued_current_jobs: 2,
            in_flight_current_jobs: 0,
            queued_prefetch_jobs: 1,
            in_flight_prefetch_jobs: 0,
            queued_playback_cursor_jobs: 1,
            in_flight_playback_cursor_jobs: 0,
            queued_expired_playback_current_jobs: 0,
            queued_expired_jobs: 0,
            dropped_expired_playback_current_jobs: 0,
            dropped_expired_jobs: 0,
            queued_scrub_cursor_jobs: 1,
            in_flight_scrub_cursor_jobs: 0,
            queued_random_access_still_jobs: 1,
            in_flight_random_access_still_jobs: 0,
            in_flight_any_lane_jobs: 0,
            in_flight_playback_lane_jobs: 0,
            in_flight_scrub_lane_jobs: 0,
            in_flight_still_lane_jobs: 0,
            in_flight_non_playback_lane_jobs: 0,
            in_flight_cross_lane_current_jobs: 0,
            queued_any_lane_eligible_jobs: 3,
            queued_playback_lane_eligible_jobs: 1,
            queued_scrub_lane_eligible_jobs: 1,
            queued_still_lane_eligible_jobs: 1,
            queued_non_playback_lane_eligible_jobs: 2,
            closed: false,
        }
    );

    sender.close();
    assert_eq!(
        sender.diagnostics(),
        MediaPreviewJobQueueDiagnostics {
            closed: true,
            ..MediaPreviewJobQueueDiagnostics::default()
        }
    );
}

#[test]
fn media_preview_job_queue_promote_returns_false_for_missing_or_prefetch() {
    let (sender, _receiver) = media_preview_job_queue(1);
    let key = test_media_key(1);

    assert_eq!(
        sender.promote(
            &key,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
            1,
            Instant::now(),
            None,
            None,
            PreviewDecodeAdaptiveHints::default(),
            PreviewHardwareDecodeRequest::Auto,
            None,
        ),
        MediaPreviewJobPromoteStatus::default()
    );
    assert_eq!(
        sender.promote(
            &key,
            MediaPreviewRequestPriority::Prefetch,
            PreviewDecodeAccessMode::PlaybackCursor,
            1,
            Instant::now(),
            None,
            None,
            PreviewDecodeAdaptiveHints::default(),
            PreviewHardwareDecodeRequest::Auto,
            None,
        ),
        MediaPreviewJobPromoteStatus::default()
    );
}

#[test]
fn media_preview_job_queue_clear_and_close_release_workers() {
    let (sender, receiver) = media_preview_job_queue(2);
    let first = test_media_key(1);
    let second = test_media_key(2);

    assert_eq!(
        sender.enqueue(test_media_job(first, MediaPreviewRequestPriority::Prefetch)),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );
    assert_eq!(
        sender.enqueue(test_media_job(
            second,
            MediaPreviewRequestPriority::Prefetch
        )),
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
    );

    assert_eq!(sender.clear(), 2);
    sender.close();

    assert!(receiver.recv().is_none());
    assert_eq!(
        sender.enqueue(test_media_job(
            test_media_key(3),
            MediaPreviewRequestPriority::Current
        )),
        MediaPreviewJobEnqueueStatus::Closed
    );
}

#[test]
fn media_preview_worker_count_reserves_cpu_capacity() {
    assert_eq!(media_preview_worker_count_for(0), 1);
    assert_eq!(media_preview_worker_count_for(1), 1);
    assert_eq!(media_preview_worker_count_for(5), 1);
    assert_eq!(media_preview_worker_count_for(6), 2);
    assert_eq!(media_preview_worker_count_for(7), 2);
    assert_eq!(media_preview_worker_count_for(8), 2);
    assert_eq!(media_preview_worker_count_for(11), 2);
    assert_eq!(media_preview_worker_count_for(12), 2);
    assert_eq!(media_preview_worker_count_for(32), 2);
}

#[test]
fn media_preview_worker_lane_reserves_playback_only_when_parallel() {
    assert_eq!(media_preview_worker_lane(0, 1), MediaPreviewWorkerLane::Any);
    assert_eq!(
        media_preview_worker_lane(0, 2),
        MediaPreviewWorkerLane::Playback
    );
    assert_eq!(
        media_preview_worker_lane(1, 2),
        MediaPreviewWorkerLane::NonPlayback
    );
    assert_eq!(
        media_preview_worker_lane(0, 3),
        MediaPreviewWorkerLane::Playback
    );
    assert_eq!(
        media_preview_worker_lane(1, 3),
        MediaPreviewWorkerLane::NonPlayback
    );
    assert_eq!(
        media_preview_worker_lane(2, 3),
        MediaPreviewWorkerLane::NonPlayback
    );
}

#[test]
fn media_preview_viewer_access_intent_tracks_playback_state() {
    assert_eq!(
        media_preview_viewer_access_intent(true, TimelineSeekSource::Settled),
        MediaPreviewAccessIntent::Playback
    );
    assert_eq!(
        media_preview_viewer_access_intent(false, TimelineSeekSource::PointerDrag),
        MediaPreviewAccessIntent::InteractiveScrub
    );
    assert_eq!(
        media_preview_viewer_access_intent(false, TimelineSeekSource::Settled),
        MediaPreviewAccessIntent::DeterministicStill
    );
}

#[test]
fn media_preview_access_intent_lowers_to_explicit_access_modes() {
    assert_eq!(
        media_preview_access_mode_for_intent(MediaPreviewAccessIntent::Playback),
        PreviewDecodeAccessMode::PlaybackCursor
    );
    assert_eq!(
        media_preview_access_mode_for_intent(MediaPreviewAccessIntent::InteractiveScrub),
        PreviewDecodeAccessMode::ScrubCursor
    );
    assert_eq!(
        media_preview_access_mode_for_intent(MediaPreviewAccessIntent::DeterministicStill),
        PreviewDecodeAccessMode::RandomAccessStillFrame
    );
}
