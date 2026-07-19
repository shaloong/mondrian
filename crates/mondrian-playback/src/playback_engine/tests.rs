use super::*;

#[test]
fn preview_resolution_scale_exposes_exact_adapter_divisors() {
    assert_eq!(PreviewResolutionScale::Full.dimension_divisor(), 1);
    assert_eq!(PreviewResolutionScale::Half.dimension_divisor(), 2);
    assert_eq!(PreviewResolutionScale::Quarter.dimension_divisor(), 4);
}

#[test]
fn frame_delivery_round_trips_opaque_demand_identity() {
    let identity = FrameDemandIdentity {
        epoch: PlaybackEpoch(7),
        quality_revision: 3,
        sequence: FrameDemandSequence(11),
        target_frame: 42,
    };
    let delivery = FrameDelivery::for_demand(identity, FrameDeliveryKind::Ready);

    assert_eq!(delivery.identity(), identity);
}

#[test]
fn presentation_ticket_classifies_completion_at_the_final_deadline() {
    let demand = FrameDemand {
        epoch: PlaybackEpoch(7),
        quality_revision: 3,
        sequence: FrameDemandSequence(11),
        sequence_id: None,
        timeline_revision: 9,
        target: FramePosition::new(42, Rational::new(1, 25)),
        deadline: Some(ts(40)),
        preview_scale: PreviewResolutionScale::Full,
    };
    let ready = FramePresentationTicket::for_demand(demand, FramePresentationQuality::Ready);
    let degraded = FramePresentationTicket::for_demand(demand, FramePresentationQuality::Degraded);

    assert_eq!(ready.complete_at(ts(39)).kind, FrameDeliveryKind::Ready);
    assert_eq!(
        degraded.complete_at(ts(39)).kind,
        FrameDeliveryKind::Degraded
    );
    assert_eq!(ready.complete_at(ts(40)).kind, FrameDeliveryKind::Late);
    assert_eq!(degraded.complete_at(ts(41)).kind, FrameDeliveryKind::Late);
}

fn ts(ms: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_duration(Duration::from_millis(ms))
}

fn audio_observation(
    engine: &PlaybackEngine,
    consumed_frames: u64,
    observed_at: MonotonicTimestamp,
) -> AudioDeviceClockObservation {
    AudioDeviceClockObservation {
        epoch: engine.snapshot().epoch,
        stream_generation: 7,
        sample_rate: 48_000,
        consumed_frames,
        media_anchor: FramePosition::new(-1_000, Rational::new(1, 48_000)),
        observed_at,
        grade: AudioClockObservationGrade::CallbackConsumptionEstimate,
        estimated_latency_frames: 0,
        uncertainty_frames: 240,
        underrun_frames: 0,
        state: AudioDeviceClockState::Running,
    }
}

#[test]
fn callback_consumption_drives_audio_device_master_with_interframe_interpolation() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();

    let anchored = engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .unwrap();
    assert_eq!(anchored.clock_master, Some(ClockMaster::AudioDevice));
    engine.tick(ts(40)).unwrap();
    assert_eq!(engine.snapshot().position.frame, 1);

    let advanced = engine
        .observe_audio_device_clock(audio_observation(&engine, 2_920, ts(40)))
        .unwrap();
    assert_eq!(advanced.position.frame, 1);
    assert_eq!(advanced.clock_master, Some(ClockMaster::AudioDevice));
}

#[test]
fn audio_clock_advance_refreshes_the_published_frame_demand() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .unwrap();
    let previous_sequence = engine.frame_demand().unwrap().sequence;

    let advanced = engine
        .observe_audio_device_clock(audio_observation(&engine, 2_920, ts(40)))
        .unwrap();
    let demand = engine.frame_demand().expect("audio-clock demand");

    assert_eq!(advanced.position.frame, 1);
    assert_eq!(demand.target, advanced.position);
    assert_ne!(demand.sequence, previous_sequence);
    assert!(engine
        .observe_frame_delivery(FrameDelivery::for_demand(
            demand.identity(),
            FrameDeliveryKind::Ready,
        ))
        .expect("current audio-clock demand delivery"));
}

