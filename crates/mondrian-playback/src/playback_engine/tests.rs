use super::*;

#[test]
fn authoritative_monotonic_timestamp_addition_fails_closed_on_overflow() {
    assert_eq!(
        MonotonicTimestamp::ZERO.checked_add(Duration::from_millis(5)),
        Ok(ts(5))
    );
    assert_eq!(
        MonotonicTimestamp::from_duration(Duration::MAX).checked_add(Duration::from_nanos(1)),
        Err(PlaybackError::TransportArithmeticOverflow)
    );
}

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
    let delivery =
        FrameDeliveryCandidate::for_demand(identity, FrameDeliveryKind::Ready).complete_at(ts(1));

    assert_eq!(delivery.identity(), identity);
    assert_eq!(delivery.completed_at(), ts(1));
}

#[test]
fn presentation_ticket_classifies_completion_at_the_final_deadline() {
    let demand = FrameDemand {
        kind: FrameDemandKind::TimedPlayback,
        epoch: PlaybackEpoch(7),
        quality_revision: 3,
        sequence: FrameDemandSequence(11),
        sequence_id: None,
        timeline_revision: 9,
        target: FramePosition::new(42, Rational::new(1, 25)),
        deadline: Some(ts(40)),
        late_presentation_grace_ns: 5_000_000,
        preview_scale: PreviewResolutionScale::Full,
    };
    let ready = FramePresentationTicket::for_demand(demand, FramePresentationQuality::Ready);
    let degraded = FramePresentationTicket::for_demand(demand, FramePresentationQuality::Degraded);

    assert_eq!(ready.delivery_kind_at(ts(39)), FrameDeliveryKind::Ready);
    assert_eq!(
        degraded.delivery_kind_at(ts(39)),
        FrameDeliveryKind::Degraded
    );
    assert_eq!(ready.complete_at(ts(39)).kind(), FrameDeliveryKind::Ready);
    assert_eq!(
        degraded.complete_at(ts(39)).kind(),
        FrameDeliveryKind::Degraded
    );
    assert_eq!(
        ready.delivery_kind_at(ts(40)),
        FrameDeliveryKind::Degraded,
        "delivery inside the late-presentation grace is presented degraded"
    );
    assert_eq!(
        ready.complete_at(ts(40)).kind(),
        FrameDeliveryKind::Degraded,
        "completion at the deadline is still presented degraded within grace"
    );
    assert_eq!(
        ready.delivery_kind_at(ts(44)),
        FrameDeliveryKind::Degraded,
        "delivery at the grace boundary remains presentable"
    );
    assert_eq!(
        ready.delivery_kind_at(ts(45)),
        FrameDeliveryKind::Degraded,
        "completion exactly at the grace boundary is still presentable"
    );
    assert_eq!(ready.delivery_kind_at(ts(46)), FrameDeliveryKind::Late);
    assert_eq!(ready.complete_at(ts(46)).kind(), FrameDeliveryKind::Late);
    assert_eq!(
        degraded.complete_at(ts(41)).kind(),
        FrameDeliveryKind::Degraded
    );
    assert_eq!(
        degraded.complete_at(ts(45)).kind(),
        FrameDeliveryKind::Degraded
    );
    assert_eq!(degraded.complete_at(ts(46)).kind(), FrameDeliveryKind::Late);
}

#[test]
fn engine_application_binds_delivery_target_snapshot_and_same_instant_phase() {
    let mut engine = engine();
    engine.play(100, ts(0)).expect("play");
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).expect("priming");
    let demand = engine.pending_frame_demand().expect("demand");
    let delivery = FramePresentationTicket::for_demand(demand, FramePresentationQuality::Ready)
        .complete_at(ts(10));

    let application = engine.observe_frame_delivery(delivery).expect("application");
    let phase = application.clock_phase().expect("clock phase");

    assert!(application.accepted());
    assert_eq!(application.delivery(), delivery);
    assert_eq!(application.target(), Some(demand.target));
    assert_eq!(application.snapshot(), engine.snapshot());
    assert_eq!(phase.epoch(), demand.epoch);
    assert_eq!(phase.master(), ClockMaster::Synthetic);
    assert_eq!(phase.observed_at(), delivery.completed_at());
    assert_eq!(phase.phase_ns(), 10_000_000);
    assert_eq!(phase.uncertainty_ns(), 0);
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
        media_anchor: AudioSamplePosition::new(
            0,
            AudioSampleRate::new(48_000).expect("sample rate"),
        ),
        observed_at,
        grade: AudioClockObservationGrade::CallbackConsumptionEstimate,
        estimated_latency_frames: u32::try_from(consumed_frames.min(1_000))
            .expect("bounded latency"),
        uncertainty_frames: 240,
        underrun_frames: 0,
        state: AudioDeviceClockState::Running,
    }
}

