use mondrian_core::{AudioSamplePosition, AudioSampleRate, FramePosition, Rational};
use mondrian_playback::{
    AudioClockObservationGrade, AudioDeviceClockObservation, AudioDeviceClockState, ClockMaster,
    FrameDeliveryCandidate, FrameDeliveryKind, MonotonicTimestamp, PlaybackEngine,
    PlaybackEvidenceCollector, PlaybackEvidenceConfig, PlaybackPolicy, PlaybackTimelineBinding,
};
use std::time::Duration;

const SAMPLE_RATE: u32 = 48_000;
const CALLBACK_FRAMES: u64 = 480;
const CALLBACK_PERIOD_MS: u64 = 10;
const RUN_SECONDS: u64 = 30 * 60;
const LOSS_AT_STEP: u64 = 90_000;
const REACQUIRE_AT_STEP: u64 = 90_100;

#[test]
fn thirty_minute_48khz_2997_av_continuity_survives_device_clock_reacquisition() {
    let video_time_base = Rational::new(1_001, 30_000);
    let mut engine = PlaybackEngine::new(video_time_base, PlaybackPolicy::default()).unwrap();
    let binding = PlaybackTimelineBinding::new(None, 1, video_time_base, 60_000).unwrap();
    engine
        .play_timeline(
            binding,
            FramePosition::new(0, video_time_base),
            timestamp(0),
        )
        .unwrap();
    engine.complete_priming(ClockMaster::Synthetic, timestamp(0)).unwrap();

    let mut evidence = PlaybackEvidenceCollector::new(PlaybackEvidenceConfig {
        event_capacity: 64,
        sample_capacity: 128,
    })
    .unwrap();
    let first = engine
        .observe_audio_device_clock(audio_observation(
            engine.snapshot().epoch,
            1,
            0,
            0,
            timestamp(0),
        ))
        .unwrap();
    assert_eq!(first.clock_master, Some(ClockMaster::AudioDevice));
    observe_and_deliver_current(&mut engine, &mut evidence, timestamp(0));

    let total_steps = RUN_SECONDS * 1_000 / CALLBACK_PERIOD_MS;
    for step in 1..=total_steps {
        let now = timestamp(step * CALLBACK_PERIOD_MS);
        if step == LOSS_AT_STEP {
            engine.audio_device_lost(now).unwrap();
        } else if step < LOSS_AT_STEP {
            engine
                .observe_audio_device_clock(audio_observation(
                    engine.snapshot().epoch,
                    1,
                    0,
                    step * CALLBACK_FRAMES,
                    now,
                ))
                .unwrap();
        } else if step < REACQUIRE_AT_STEP {
            engine.tick(now).unwrap();
        } else {
            let anchor_sample = REACQUIRE_AT_STEP * CALLBACK_FRAMES;
            engine
                .observe_audio_device_clock(audio_observation(
                    engine.snapshot().epoch,
                    2,
                    anchor_sample,
                    (step - REACQUIRE_AT_STEP) * CALLBACK_FRAMES,
                    now,
                ))
                .unwrap();
        }
        observe_and_deliver_current(&mut engine, &mut evidence, now);
    }

    let snapshot = engine.snapshot();
    let report = evidence.report();
    assert_eq!(snapshot.position.frame, 53_946);
    assert_eq!(snapshot.clock_master, Some(ClockMaster::AudioDevice));
    assert_eq!(report.observed_duration_us, RUN_SECONDS * 1_000_000);
    assert_eq!(report.clock_residency.synthetic_us, 1_000_000);
    assert_eq!(report.clock_residency.audio_device_us, 1_799_000_000);
    assert!(report.delivery_phase_error.audio_device.point_error.max_us <= 10_000);
    assert!(report.delivery_phase_error.audio_device.uncertainty.max_us <= 10_000);
    assert!(report.delivery_phase_error.audio_device.proven_error.max_us <= 20_000);
    assert_eq!(report.delivery_phase_error.synthetic.uncertainty.max_us, 0);
    assert!(report.delivery_phase_error.synthetic.proven_error.max_us <= 10_000);
    assert_eq!(
        report.delivery_phase_error.audio_device.proven_error.count
            + report.delivery_phase_error.synthetic.proven_error.count,
        report.deliveries.ready
    );
    assert_eq!(report.delivery_phase_error.unproven_presentable, 0);
    assert_eq!(report.delivery_phase_error.phase_not_applicable, 0);
    assert_eq!(report.audio_underrun_frames, 0);
    assert_eq!(report.audio_underrun_recoveries, 0);
    assert_eq!(report.retained_event_count, 64);
    assert!(report.evicted_event_count > 100_000);
}

fn audio_observation(
    epoch: mondrian_playback::PlaybackEpoch,
    stream_generation: u64,
    media_anchor_sample: u64,
    consumed_frames: u64,
    observed_at: MonotonicTimestamp,
) -> AudioDeviceClockObservation {
    AudioDeviceClockObservation {
        epoch,
        stream_generation,
        sample_rate: SAMPLE_RATE,
        consumed_frames,
        media_anchor: AudioSamplePosition::new(
            i64::try_from(media_anchor_sample).expect("sample anchor"),
            AudioSampleRate::new(SAMPLE_RATE).expect("sample rate"),
        ),
        observed_at,
        grade: AudioClockObservationGrade::CallbackConsumptionEstimate,
        estimated_latency_frames: 0,
        uncertainty_frames: CALLBACK_FRAMES as u32,
        underrun_frames: 0,
        state: AudioDeviceClockState::Running,
    }
}

fn observe_and_deliver_current(
    engine: &mut PlaybackEngine,
    evidence: &mut PlaybackEvidenceCollector,
    now: MonotonicTimestamp,
) {
    let snapshot = engine.snapshot();
    let demand = engine.frame_demand();
    evidence.observe_snapshot(now, snapshot, demand).unwrap();
    let Some(demand) = demand else {
        return;
    };
    let delivery = FrameDeliveryCandidate::for_demand(demand.identity(), FrameDeliveryKind::Ready)
        .complete_at(now);
    let application = engine.observe_frame_delivery(delivery).unwrap();
    if application.accepted() {
        evidence.observe_delivery(application).unwrap();
    }
}

fn timestamp(milliseconds: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_duration(Duration::from_millis(milliseconds))
}