#[test]
fn audio_clock_natural_end_publishes_an_untimed_final_demand() {
    let mut engine = engine();
    engine.play(1, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .unwrap();

    let ended = engine
        .observe_audio_device_clock(audio_observation(&engine, 2_920, ts(40)))
        .unwrap();
    let final_demand = engine.pending_frame_demand().expect("final demand");

    assert_eq!(ended.state, TransportState::Ended);
    assert_eq!(ended.clock_master, None);
    assert_eq!(final_demand.target.frame, 1);
    assert_eq!(final_demand.deadline, None);
}

#[test]
fn audio_device_loss_hands_off_without_position_jump() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 2_920, ts(40)))
        .unwrap();

    let handoff = engine.audio_device_lost(ts(50)).unwrap();
    assert_eq!(handoff.position.frame, 1);
    assert_eq!(handoff.clock_master, Some(ClockMaster::Synthetic));

    let continued = engine.tick(ts(90)).unwrap();
    assert_eq!(continued.position.frame, 2);
}

#[test]
fn headless_seek_device_and_delayed_presentation_fault_sequence_stays_continuous() {
    let mut engine = engine();
    engine.play(200, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .unwrap();
    assert_eq!(
        engine.snapshot().clock_master,
        Some(ClockMaster::AudioDevice)
    );

    let stale_ticket = FramePresentationTicket::for_demand(
        engine.pending_frame_demand().expect("pre-seek demand"),
        FramePresentationQuality::Ready,
    );
    engine.seek(FramePosition::new(12, Rational::new(1, 25)), ts(40)).unwrap();
    let post_seek = engine.pending_frame_demand().expect("post-seek demand");
    assert_ne!(stale_ticket.identity(), post_seek.identity());
    assert!(!engine
        .observe_frame_delivery(stale_ticket.complete_at(ts(45)))
        .expect("stale completion is a non-authoritative fact"));

    engine.complete_priming(ClockMaster::Synthetic, ts(40)).unwrap();
    let mut recovered_audio = audio_observation(&engine, 0, ts(40));
    recovered_audio.stream_generation = 8;
    recovered_audio.media_anchor = FramePosition::new(12, Rational::new(1, 25));
    engine.observe_audio_device_clock(recovered_audio).unwrap();
    assert_eq!(
        engine.snapshot().clock_master,
        Some(ClockMaster::AudioDevice)
    );

    let before_loss = engine.snapshot().position;
    engine.audio_device_lost(ts(50)).unwrap();
    assert_eq!(engine.snapshot().clock_master, Some(ClockMaster::Synthetic));
    assert!(engine.snapshot().position.frame >= before_loss.frame);
    let continued = engine.tick(ts(90)).unwrap();
    assert!(continued.position.frame >= before_loss.frame);

    let delayed_ticket = FramePresentationTicket::for_demand(
        engine.pending_frame_demand().expect("synthetic-clock demand"),
        FramePresentationQuality::Ready,
    );
    let deadline = delayed_ticket.deadline().expect("playing demand deadline");
    let delivery = delayed_ticket.complete_at(deadline.saturating_add(Duration::from_millis(1)));
    assert_eq!(delivery.kind, FrameDeliveryKind::Late);
    assert!(engine
        .observe_frame_delivery(delivery)
        .expect("current delayed presentation remains an accepted terminal fact"));
    assert_eq!(engine.snapshot().clock_master, Some(ClockMaster::Synthetic));
}

#[test]
fn audio_device_loss_refreshes_demand_when_handoff_crosses_a_frame_boundary() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .unwrap();
    let previous_sequence = engine.frame_demand().unwrap().sequence;

    let handoff = engine.audio_device_lost(ts(80)).unwrap();
    let demand = engine.frame_demand().expect("handoff demand");

    assert_eq!(handoff.position.frame, 2);
    assert_eq!(demand.target, handoff.position);
    assert_ne!(demand.sequence, previous_sequence);
}

#[test]
fn transient_audio_uncertainty_preserves_subframe_phase_and_can_reacquire() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_960, ts(20)))
        .unwrap();

    let mut uncertain = audio_observation(&engine, 2_680, ts(35));
    uncertain.uncertainty_frames = 3_000;
    let fallback = engine.observe_audio_device_clock(uncertain).unwrap();
    assert_eq!(fallback.clock_master, Some(ClockMaster::Synthetic));
    assert_eq!(fallback.position.frame, 0);

    let reacquired = engine
        .observe_audio_device_clock(audio_observation(&engine, 2_920, ts(40)))
        .unwrap();
    assert_eq!(reacquired.clock_master, Some(ClockMaster::AudioDevice));
    assert_eq!(engine.tick(ts(45)).unwrap().position.frame, 1);
}