#[test]
fn authoritative_phase_lowers_directly_to_nearest_audio_sample() {
    let time_base = Rational::new(1_001, 30_000);
    let mut engine = PlaybackEngine::new(time_base, PlaybackPolicy::default()).expect("engine");
    let binding = PlaybackTimelineBinding::new(None, 1, time_base, 100).expect("binding");
    let snapshot = engine
        .seek_timeline(binding, FramePosition::new(1, time_base), ts(0))
        .expect("seek");
    let sample_rate = AudioSampleRate::new(48_000).expect("sample rate");

    let anchor = engine
        .authoritative_audio_sample_position_at(snapshot.epoch, ts(0), sample_rate)
        .expect("audio anchor");

    assert_eq!(anchor.sample(), 1_602);
    assert_eq!(anchor.rate(), sample_rate);
    assert_eq!(
        engine.authoritative_audio_sample_position_at(PlaybackEpoch(99), ts(0), sample_rate),
        Err(PlaybackError::MismatchedPlaybackEpoch)
    );
}

#[test]
fn active_audio_master_rejects_a_different_output_sample_grid() {
    let mut engine = engine();
    engine.play(100, ts(0)).expect("play");
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).expect("priming");
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .expect("audio handoff");
    let before = engine.snapshot();

    assert_eq!(
        engine.authoritative_audio_sample_position_at(
            before.epoch,
            ts(0),
            AudioSampleRate::new(44_100).expect("other rate"),
        ),
        Err(PlaybackError::MismatchedAudioSampleRate)
    );
    assert_eq!(engine.snapshot(), before);
}

#[test]
fn inconsistent_or_negative_audio_anchor_fails_before_engine_mutation() {
    let mut engine = engine();
    engine.play(100, ts(0)).expect("play");
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).expect("priming");
    let before = engine.snapshot();

    let mut mismatched = audio_observation(&engine, 1_000, ts(0));
    mismatched.media_anchor =
        AudioSamplePosition::new(0, AudioSampleRate::new(44_100).expect("mismatched rate"));
    assert_eq!(
        engine.observe_audio_device_clock(mismatched),
        Err(PlaybackError::MismatchedAudioSampleRate)
    );
    assert_eq!(engine.snapshot(), before);

    let mut negative = audio_observation(&engine, 1_000, ts(0));
    negative.media_anchor =
        AudioSamplePosition::new(-1, AudioSampleRate::new(48_000).expect("sample rate"));
    assert_eq!(
        engine.observe_audio_device_clock(negative),
        Err(PlaybackError::InvalidAudioClockPosition)
    );
    assert_eq!(engine.snapshot(), before);
}

#[test]
fn handoff_budget_includes_ceil_sample_uncertainty() {
    let sample_rate = AudioSampleRate::new(48_000).expect("sample rate");
    let qualify = |anchor_sample| {
        let mut engine = engine();
        engine.play(100, ts(0)).expect("play");
        engine.complete_priming(ClockMaster::Synthetic, ts(0)).expect("priming");
        let mut observation = audio_observation(&engine, 0, ts(0));
        observation.stream_generation = 9;
        observation.media_anchor = AudioSamplePosition::new(anchor_sample, sample_rate);
        observation.estimated_latency_frames = 0;
        engine.observe_audio_device_clock(observation).expect("handoff observation")
    };

    let boundary = qualify(720);
    assert_eq!(boundary.clock_master, Some(ClockMaster::AudioDevice));
    assert_eq!(
        boundary.audio_handoff.map(|evidence| evidence.proven_phase_error_ns),
        Some(20_000_000)
    );

    let over = qualify(721);
    assert_eq!(over.clock_master, Some(ClockMaster::Synthetic));
    assert_eq!(
        over.audio_handoff.map(|evidence| evidence.status),
        Some(AudioClockHandoffStatus::PhaseRejected)
    );
}

#[test]
fn sample_duration_upper_bound_uses_ceil_not_floor() {
    assert_eq!(
        sample_frames_ns_ceil(1, AudioSampleRate::new(3).expect("sample rate")),
        Ok(333_333_334)
    );
}

#[test]
fn audio_master_cannot_be_selected_without_a_real_sample_anchor() {
    let mut engine = engine();
    engine.play(100, ts(0)).expect("play");
    let before = engine.snapshot();

    assert_eq!(
        engine.complete_priming(ClockMaster::AudioDevice, ts(0)),
        Err(PlaybackError::InvalidAudioClockPosition)
    );
    assert_eq!(engine.snapshot(), before);
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
        .observe_frame_delivery(
            FrameDeliveryCandidate::for_demand(demand.identity(), FrameDeliveryKind::Ready)
                .complete_at(ts(40))
        )
        .expect("current audio-clock demand delivery")
        .accepted());
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
fn pause_supersedes_the_running_deadline_with_an_untimed_current_demand() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let running = engine.pending_frame_demand().expect("running demand");
    assert!(running.deadline.is_some());

    let paused = engine.pause(ts(10)).expect("pause");
    let demand = engine.pending_frame_demand().expect("paused demand");

    assert_eq!(paused.state, TransportState::Paused);
    assert_eq!(demand.target, paused.position);
    assert_eq!(demand.deadline, None);
    assert_ne!(demand.sequence, running.sequence);
    assert!(!engine
        .observe_frame_delivery(
            FrameDeliveryCandidate::for_demand(running.identity(), FrameDeliveryKind::Ready)
                .complete_at(ts(0))
        )
        .expect("superseded running delivery")
        .accepted());
    assert_eq!(
        FramePresentationTicket::for_demand(demand, FramePresentationQuality::Ready)
            .complete_at(ts(10_000))
            .kind(),
        FrameDeliveryKind::Ready
    );
}

