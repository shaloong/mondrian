//! Real App transport and Media FIFO with a third-party CLAP effect.
//! The existing software callback and headless video-ready seam are reused;
//! no positive direct renderer call or alternative transport clock is used.
use super::*;

// Duration stores integer nanoseconds; exact sample boundaries can be fractional.
const TIMESTAMP_QUANTUM: Duration = Duration::from_nanos(1);

const SCENARIO: Scenario = Scenario {
    mode: 0,
    chunk: 128,
    watermark: 4_096,
    capture_windows: 256,
    capture_clears: 16,
    output_capacity: 8_192,
    lookahead: 0,
};

fn source_sample(frame: usize) -> [f32; 2] {
    // Exact binary scaling keeps the compressor below threshold and the output
    // within headroom for the selected12dB gain, at every tested source position.
    input_sample(frame).map(|value| value * 0.25)
}

fn sample_at_frame(
    state: &AppState,
    frame: i64,
    rounding: mondrian_core::AudioSampleRounding,
) -> usize {
    use mondrian_core::{AudioSamplePosition, AudioSampleRate, FramePosition, TimelineTime};
    let time = TimelineTime::from_frame_position(FramePosition::new(
        frame,
        state.active_sequence().unwrap().time_base(),
    ))
    .unwrap();
    usize::try_from(
        AudioSamplePosition::from_timeline_time(
            time,
            AudioSampleRate::new(RATE).unwrap(),
            rounding,
        )
        .unwrap()
        .sample(),
    )
    .unwrap()
}

fn at_watermark(frames: usize) -> bool {
    // A partial callback need not permit one more indivisible native block.
    frames <= SCENARIO.watermark && SCENARIO.watermark - frames < SCENARIO.chunk
}

fn assert_pcm(frames: &[[f32; 2]], first: usize, gain_db: f64) {
    let gain = 10.0_f64.powf(gain_db / 20.0);
    // Fixed absolute Float32 arithmetic bound; a one-sample source shift is
    // over15 times larger, including at the lower6dB gain.
    let tolerance = 2.0 * f64::from(f32::EPSILON);
    for (offset, actual) in frames.iter().enumerate() {
        for (actual, input) in actual.iter().zip(source_sample(first + offset)) {
            let expected = f64::from(input) * gain;
            assert!(
                actual.is_finite() && (f64::from(*actual) - expected).abs() <= tolerance,
                "public sample{}: actual={actual}, expected={expected}",
                first + offset
            );
        }
    }
}

fn assert_fifo(output: &Arc<Mutex<CaptureState>>, first: usize, gain_db: f64) {
    let capture = output.lock();
    assert!(!capture.overflow);
    assert!(at_watermark(capture.queue.len()));
    assert!(capture.snapshot.active);
    assert_pcm(
        &capture.queue.iter().copied().collect::<Vec<_>>(),
        first,
        gain_db,
    );
}

fn ready_current_video(state: &mut AppState) {
    // Same seam as warm_native_fader_source: acknowledge only the actual current
    // demand. This audio test does not qualify GPU or Viewer presentation.
    if state.playback_engine.snapshot().state != mondrian_playback::TransportState::Playing
        && state.playback_engine.frame_demand().is_some()
    {
        state.observe_viewer_frame_delivery(mondrian_playback::FrameDeliveryKind::Ready);
        if let Some(demand) = state.playback_engine.frame_demand().map(|demand| demand.identity()) {
            state.observe_video_preroll_at_wall(
                demand,
                0,
                0,
                true,
                true,
                state.playback_observation_instant_anchor,
            );
        }
    }
}