#[test]
fn transient_stale_callback_keeps_audio_master_within_grace() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_960, ts(20)))
        .unwrap();

    let mut uncertain = audio_observation(&engine, 1_960, ts(80));
    uncertain.state = AudioDeviceClockState::Uncertain;
    let held = engine.observe_audio_device_clock(uncertain).unwrap();
    assert_eq!(held.clock_master, Some(ClockMaster::AudioDevice));

    let resumed = engine
        .observe_audio_device_clock(audio_observation(&engine, 2_920, ts(100)))
        .unwrap();
    assert_eq!(resumed.clock_master, Some(ClockMaster::AudioDevice));
}

#[test]
fn prolonged_callback_uncertainty_hands_off_to_synthetic() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .unwrap();

    let mut uncertain = audio_observation(&engine, 1_000, ts(60));
    uncertain.state = AudioDeviceClockState::Uncertain;
    engine.observe_audio_device_clock(uncertain).unwrap();
    uncertain.observed_at = ts(1_061);
    let fallback = engine.observe_audio_device_clock(uncertain).unwrap();

    assert_eq!(fallback.clock_master, Some(ClockMaster::Synthetic));
}

#[test]
fn impossible_callback_counter_slope_falls_back_without_position_jump() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .unwrap();

    let fallback = engine
        .observe_audio_device_clock(audio_observation(&engine, 49_000, ts(10)))
        .unwrap();

    assert_eq!(fallback.clock_master, Some(ClockMaster::Synthetic));
    assert_eq!(fallback.position.frame, 0);
    assert_eq!(engine.tick(ts(40)).unwrap().position.frame, 1);
}

#[test]
fn decreasing_callback_position_falls_back_to_synthetic() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 2_000, ts(0)))
        .unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 3_920, ts(40)))
        .unwrap();

    let fallback = engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(50)))
        .unwrap();

    assert_eq!(fallback.position.frame, 1);
    assert_eq!(fallback.clock_master, Some(ClockMaster::Synthetic));
    assert_eq!(engine.tick(ts(90)).unwrap().position.frame, 2);
}

#[test]
fn changed_media_anchor_cannot_reuse_active_callback_consumption() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .unwrap();

    let mut reanchored = audio_observation(&engine, 2_920, ts(40));
    reanchored.media_anchor = FramePosition::new(48_000, Rational::new(1, 48_000));
    let fallback = engine.observe_audio_device_clock(reanchored).unwrap();

    assert_eq!(fallback.position.frame, 1);
    assert_eq!(fallback.clock_master, Some(ClockMaster::Synthetic));
    assert_eq!(engine.tick(ts(80)).unwrap().position.frame, 2);
}

#[test]
fn uncertain_audio_observation_cannot_claim_audio_device_master() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let mut observation = audio_observation(&engine, 1_000, ts(0));
    observation.uncertainty_frames = 3_000;

    let snapshot = engine.observe_audio_device_clock(observation).unwrap();

    assert_eq!(snapshot.clock_master, Some(ClockMaster::Synthetic));
    assert_eq!(engine.tick(ts(40)).unwrap().position.frame, 1);
}

#[test]
fn subframe_aligned_audio_handoff_uses_continuous_synthetic_phase() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    assert_eq!(engine.tick(ts(39)).unwrap().position.frame, 0);
    let mut observation = audio_observation(&engine, 1_872, ts(39));
    observation.stream_generation = 8;
    observation.media_anchor = FramePosition::new(0, Rational::new(1, 48_000));

    let accepted = engine.observe_audio_device_clock(observation).unwrap();

    assert_eq!(accepted.clock_master, Some(ClockMaster::AudioDevice));
    assert_eq!(
        accepted.audio_handoff,
        Some(AudioClockHandoffEvidence {
            stream_generation: 8,
            phase_error_ns: 0,
            status: AudioClockHandoffStatus::Accepted,
        })
    );
}