#[test]
fn explicit_pause_reissues_a_consumed_stable_frame_demand() {
    let mut engine = engine();
    engine.play(2, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let ended = engine.tick(ts(80)).expect("natural end");
    let consumed = engine.pending_frame_demand().expect("final demand");
    assert_eq!(ended.state, TransportState::Ended);
    assert_eq!(consumed.deadline, None);

    assert!(engine
        .observe_frame_delivery(
            FrameDeliveryCandidate::for_demand(consumed.identity(), FrameDeliveryKind::Failed)
                .complete_at(ts(80))
        )
        .expect("terminal final-frame delivery")
        .accepted());
    assert!(engine.pending_frame_demand().is_none());

    let paused = engine.pause(ts(80)).expect("idempotent pause");
    let replacement = engine.pending_frame_demand().expect("replacement stable-frame demand");

    assert_eq!(paused.state, TransportState::Ended);
    assert_eq!(replacement.target, paused.position);
    assert_eq!(replacement.deadline, None);
    assert_ne!(replacement.sequence, consumed.sequence);
}

#[test]
fn pause_preserves_natural_end_and_correctness_blockers() {
    let mut ended = engine();
    ended.play(1, ts(0)).unwrap();
    ended.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let natural_end = ended.tick(ts(40)).expect("reach natural end");
    let final_demand = ended.frame_demand().expect("untimed final demand");

    let pause_at_end = ended.pause(ts(50)).expect("idempotent pause at end");
    assert_eq!(pause_at_end.state, TransportState::Ended);
    assert_eq!(pause_at_end.position, natural_end.position);
    assert_eq!(ended.frame_demand(), Some(final_demand));

    let mut blocked = engine();
    blocked.play(100, ts(0)).unwrap();
    blocked.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    blocked
        .observe_frame_delivery(current_delivery(
            &blocked,
            FrameDeliveryKind::Blocked,
            ts(0),
        ))
        .expect("apply correctness blocker");
    let blocked_snapshot = blocked.snapshot();

    let pause_while_blocked = blocked.pause(ts(10)).expect("idempotent pause while blocked");
    assert_eq!(pause_while_blocked.state, TransportState::Blocked);
    assert_eq!(pause_while_blocked.position, blocked_snapshot.position);
    assert_eq!(pause_while_blocked.clock_master, None);
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
fn invalid_final_audio_observation_cannot_block_confirmed_device_loss() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .expect("qualify stream generation 7");
    let mut invalid_final = audio_observation(&engine, 1_960, ts(20));
    invalid_final.sample_rate = 44_100;

    let application = engine
        .audio_device_lost_with_final_observation(7, Some(invalid_final), ts(20))
        .expect("mandatory loss handoff");

    assert!(!application.final_observation_applied());
    assert_eq!(application.snapshot(), engine.snapshot());
    assert_eq!(
        application.snapshot().clock_master,
        Some(ClockMaster::Synthetic)
    );
    assert_eq!(engine.monotonic_high_water(), ts(20));
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
        .expect("stale completion is a non-authoritative fact")
        .accepted());

    engine.complete_priming(ClockMaster::Synthetic, ts(40)).unwrap();
    let mut recovered_audio = audio_observation(&engine, 0, ts(40));
    recovered_audio.stream_generation = 8;
    recovered_audio.media_anchor =
        AudioSamplePosition::new(23_040, AudioSampleRate::new(48_000).expect("sample rate"));
    recovered_audio.estimated_latency_frames = 0;
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
    let completed_at = deadline.saturating_add(Duration::from_millis(1));
    let delivery = delayed_ticket.complete_at(completed_at);
    assert_eq!(
        delivery.kind(),
        FrameDeliveryKind::Degraded,
        "a delivery inside the late-presentation grace is presented degraded rather than dropped"
    );
    assert!(engine
        .observe_frame_delivery(delivery)
        .expect("current delayed presentation remains an accepted terminal fact")
        .accepted());
    assert_eq!(engine.snapshot().clock_master, Some(ClockMaster::Synthetic));

    let beyond_grace =
        delayed_ticket.complete_at(deadline.saturating_add(Duration::from_millis(60)));
    assert_eq!(
        beyond_grace.kind(),
        FrameDeliveryKind::Late,
        "a delivery beyond the late-presentation grace remains Late"
    );
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
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 2_920, ts(40)))
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
    reanchored.media_anchor =
        AudioSamplePosition::new(48_000, AudioSampleRate::new(48_000).expect("sample rate"));
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
    observation.media_anchor =
        AudioSamplePosition::new(0, AudioSampleRate::new(48_000).expect("sample rate"));
    observation.estimated_latency_frames = 0;

    let accepted = engine.observe_audio_device_clock(observation).unwrap();

    assert_eq!(accepted.clock_master, Some(ClockMaster::AudioDevice));
    assert_eq!(
        accepted.audio_handoff,
        Some(AudioClockHandoffEvidence {
            stream_generation: 8,
            phase_error_ns: 0,
            uncertainty_ns: 5_000_000,
            proven_phase_error_ns: 5_000_000,
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
    observation.media_anchor =
        AudioSamplePosition::new(0, AudioSampleRate::new(48_000).expect("sample rate"));
    observation.estimated_latency_frames = 0;

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
    rejected.media_anchor =
        AudioSamplePosition::new(0, AudioSampleRate::new(48_000).expect("sample rate"));
    rejected.estimated_latency_frames = 0;
    engine.observe_audio_device_clock(rejected).unwrap();

    let mut aligned = audio_observation(&engine, 1_000, ts(90));
    aligned.stream_generation = 9;
    aligned.media_anchor =
        AudioSamplePosition::new(3_320, AudioSampleRate::new(48_000).expect("sample rate"));
    aligned.estimated_latency_frames = 0;
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

fn current_delivery(
    engine: &PlaybackEngine,
    kind: FrameDeliveryKind,
    completed_at: MonotonicTimestamp,
) -> FrameDelivery {
    let demand = engine.frame_demand().expect("active frame demand");
    FrameDeliveryCandidate::for_demand(demand.identity(), kind).complete_at(completed_at)
}

fn video_preroll(
    engine: &PlaybackEngine,
    ready_media_frames: usize,
    preservable_media_frames: usize,
) -> VideoPrerollObservation {
    VideoPrerollObservation {
        demand: engine.frame_demand().expect("active frame demand").identity(),
        ready_media_frames,
        preservable_media_frames,
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
fn priming_deadline_remains_independent_from_media_frame_boundaries() {
    let mut engine = engine();
    engine.play(100, ts(10)).unwrap();
    let priming = engine.pending_frame_demand().expect("priming demand");
    assert_eq!(priming.deadline, Some(ts(1510)));

    engine.tick(ts(200)).unwrap();
    assert_eq!(engine.pending_frame_demand(), Some(priming));

    engine.complete_priming(ClockMaster::Synthetic, ts(200)).unwrap();
    let running = engine.pending_frame_demand().expect("running demand");
    assert_eq!(running.deadline, Some(ts(220)));
}

#[test]
fn terminal_late_priming_demand_still_exposes_fallback_and_next_frame_wakes() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    let priming = engine.pending_frame_demand().expect("priming demand");
    assert_eq!(priming.deadline, Some(ts(1500)));

    let late = FrameDeliveryCandidate::for_demand(priming.identity(), FrameDeliveryKind::Late)
        .complete_at(ts(250));
    assert!(engine.observe_frame_delivery(late).unwrap().accepted());
    assert_eq!(engine.snapshot().state, TransportState::Priming);
    assert!(engine.pending_frame_demand().is_none());
    assert_eq!(
        engine.time_until_next_wake(ts(250)).unwrap(),
        Some(Duration::from_millis(1250))
    );

    assert_eq!(
        engine.tick(ts(1499)).unwrap().state,
        TransportState::Priming
    );
    let fallback = engine.tick(ts(1500)).unwrap();
    assert_eq!(fallback.state, TransportState::Playing);
    assert_eq!(fallback.position.frame, 0);
    assert_eq!(
        engine.time_until_next_wake(ts(1500)).unwrap(),
        Some(Duration::from_millis(40))
    );

    let advanced = engine.tick(ts(1540)).unwrap();
    assert_eq!(advanced.position.frame, 1);
    let next = engine.pending_frame_demand().expect("next-frame demand");
    assert_ne!(next.identity(), priming.identity());
    assert_eq!(next.target.frame, 1);
}

#[test]
fn presented_current_frame_waits_for_bounded_video_preroll() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();

    let delivery = current_delivery(&engine, FrameDeliveryKind::Ready, ts(5));
    assert!(engine.observe_frame_delivery(delivery).unwrap().accepted());
    assert_eq!(engine.snapshot().state, TransportState::Priming);
    assert!(engine.frame_demand().is_some());
    assert!(engine.pending_frame_demand().is_none());

    assert!(engine.observe_video_preroll(video_preroll(&engine, 1, 1), ts(17)).unwrap());
    assert_eq!(engine.snapshot().state, TransportState::Playing);
    assert_eq!(engine.snapshot().clock_master, Some(ClockMaster::Synthetic));
    let next_deadline = ts(17).saturating_add(
        engine.time_until_next_wake(ts(17)).unwrap().expect("playing frame deadline"),
    );
    assert_eq!(next_deadline, ts(57));
}

#[test]
fn video_preroll_cannot_start_before_current_frame_is_presented() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();

    assert!(!engine.observe_video_preroll(video_preroll(&engine, 1, 1), ts(5)).unwrap());
    assert_eq!(engine.snapshot().state, TransportState::Priming);

    let delivery = current_delivery(&engine, FrameDeliveryKind::Ready, ts(17));
    assert!(engine.observe_frame_delivery(delivery).unwrap().accepted());
    assert_eq!(engine.snapshot().state, TransportState::Playing);
    let next_deadline = ts(17).saturating_add(
        engine.time_until_next_wake(ts(17)).unwrap().expect("playing frame deadline"),
    );
    assert_eq!(next_deadline, ts(57));
}

#[test]
fn no_available_future_media_releases_presented_current_frame() {
    let mut engine = engine();
    engine.play(0, ts(0)).unwrap();
    let delivery = current_delivery(&engine, FrameDeliveryKind::Ready, ts(5));
    engine.observe_frame_delivery(delivery).unwrap();

    assert!(engine.observe_video_preroll(video_preroll(&engine, 0, 0), ts(17)).unwrap());
    assert_eq!(engine.snapshot().state, TransportState::Playing);
}

#[test]
fn observed_priming_reanchor_preserves_fractional_frame_boundary() {
    let frame_duration = Duration::from_nanos(33_366_667);
    let mut engine =
        PlaybackEngine::new(Rational::new(1001, 30000), PlaybackPolicy::default()).unwrap();
    engine.play(100, ts(0)).unwrap();

    assert!(!engine.observe_video_preroll(video_preroll(&engine, 1, 1), ts(5)).unwrap());
    let delivery = current_delivery(&engine, FrameDeliveryKind::Ready, ts(17));
    assert!(engine.observe_frame_delivery(delivery).unwrap().accepted());

    assert_eq!(
        engine.time_until_next_wake(ts(17)).unwrap(),
        Some(frame_duration)
    );
    assert_eq!(
        ts(17).saturating_add(frame_duration),
        MonotonicTimestamp::from_duration(Duration::from_nanos(50_366_667))
    );
}

#[test]
fn invalid_or_stale_video_preroll_cannot_mutate_session() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    let first_demand = engine.frame_demand().expect("first demand").identity();
    engine.stop(ts(1)).unwrap();
    engine.play(100, ts(1)).unwrap();
    let before = engine.snapshot();
    let high_water_before = engine.monotonic_high_water();

    assert!(!engine
        .observe_video_preroll(
            VideoPrerollObservation {
                demand: first_demand,
                ready_media_frames: 1,
                preservable_media_frames: 1,
            },
            ts(2)
        )
        .unwrap());
    assert_eq!(engine.snapshot(), before);
    assert_eq!(engine.monotonic_high_water(), high_water_before);

    let mut wrong_demand = engine.frame_demand().expect("current demand").identity();
    wrong_demand.target_frame += 1;
    assert!(!engine
        .observe_video_preroll(
            VideoPrerollObservation {
                demand: wrong_demand,
                ready_media_frames: 1,
                preservable_media_frames: 1,
            },
            ts(2)
        )
        .unwrap());
    assert_eq!(engine.snapshot(), before);
    assert_eq!(engine.monotonic_high_water(), high_water_before);

    assert_eq!(
        engine.observe_video_preroll(video_preroll(&engine, 2, 1), ts(2)),
        Err(PlaybackError::InvalidVideoPrerollObservation)
    );
    assert_eq!(
        engine.observe_video_preroll(
            video_preroll(
                &engine,
                MAX_BOUNDED_VIDEO_PREROLL_FRAMES,
                MAX_BOUNDED_VIDEO_PREROLL_FRAMES + 1,
            ),
            ts(3)
        ),
        Err(PlaybackError::InvalidVideoPrerollObservation)
    );
    assert_eq!(engine.snapshot(), before);
}

#[test]
fn audio_loss_handoff_is_continuous_and_never_uses_video_as_master() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .unwrap();
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
        .observe_frame_delivery(
            FrameDeliveryCandidate::for_demand(
                FrameDemandIdentity {
                    epoch: first.epoch,
                    quality_revision: first.quality_revision,
                    sequence: FrameDemandSequence(0),
                    target_frame: 0,
                },
                FrameDeliveryKind::Ready,
            )
            .complete_at(ts(0)),
        )
        .unwrap();
    assert!(!accepted.accepted());
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
        ticket.complete_at(ts(10_000)).kind(),
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
        let completed_at = ts((index as u64 + 1) * 40);
        let delivery = current_delivery(&engine, kind, completed_at);
        engine.observe_frame_delivery(delivery).unwrap();
    }
    let recovering = engine.snapshot();
    assert_eq!(recovering.state, TransportState::Recovering);
    assert_eq!(recovering.preview_scale, PreviewResolutionScale::Half);
    assert!(recovering.quality_revision > playing.quality_revision);

    for index in 0..2 {
        engine.tick(ts(200 + index * 40)).unwrap();
        let completed_at = ts(200 + index * 40);
        let delivery = current_delivery(&engine, FrameDeliveryKind::Ready, completed_at);
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

    let priming_delivery = current_delivery(&engine, FrameDeliveryKind::Degraded, ts(0));
    engine.observe_frame_delivery(priming_delivery).unwrap();
    engine.observe_video_preroll(video_preroll(&engine, 1, 1), ts(0)).unwrap();
    assert_eq!(engine.snapshot().state, TransportState::Playing);

    for index in 1..=2 {
        engine.tick(ts(index * 40)).unwrap();
        let completed_at = ts(index * 40);
        let delivery = current_delivery(&engine, FrameDeliveryKind::Degraded, completed_at);
        engine.observe_frame_delivery(delivery).unwrap();
    }

    assert_eq!(engine.snapshot().state, TransportState::Recovering);
    assert_eq!(
        engine.snapshot().preview_scale,
        PreviewResolutionScale::Half
    );
}