fn fill_after_transport(state: &mut AppState) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        ready_current_video(state);
        let now = state.playback_observation_instant_anchor;
        let snapshot = poll_audio_playback_at_for_test(state, now, false)
            .expect("original App/Media poll, including native reprepare service");
        assert_eq!(snapshot.render_substitution_count, 0);
        assert_eq!(snapshot.render_generation_recovery_count, 0);
        assert_eq!(snapshot.underrun_recovery_count, 0);
        assert_eq!(snapshot.output.unwrap().underrun_frames, 0);
        assert!(!snapshot.output.unwrap().stream_failed);
        if snapshot.in_flight == 0
            && snapshot.output.unwrap().active
            && at_watermark(snapshot.output.unwrap().buffered_frames)
        {
            assert_eq!(
                state.playback_engine.snapshot().state,
                mondrian_playback::TransportState::Playing
            );
            assert!(snapshot.activation_preroll_satisfied);
            return;
        }
        assert!(
            Instant::now() < deadline,
            "actual transport did not settle: {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn consume_frames(
    state: &mut AppState,
    output: &Arc<Mutex<CaptureState>>,
    first: usize,
    frames: usize,
    db: f64,
) {
    let consumed = output.lock().snapshot.active_callback_consumed_frames;
    // Round cumulative sample boundaries once, rather than accumulating a
    // rounded block duration. Their absolute timestamp error is <one quantum.
    let before_ns = (consumed * 1_000_000_000).div_ceil(u64::from(RATE));
    let after_ns = ((consumed + frames as u64) * 1_000_000_000).div_ceil(u64::from(RATE));
    let observed_at =
        state.playback_observation_instant_anchor + Duration::from_nanos(after_ns - before_ns);
    let actual = output.lock().consume(frames, observed_at);
    assert_pcm(&actual, first, db);
    poll_audio_playback_at_for_test(state, observed_at, true).unwrap();
    fill_after_transport(state);
}

#[test]
#[ignore = "official Zam4.5 CLAP, actual Makeup12dB state, matching Product and FFmpeg; App play/pause/resume/seek with software callbacks"]
fn native_zam_clap_transport_preserves_state_gain_and_sample_position() {
    for host_override in [None, Some(6.0)] {
        // Explicit coverage inputs, not assertions about project defaults.
        for frame_rate in [
            mondrian_core::Rational::FPS_24,
            mondrian_core::Rational::FPS_2997,
        ] {
            qualify_transport(host_override, frame_rate);
        }
    }
}