#[test]
fn out_of_phase_new_stream_is_rejected_without_reanchoring_synthetic_time() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine.tick(ts(80)).unwrap();
    let mut observation = audio_observation(&engine, 1_000, ts(80));
    observation.stream_generation = 8;
    observation.media_anchor = FramePosition::new(0, Rational::new(1, 48_000));

    let rejected = engine.observe_audio_device_clock(observation).unwrap();

    assert_eq!(rejected.position.frame, 2);
    assert_eq!(rejected.clock_master, Some(ClockMaster::Synthetic));
    assert_eq!(
        rejected.audio_handoff.map(|evidence| evidence.status),
        Some(AudioClockHandoffStatus::PhaseRejected)
    );
    assert_eq!(engine.tick(ts(120)).unwrap().position.frame, 3);
}

#[test]
fn aligned_reprimed_stream_can_take_master_after_phase_rejection() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine.tick(ts(80)).unwrap();
    let mut rejected = audio_observation(&engine, 1_000, ts(80));
    rejected.stream_generation = 8;
    rejected.media_anchor = FramePosition::new(0, Rational::new(1, 48_000));
    engine.observe_audio_device_clock(rejected).unwrap();

    let mut aligned = audio_observation(&engine, 1_000, ts(90));
    aligned.stream_generation = 9;
    aligned.media_anchor = FramePosition::new(3_320, Rational::new(1, 48_000));
    let accepted = engine.observe_audio_device_clock(aligned).unwrap();

    assert_eq!(accepted.clock_master, Some(ClockMaster::AudioDevice));
    assert_eq!(
        accepted.audio_handoff.map(|evidence| evidence.status),
        Some(AudioClockHandoffStatus::Accepted)
    );
    assert!(accepted.audio_handoff.is_some_and(|evidence| {
        evidence.phase_error_ns.unsigned_abs()
            <= PlaybackPolicy::default().max_audio_handoff_phase_error.as_nanos() as u64
    }));
}

fn engine() -> PlaybackEngine {
    PlaybackEngine::new(Rational::new(1, 25), PlaybackPolicy::default()).unwrap()
}

fn timeline_binding(end_frame: i64) -> PlaybackTimelineBinding {
    PlaybackTimelineBinding::new(None, 1, Rational::new(1, 25), end_frame).unwrap()
}

fn current_delivery(engine: &PlaybackEngine, kind: FrameDeliveryKind) -> FrameDelivery {
    let demand = engine.frame_demand().expect("active frame demand");
    FrameDelivery {
        epoch: demand.epoch,
        quality_revision: demand.quality_revision,
        demand_sequence: demand.sequence,
        target_frame: demand.target.frame,
        kind,
    }
}

fn video_preroll(
    engine: &PlaybackEngine,
    ready_media_frames: usize,
    available_media_frames: usize,
) -> VideoPrerollObservation {
    VideoPrerollObservation {
        epoch: engine.snapshot().epoch,
        ready_media_frames,
        available_media_frames,
    }
}

#[test]
fn priming_holds_then_synthetic_clock_advances_exact_frames() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    assert_eq!(engine.tick(ts(400)).unwrap().position.frame, 0);

    engine.complete_priming(ClockMaster::Synthetic, ts(400)).unwrap();
    assert_eq!(engine.tick(ts(440)).unwrap().position.frame, 1);
    assert_eq!(engine.tick(ts(800)).unwrap().position.frame, 10);
}

#[test]
fn presented_current_frame_waits_for_bounded_video_preroll() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();

    let delivery = current_delivery(&engine, FrameDeliveryKind::Ready);
    assert!(engine.observe_frame_delivery(delivery).unwrap());
    assert_eq!(engine.snapshot().state, TransportState::Priming);
    assert!(engine.frame_demand().is_some());
    assert!(engine.pending_frame_demand().is_none());

    assert!(engine.observe_video_preroll(video_preroll(&engine, 1, 1)).unwrap());
    assert_eq!(engine.snapshot().state, TransportState::Playing);
    assert_eq!(engine.snapshot().clock_master, Some(ClockMaster::Synthetic));
}

#[test]
fn video_preroll_cannot_start_before_current_frame_is_presented() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();

    assert!(!engine.observe_video_preroll(video_preroll(&engine, 1, 1)).unwrap());
    assert_eq!(engine.snapshot().state, TransportState::Priming);

    let delivery = current_delivery(&engine, FrameDeliveryKind::Ready);
    assert!(engine.observe_frame_delivery(delivery).unwrap());
    assert_eq!(engine.snapshot().state, TransportState::Playing);
}