#[test]
fn clock_superseded_unpresented_demands_enter_resolution_recovery() {
    let policy = PlaybackPolicy {
        pressure_window: 3,
        pressure_threshold: 3,
        ..PlaybackPolicy::default()
    };
    let mut engine = PlaybackEngine::new(Rational::new(1, 25), policy).unwrap();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();

    engine.tick(ts(40)).unwrap();
    engine.tick(ts(80)).unwrap();
    engine.tick(ts(120)).unwrap();

    assert_eq!(engine.snapshot().state, TransportState::Recovering);
    assert_eq!(
        engine.snapshot().preview_scale,
        PreviewResolutionScale::Half
    );
    assert_eq!(
        engine.pending_frame_demand().expect("recovery demand").target.frame,
        3
    );

    for instant in [160, 200, 240] {
        engine.tick(ts(instant)).unwrap();
    }
    assert_eq!(
        engine.snapshot().preview_scale,
        PreviewResolutionScale::Half,
        "a new scale must receive one terminal execution attempt before another reduction"
    );

    let half_attempt = current_delivery(&engine, FrameDeliveryKind::Late, ts(240));
    engine.observe_frame_delivery(half_attempt).unwrap();
    assert_eq!(
        engine.snapshot().preview_scale,
        PreviewResolutionScale::Quarter,
        "a failed Half attempt may authorize the next bounded reduction"
    );
}