fn qualify_transport(host_override: Option<f64>, frame_rate: mondrian_core::Rational) {
    println!("ZAM_TRANSPORT_INPUT: fps={frame_rate:?}, host_override={host_override:?}");
    let path =
        |key| std::path::PathBuf::from(std::env::var_os(key).expect("native qualification input"));
    let seed = std::fs::read(path("MONDRIAN_ZAM_NONDEFAULT_STATE")).unwrap();
    assert!(!seed.is_empty() && seed.len() <= 64 * 1024);
    let installed = Arc::new(
        DiscoveredClapAudioProcessorSpecResolver::discover(
            path("MONDRIAN_CLAP_TEST_HELPER"),
            [path("MONDRIAN_CLAP_MIXED_VENDOR_TEST_PLUGIN")],
        )
        .unwrap(),
    );
    let mut contract = reference_render_contract();
    contract.sample_rate = RATE;
    contract.max_block_frames = SCENARIO.chunk;
    contract.processing_mode = mondrian_audio::AudioProcessingMode::Realtime;
    let mut instance = installed
        .create_instance("com.zamaudio.ZamCompX2", contract, Some(seed))
        .unwrap();
    assert!(instance
        .parameters
        .contains_key(&mondrian_core::ParameterId::new("clap.param.5").unwrap()));
    assert!(instance.parameters.values().all(|parameter| parameter.automation.is_none()));
    if let Some(value) = host_override {
        let id = mondrian_core::ParameterId::new("clap.param.5").unwrap();
        instance
            .parameters
            .get_mut(&id)
            .unwrap()
            .set_automation(mondrian_core::ExactAutomationCurve::new(id, value).unwrap())
            .unwrap();
    }
    let resolver = Arc::new(IsolatedAudioProcessorResolver::new(installed));
    let (_directory, library, mut sequence) =
        native_gain_media_fixture_with_samples(SOURCE_FRAMES, source_sample);
    sequence.settings.frame_rate = frame_rate;
    sequence.settings.audio_sample_rate = RATE;
    sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
    let mut state = AppState::with_audio_processor_resolver(resolver);
    assert!(state.shutdown_audio_playback_and_wait().all_workers_terminated());
    let mut capture = CaptureState::new(SCENARIO);
    // The existing adapter contract carries query/quantization uncertainty.
    // Zero would falsely claim exact rational sample time in an integer-ns
    // clock; its conservative conversion to sample units belongs to the adapter.
    capture.snapshot.last_callback_playback_delay_uncertainty = Some(TIMESTAMP_QUANTUM);
    let output = Arc::new(Mutex::new(capture));
    state.audio_playback = AppAudioPlayback::Available(Box::new(
        AudioPlayback::with_output(
            AudioPlaybackConfig {
                sample_rate: RATE,
                channel_layout: AudioChannelLayout::Stereo,
                chunk_frames: SCENARIO.chunk,
                preroll_frames: SCENARIO.chunk,
                high_watermark_frames: SCENARIO.watermark,
                max_in_flight: 1,
                ..AudioPlaybackConfig::product_default()
            },
            Box::new(CaptureOutput(Arc::clone(&output))),
        )
        .unwrap(),
    ));
    state.test_set_sequence(Some(sequence));
    state.test_set_asset_library(Some(library));
    let document = state.authoring.as_ref().unwrap().document().clone();
    let generation = state.project_author_generation();
    let history = state.authoring_history().unwrap().diagnostics();
    let db = host_override.unwrap_or(12.0);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        warm_native_fader_source(&mut state, &output, SCENARIO);
        assert_fifo(&output, 0, db);
        // Complete one authored frame even at fractional rates. Coordinates
        // derive from author inputs and are never fitted to produced PCM.
        let first_frame = sample_at_frame(&state, 1, mondrian_core::AudioSampleRounding::Ceil);
        let mut first = 0;
        while first < first_frame {
            let frames = (first_frame - first).min(SCENARIO.chunk);
            consume_frames(&mut state, &output, first, frames, db);
            first += frames;
        }
        assert_fifo(&output, first_frame, db);
        assert_eq!(
            state.playback_clock_master(),
            Some(mondrian_playback::ClockMaster::AudioDevice),
            "actual clock handoff: {:?}; audio={:?}",
            state.playback_engine.snapshot(),
            state.audio_playback_snapshot()
        );
        std::thread::sleep(
            state
                .playback_observation_instant_anchor
                .saturating_duration_since(Instant::now()),
        );
        state.pause().expect("public Pause after actual software consumption");
        assert_eq!(
            state.playback_engine.snapshot().state,
            mondrian_playback::TransportState::Paused
        );
        // Pause samples Instant::now; device-clock projection may legitimately
        // advance during synchronous worker waits. Its frame is the Resume input.
        let paused_frame = state.current_frame();
        assert!(paused_frame >= 1);
        let resume_sample = sample_at_frame(
            &state,
            paused_frame,
            mondrian_core::AudioSampleRounding::Nearest,
        );
        {
            let mut capture = output.lock();
            assert!(!capture.snapshot.active);
            let before = capture.queue.clone();
            assert_eq!(
                capture.callback(
                    INACTIVE_CALLBACK_FRAMES,
                    state.playback_observation_instant_anchor
                ),
                vec![[0.0; 2]; INACTIVE_CALLBACK_FRAMES]
            );
            assert_eq!(
                capture.queue, before,
                "paused callbacks cannot consume queued media"
            );
        }
        state.play().expect("public Resume");
        fill_after_transport(&mut state);
        assert_fifo(&output, resume_sample, db);
        consume_frames(&mut state, &output, resume_sample, SCENARIO.chunk, db);

        for frame in [paused_frame.checked_add(2).unwrap(), 1] {
            let sample =
                sample_at_frame(&state, frame, mondrian_core::AudioSampleRounding::Nearest);
            assert!(sample + SCENARIO.watermark < SOURCE_FRAMES);
            std::thread::sleep(
                state
                    .playback_observation_instant_anchor
                    .saturating_duration_since(Instant::now()),
            );
            state.seek(frame).expect("public seek while playing");
            assert_eq!(state.current_frame(), frame);
            {
                let mut capture = output.lock();
                assert!(!capture.snapshot.active);
                capture.callback(
                    INACTIVE_CALLBACK_FRAMES,
                    state.playback_observation_instant_anchor,
                );
            }
            fill_after_transport(&mut state);
            assert_fifo(&output, sample, db);
            consume_frames(&mut state, &output, sample, SCENARIO.chunk, db);
        }
        assert_eq!(state.authoring.as_ref().unwrap().document(), &document);
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(state.authoring_history().unwrap().diagnostics(), history);
        assert!(state.native_audio_editor.is_none());
    }));
    let deadline = Instant::now() + Duration::from_secs(2);
    let pause = state.pause();
    let audio = state.shutdown_audio_playback_until(deadline);
    let cleanup = state.shutdown_for_endurance(deadline);
    assert!(
        pause.is_ok() && audio.all_workers_terminated() && cleanup.all_resources_released(),
        "finite transport cleanup: pause={pause:?}, audio={audio:?}, cleanup={cleanup:?}"
    );
    assert!(!output.lock().overflow);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