#[test]
fn no_available_future_media_releases_presented_current_frame() {
    let mut engine = engine();
    engine.play(0, ts(0)).unwrap();
    let delivery = current_delivery(&engine, FrameDeliveryKind::Ready);
    engine.observe_frame_delivery(delivery).unwrap();

    assert!(engine.observe_video_preroll(video_preroll(&engine, 0, 0)).unwrap());
    assert_eq!(engine.snapshot().state, TransportState::Playing);
}

#[test]
fn invalid_or_stale_video_preroll_cannot_mutate_session() {
    let mut engine = engine();
    let first = engine.play(100, ts(0)).unwrap();
    engine.stop(ts(1)).unwrap();
    engine.play(100, ts(1)).unwrap();
    let before = engine.snapshot();

    assert!(!engine
        .observe_video_preroll(VideoPrerollObservation {
            epoch: first.epoch,
            ready_media_frames: 1,
            available_media_frames: 1,
        })
        .unwrap());
    assert_eq!(engine.snapshot(), before);
    assert_eq!(
        engine.observe_video_preroll(video_preroll(&engine, 2, 1)),
        Err(PlaybackError::InvalidVideoPrerollObservation)
    );
    assert_eq!(
        engine.observe_video_preroll(video_preroll(
            &engine,
            MAX_BOUNDED_VIDEO_PREROLL_FRAMES,
            MAX_BOUNDED_VIDEO_PREROLL_FRAMES + 1,
        )),
        Err(PlaybackError::InvalidVideoPrerollObservation)
    );
    assert_eq!(engine.snapshot(), before);
}

#[test]
fn audio_loss_handoff_is_continuous_and_never_uses_video_as_master() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::AudioDevice, ts(0)).unwrap();
    let before = engine.tick(ts(400)).unwrap();
    let handoff = engine.audio_device_lost(ts(400)).unwrap();
    let after = engine.tick(ts(440)).unwrap();

    assert_eq!(before.position, handoff.position);
    assert_eq!(handoff.clock_master, Some(ClockMaster::Synthetic));
    assert_eq!(after.position.frame, before.position.frame + 1);
}

#[test]
fn seek_invalidates_old_epoch_delivery() {
    let mut engine = engine();
    let first = engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let current = engine.seek(FramePosition::new(50, Rational::new(1, 25)), ts(10)).unwrap();

    let accepted = engine
        .observe_frame_delivery(FrameDelivery {
            epoch: first.epoch,
            quality_revision: first.quality_revision,
            demand_sequence: FrameDemandSequence(0),
            target_frame: 0,
            kind: FrameDeliveryKind::Ready,
        })
        .unwrap();
    assert!(!accepted);
    assert_eq!(engine.snapshot(), current);
}

#[test]
fn atomic_running_seek_rotates_one_epoch_and_publishes_priming_demand() {
    let mut engine = engine();
    engine
        .play_timeline(
            timeline_binding(100),
            FramePosition::new(0, Rational::new(1, 25)),
            ts(0),
        )
        .unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let prior_epoch = engine.snapshot().epoch;

    let snapshot = engine
        .seek_timeline(
            timeline_binding(100),
            FramePosition::new(50, Rational::new(1, 25)),
            ts(10),
        )
        .unwrap();
    let demand = engine.pending_frame_demand().expect("running seek demand");

    assert_eq!(snapshot.epoch.get(), prior_epoch.get() + 1);
    assert_eq!(snapshot.state, TransportState::Priming);
    assert_eq!(snapshot.clock_master, Some(ClockMaster::Synthetic));
    assert_eq!(demand.target, snapshot.position);
    assert!(demand.deadline.is_some());
}

#[test]
fn rejected_atomic_binding_does_not_consume_time_or_mutate_transport() {
    let mut engine = engine();
    let before = engine.snapshot();

    let rejected = engine.play_timeline(
        timeline_binding(100),
        FramePosition::new(0, Rational::new(1, 30)),
        ts(10),
    );

    assert_eq!(rejected, Err(PlaybackError::MismatchedTimelineTimeBase));
    assert_eq!(engine.snapshot(), before);
    engine
        .play_timeline(
            timeline_binding(100),
            FramePosition::new(0, Rational::new(1, 25)),
            ts(5),
        )
        .expect("rejected binding must not consume monotonic time");
}