#[test]
fn blocked_delivery_is_not_reclassified_as_buffering_or_pressure() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let delivery = current_delivery(&engine, FrameDeliveryKind::Blocked, ts(0));
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
fn timeline_binding_rejects_a_negative_content_extent() {
    assert_eq!(
        PlaybackTimelineBinding::new(None, 1, Rational::new(1, 25), -1),
        Err(PlaybackError::InvalidTimelineExtent)
    );
}

#[test]
fn transport_arithmetic_failure_does_not_publish_a_partial_session() {
    let mut engine = engine();
    let before_snapshot = engine.snapshot();
    let before_demand = engine.frame_demand();
    let before_sequence_id = engine.sequence_id();
    let before_timeline_revision = engine.timeline_revision();
    let before_timestamp = engine.last_timestamp;
    let before_demand_sequence = engine.next_demand_sequence;
    let overflowing_time_base = Rational::new(i64::MAX, 1);
    let binding =
        PlaybackTimelineBinding::new(Some(SequenceId::new()), 99, overflowing_time_base, i64::MAX)
            .expect("positive binding");

    let result = engine.play_timeline(
        binding,
        FramePosition::new(i64::MAX, overflowing_time_base),
        ts(0),
    );

    assert_eq!(result, Err(PlaybackError::TransportArithmeticOverflow));
    assert_eq!(engine.snapshot(), before_snapshot);
    assert_eq!(engine.frame_demand(), before_demand);
    assert_eq!(engine.sequence_id(), before_sequence_id);
    assert_eq!(engine.timeline_revision(), before_timeline_revision);
    assert_eq!(engine.last_timestamp, before_timestamp);
    assert_eq!(engine.next_demand_sequence, before_demand_sequence);
}

#[test]
fn invalid_audio_clock_position_does_not_publish_observation_or_phase() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let before_snapshot = engine.snapshot();
    let before_demand = engine.frame_demand();
    let mut invalid = audio_observation(&engine, 100, ts(10));
    invalid.estimated_latency_frames = 101;

    let result = engine.observe_audio_device_clock(invalid);

    assert_eq!(result, Err(PlaybackError::InvalidAudioClockPosition));
    assert_eq!(engine.snapshot(), before_snapshot);
    assert_eq!(engine.frame_demand(), before_demand);
}

#[test]
fn fractional_frame_demand_uses_phase_budget_before_exact_frame_boundary() {
    let mut engine =
        PlaybackEngine::new(Rational::new(1001, 30000), PlaybackPolicy::default()).unwrap();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();

    let demand = engine.pending_frame_demand().expect("fractional demand");
    assert_eq!(
        demand.deadline,
        Some(MonotonicTimestamp::from_duration(Duration::from_millis(20)))
    );
    let boundary = engine
        .tick(MonotonicTimestamp::from_duration(Duration::from_nanos(
            33_366_667,
        )))
        .expect("exact boundary");
    assert_eq!(boundary.position.frame, 1);
}

#[test]
fn next_wake_delay_uses_remaining_presentation_phase_budget() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    assert_eq!(
        engine.time_until_next_wake(ts(10)).unwrap(),
        Some(Duration::from_millis(10))
    );
}