#[test]
fn paused_seek_publishes_an_untimed_current_frame_demand() {
    let mut engine = engine();

    let snapshot = engine.seek(FramePosition::new(50, Rational::new(1, 25)), ts(10)).expect("seek");
    let demand = engine.pending_frame_demand().expect("paused seek demand");

    assert_eq!(snapshot.state, TransportState::Paused);
    assert_eq!(demand.target, snapshot.position);
    assert!(demand.deadline.is_none());
    let ticket = FramePresentationTicket::for_demand(demand, FramePresentationQuality::Ready);
    assert_eq!(ticket.deadline(), None);
    assert_eq!(
        ticket.complete_at(ts(10_000)).kind,
        FrameDeliveryKind::Ready
    );
}

#[test]
fn sustained_pressure_recovers_by_resolution_without_proxy_semantics() {
    let policy = PlaybackPolicy {
        priming_limit: Duration::from_millis(500),
        pressure_window: 4,
        pressure_threshold: 3,
        healthy_deliveries_to_recover: 2,
        ..PlaybackPolicy::default()
    };
    let mut engine = PlaybackEngine::new(Rational::new(1, 25), policy).unwrap();
    engine.play(100, ts(0)).unwrap();
    let playing = engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();

    for (index, kind) in [
        FrameDeliveryKind::Late,
        FrameDeliveryKind::Late,
        FrameDeliveryKind::Ready,
        FrameDeliveryKind::Failed,
    ]
    .into_iter()
    .enumerate()
    {
        engine.tick(ts((index as u64 + 1) * 40)).unwrap();
        let delivery = current_delivery(&engine, kind);
        engine.observe_frame_delivery(delivery).unwrap();
    }
    let recovering = engine.snapshot();
    assert_eq!(recovering.state, TransportState::Recovering);
    assert_eq!(recovering.preview_scale, PreviewResolutionScale::Half);
    assert!(recovering.quality_revision > playing.quality_revision);

    for index in 0..2 {
        engine.tick(ts(200 + index * 40)).unwrap();
        let delivery = current_delivery(&engine, FrameDeliveryKind::Ready);
        engine.observe_frame_delivery(delivery).unwrap();
    }
    assert_eq!(engine.snapshot().state, TransportState::Playing);
    assert_eq!(
        engine.snapshot().preview_scale,
        PreviewResolutionScale::Full
    );
}

#[test]
fn repeated_presentable_degradation_enters_resolution_recovery() {
    let policy = PlaybackPolicy {
        pressure_window: 3,
        pressure_threshold: 3,
        ..PlaybackPolicy::default()
    };
    let mut engine = PlaybackEngine::new(Rational::new(1, 25), policy).unwrap();
    engine.play(100, ts(0)).unwrap();

    let priming_delivery = current_delivery(&engine, FrameDeliveryKind::Degraded);
    engine.observe_frame_delivery(priming_delivery).unwrap();
    engine.observe_video_preroll(video_preroll(&engine, 1, 1)).unwrap();
    assert_eq!(engine.snapshot().state, TransportState::Playing);

    for index in 1..=2 {
        engine.tick(ts(index * 40)).unwrap();
        let delivery = current_delivery(&engine, FrameDeliveryKind::Degraded);
        engine.observe_frame_delivery(delivery).unwrap();
    }

    assert_eq!(engine.snapshot().state, TransportState::Recovering);
    assert_eq!(
        engine.snapshot().preview_scale,
        PreviewResolutionScale::Half
    );
}

#[test]
fn blocked_delivery_is_not_reclassified_as_buffering_or_pressure() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let delivery = current_delivery(&engine, FrameDeliveryKind::Blocked);
    engine.observe_frame_delivery(delivery).unwrap();
    assert_eq!(engine.snapshot().state, TransportState::Blocked);
    assert_eq!(engine.snapshot().clock_master, None);
}

#[test]
fn non_monotonic_time_is_rejected_without_moving_position() {
    let mut engine = engine();
    engine.play(100, ts(100)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(100)).unwrap();
    let before = engine.snapshot();
    assert_eq!(
        engine.tick(ts(99)),
        Err(PlaybackError::NonMonotonicTimestamp)
    );
    assert_eq!(engine.snapshot(), before);
}