#[test]
fn audio_clock_wake_does_not_sleep_past_the_projected_video_boundary() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .expect("audio handoff");
    let demand = engine.pending_frame_demand().expect("initial audio demand");
    let delivery = FramePresentationTicket::for_demand(demand, FramePresentationQuality::Ready)
        .complete_at(ts(1));
    assert!(engine.observe_frame_delivery(delivery).unwrap().accepted());

    assert_eq!(
        engine.next_wake(ts(20)).unwrap(),
        Some(PlaybackWake {
            after: Duration::from_millis(2),
            reason: PlaybackWakeReason::AudioDevicePoll,
        })
    );
    assert_eq!(
        engine.next_wake(ts(39)).unwrap(),
        Some(PlaybackWake {
            after: Duration::from_millis(1),
            reason: PlaybackWakeReason::FrameBoundary,
        })
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
    let delivery = current_delivery(&engine, FrameDeliveryKind::Late, ts(0));
    assert!(engine.observe_frame_delivery(delivery).unwrap().accepted());
    assert!(!engine.observe_frame_delivery(delivery).unwrap().accepted());
}

#[test]
fn frame_demand_identity_and_deadline_advance_once_per_target() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let first = engine.frame_demand().expect("initial demand");
    assert_eq!(first.target.frame, 0);
    assert_eq!(first.deadline, Some(ts(20)));

    engine.tick(ts(10)).unwrap();
    assert_eq!(engine.frame_demand(), Some(first));

    engine.tick(ts(40)).unwrap();
    let second = engine.frame_demand().expect("next demand");
    assert_eq!(second.target.frame, 1);
    assert!(second.sequence.get() > first.sequence.get());
    assert_eq!(second.deadline, Some(ts(60)));
}

#[test]
fn running_demand_deadline_uses_remaining_synthetic_clock_phase() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();

    let mid_frame = engine.tick(ts(55)).expect("mid-frame tick");
    let mid_frame_demand = engine.pending_frame_demand().expect("mid-frame demand");
    assert_eq!(mid_frame.position.frame, 1);
    assert_eq!(mid_frame_demand.target.frame, 1);
    assert_eq!(mid_frame_demand.deadline, Some(ts(60)));

    let tardy = engine.tick(ts(159)).expect("tardy tick");
    let tardy_demand = engine.pending_frame_demand().expect("tardy demand");
    assert_eq!(tardy.position.frame, 3);
    assert_eq!(tardy_demand.target.frame, 3);
    assert_eq!(tardy_demand.deadline, Some(ts(159)));
}

#[test]
fn audio_clock_demand_deadline_uses_observed_media_phase() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine
        .observe_audio_device_clock(audio_observation(&engine, 1_000, ts(0)))
        .expect("audio handoff");

    let snapshot = engine
        .observe_audio_device_clock(audio_observation(&engine, 3_640, ts(55)))
        .expect("mid-frame audio observation");
    let demand = engine.pending_frame_demand().expect("audio-clock demand");

    assert_eq!(snapshot.clock_master, Some(ClockMaster::AudioDevice));
    assert_eq!(snapshot.position.frame, 1);
    assert_eq!(demand.target.frame, 1);
    assert_eq!(demand.deadline, Some(ts(55)));
}

#[test]
fn audio_clock_handoff_accounts_for_point_and_growing_uncertainty() {
    let mut ahead = engine();
    ahead.play(100, ts(0)).unwrap();
    ahead.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    ahead.tick(ts(20)).unwrap();
    let synthetic = ahead.pending_frame_demand().expect("synthetic demand");
    let handed_off = ahead
        .observe_audio_device_clock(audio_observation(&ahead, 2_440, ts(20)))
        .expect("ahead audio handoff");
    let audio = ahead.pending_frame_demand().expect("audio demand");
    assert_eq!(handed_off.clock_master, Some(ClockMaster::AudioDevice));
    assert_ne!(audio.sequence, synthetic.sequence);
    assert_eq!(audio.deadline, synthetic.deadline);
    assert_eq!(audio.deadline, Some(ts(20)));

    let mut behind = engine();
    behind.play(100, ts(0)).unwrap();
    behind.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    behind.tick(ts(20)).unwrap();
    let synthetic = behind.pending_frame_demand().expect("synthetic demand");
    let handed_off = behind
        .observe_audio_device_clock(audio_observation(&behind, 1_480, ts(20)))
        .expect("behind audio handoff");
    let audio = behind.pending_frame_demand().expect("audio demand");
    assert_eq!(handed_off.clock_master, Some(ClockMaster::AudioDevice));
    assert_ne!(audio.sequence, synthetic.sequence);
    assert_eq!(audio.deadline, synthetic.deadline);

    let mut aligned = engine();
    aligned.play(100, ts(0)).unwrap();
    aligned.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    aligned
        .observe_audio_device_clock(audio_observation(&aligned, 1_000, ts(0)))
        .expect("aligned audio handoff");
    let audio = aligned.pending_frame_demand().expect("aligned audio demand");
    assert_eq!(
        audio.deadline,
        Some(MonotonicTimestamp::from_duration(Duration::from_nanos(
            7_500_000,
        )))
    );
}

#[test]
fn quality_revision_and_recovery_cannot_extend_current_frame_deadline() {
    let policy = PlaybackPolicy {
        pressure_window: 1,
        pressure_threshold: 1,
        healthy_deliveries_to_recover: 1,
        ..PlaybackPolicy::default()
    };
    let mut engine = PlaybackEngine::new(Rational::new(1, 25), policy).unwrap();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    engine.tick(ts(10)).unwrap();

    let full_quality = engine.pending_frame_demand().expect("full-quality demand");
    assert_eq!(full_quality.deadline, Some(ts(20)));
    assert!(engine
        .observe_frame_delivery(
            FrameDeliveryCandidate::for_demand(full_quality.identity(), FrameDeliveryKind::Late,)
                .complete_at(ts(10))
        )
        .expect("pressure delivery")
        .accepted());

    let recovery = engine.pending_frame_demand().expect("recovery demand");
    assert_eq!(engine.snapshot().state, TransportState::Recovering);
    assert_eq!(recovery.target, full_quality.target);
    assert_eq!(recovery.deadline, full_quality.deadline);
    assert!(engine
        .observe_frame_delivery(
            FrameDeliveryCandidate::for_demand(recovery.identity(), FrameDeliveryKind::Ready)
                .complete_at(ts(10))
        )
        .expect("healthy recovery delivery")
        .accepted());

    let restored = engine.pending_frame_demand().expect("restored demand");
    assert_eq!(engine.snapshot().state, TransportState::Playing);
    assert_eq!(restored.target, full_quality.target);
    assert_eq!(restored.deadline, full_quality.deadline);
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
        .observe_frame_delivery(
            FrameDeliveryCandidate::for_demand(penultimate.identity(), FrameDeliveryKind::Ready,)
                .complete_at(ts(40))
        )
        .unwrap()
        .accepted());

    let snapshot = engine.tick(ts(80)).unwrap();
    let final_demand = engine.pending_frame_demand().expect("final demand");

    assert_eq!(snapshot.state, TransportState::Ended);
    assert_eq!(final_demand.target.frame, 2);
    assert_eq!(final_demand.deadline, None);
    assert_eq!(
        final_demand.kind,
        FrameDemandKind::PersistentStill,
        "natural end retires the timed playback demand into a persistent still demand"
    );
    assert_eq!(
        engine.active_playback_demand(),
        None,
        "ended transport has no realtime playback demand"
    );
    assert!(final_demand.sequence.get() > penultimate.sequence.get());
}

#[test]
fn ended_still_demand_is_presentable_without_a_deadline_and_holds_until_superseded() {
    let mut engine = engine();
    engine.play(60, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    assert_eq!(
        engine.frame_demand().map(|demand| demand.kind),
        Some(FrameDemandKind::TimedPlayback)
    );
    assert!(engine.active_playback_demand().is_some());

    let snapshot = engine.tick(ts(2_400)).unwrap();
    assert_eq!(snapshot.state, TransportState::Ended);
    assert_eq!(snapshot.position.frame, 60);
    let still = engine.frame_demand().expect("ended still demand");
    assert_eq!(still.kind, FrameDemandKind::PersistentStill);
    assert_eq!(still.target.frame, 60);
    assert_eq!(still.deadline, None);

    // A late-by-any-amount presentation of the still frame is never Late:
    // the still obligation has no realtime deadline.
    let delivery = FramePresentationTicket::for_demand(still, FramePresentationQuality::Ready)
        .complete_at(ts(9_999));
    assert_eq!(delivery.kind(), FrameDeliveryKind::Ready);
    assert!(engine.observe_frame_delivery(delivery).unwrap().accepted());
    assert_eq!(engine.pending_frame_demand(), None);

    // Seek supersedes the consumed still demand with a fresh one.
    engine.seek(FramePosition::new(30, Rational::new(1, 25)), ts(10_000)).unwrap();
    let superseding = engine.frame_demand().expect("superseding demand");
    assert_eq!(superseding.kind, FrameDemandKind::PersistentStill);
    assert_eq!(superseding.target.frame, 30);
    assert_eq!(engine.active_playback_demand(), None);
    assert_ne!(superseding.identity(), still.identity());
}

#[test]
fn paused_and_stopped_transport_hold_persistent_still_demands() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.pause(ts(400)).unwrap();
    assert_eq!(
        engine.frame_demand().map(|demand| demand.kind),
        Some(FrameDemandKind::PersistentStill)
    );
    assert_eq!(engine.active_playback_demand(), None);

    engine.stop(ts(500)).unwrap();
    assert_eq!(
        engine.frame_demand().map(|demand| demand.kind),
        Some(FrameDemandKind::PersistentStill)
    );
    assert_eq!(engine.active_playback_demand(), None);
}

#[test]
fn delivery_for_superseded_demand_is_rejected_even_on_same_epoch() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();
    engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
    let old = current_delivery(&engine, FrameDeliveryKind::Ready, ts(0));
    engine.tick(ts(40)).unwrap();

    assert!(!engine.observe_frame_delivery(old).unwrap().accepted());
    assert_eq!(engine.snapshot().state, TransportState::Playing);
}

#[test]
fn priming_timeout_starts_at_deadline_and_catches_up_without_extra_drift() {
    let mut engine = engine();
    engine.play(100, ts(0)).unwrap();

    let snapshot = engine.tick(ts(1750)).unwrap();

    assert_eq!(snapshot.state, TransportState::Playing);
    assert_eq!(snapshot.clock_master, Some(ClockMaster::Synthetic));
    assert_eq!(snapshot.position.frame, 6);
}