#[test]
fn fractional_rates_do_not_accumulate_float_drift() {
    let mut engine =
        PlaybackEngine::new(Rational::new(1001, 30000), PlaybackPolicy::default()).unwrap();
    engine.play(100_000, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let snapshot = engine
        .tick(MonotonicTimestamp::from_duration(Duration::from_secs(1001)))
        .unwrap();
    assert_eq!(snapshot.position.frame, 30_000);
}

#[test]
fn next_frame_delay_uses_remaining_subframe_time() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    assert_eq!(
        engine.time_until_next_frame(ts(10)).unwrap(),
        Some(Duration::from_millis(30))
    );
}

#[test]
fn invalid_recovery_policy_is_rejected_at_the_interface() {
    let result = PlaybackEngine::new(
        Rational::new(1, 25),
        PlaybackPolicy {
            priming_limit: Duration::from_millis(500),
            pressure_window: 4,
            pressure_threshold: 5,
            healthy_deliveries_to_recover: 1,
            ..PlaybackPolicy::default()
        },
    );
    assert!(matches!(result, Err(PlaybackError::InvalidPolicy)));
}

#[test]
fn paused_playhead_may_seek_beyond_current_content_end() {
    let mut engine = engine();
    engine
        .seek_timeline(
            timeline_binding(20),
            FramePosition::new(120, Rational::new(1, 25)),
            ts(0),
        )
        .unwrap();
    let snapshot = engine.seek(FramePosition::new(200, Rational::new(1, 25)), ts(0)).unwrap();
    assert_eq!(snapshot.state, TransportState::Paused);
    assert_eq!(snapshot.position.frame, 200);
}

#[test]
fn duplicate_terminal_delivery_cannot_mutate_pressure_twice() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let delivery = current_delivery(&engine, FrameDeliveryKind::Late);
    assert!(engine.observe_frame_delivery(delivery).unwrap());
    assert!(!engine.observe_frame_delivery(delivery).unwrap());
}

#[test]
fn frame_demand_identity_and_deadline_advance_once_per_target() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let first = engine.frame_demand().expect("initial demand");
    assert_eq!(first.target.frame, 0);
    assert_eq!(first.deadline, Some(ts(40)));

    engine.tick(ts(10)).unwrap();
    assert_eq!(engine.frame_demand(), Some(first));

    engine.tick(ts(40)).unwrap();
    let second = engine.frame_demand().expect("next demand");
    assert_eq!(second.target.frame, 1);
    assert!(second.sequence.get() > first.sequence.get());
    assert_eq!(second.deadline, Some(ts(80)));
}

#[test]
fn natural_end_replaces_the_previous_terminal_with_an_untimed_final_demand() {
    let mut engine = engine();
    engine.play(2, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine.tick(ts(40)).unwrap();
    let penultimate = engine.pending_frame_demand().expect("penultimate demand");
    assert_eq!(penultimate.target.frame, 1);
    assert!(engine
        .observe_frame_delivery(FrameDelivery::for_demand(
            penultimate.identity(),
            FrameDeliveryKind::Ready,
        ))
        .unwrap());

    let snapshot = engine.tick(ts(80)).unwrap();
    let final_demand = engine.pending_frame_demand().expect("final demand");

    assert_eq!(snapshot.state, TransportState::Ended);
    assert_eq!(final_demand.target.frame, 2);
    assert_eq!(final_demand.deadline, None);
    assert!(final_demand.sequence.get() > penultimate.sequence.get());
}

#[test]
fn delivery_for_superseded_demand_is_rejected_even_on_same_epoch() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let old = current_delivery(&engine, FrameDeliveryKind::Ready);
    engine.tick(ts(40)).unwrap();

    assert!(!engine.observe_frame_delivery(old).unwrap());
    assert_eq!(engine.snapshot().state, TransportState::Playing);
}

#[test]
fn priming_timeout_starts_at_deadline_and_catches_up_without_extra_drift() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();

    let snapshot = engine.tick(ts(750)).unwrap();

    assert_eq!(snapshot.state, TransportState::Playing);
    assert_eq!(snapshot.clock_master, Some(ClockMaster::Synthetic));
    assert_eq!(snapshot.position.frame, 6);
}
