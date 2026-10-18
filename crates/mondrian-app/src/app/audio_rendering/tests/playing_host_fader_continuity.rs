//! Finite owned delay/PDC and echo-history continuity, using a deterministic
//! software callback driver. This does not qualify native DSP migration, exact
//! create/Enter counts, the complete UI pump, arbitrary tails or a physical DAC.

use super::*;
use crate::app::playback::{poll_audio_playback_at_for_test, AppAudioPlayback};
use crate::app::product_action::{AudioProductAction, ProductAction};
use mondrian_audio::{
    AudioContinuityEvidenceVisitor, AudioExecutionError, AudioProcessorCursorEvidence,
    AudioProcessorCursorState, AudioSessionCursorEvidence, AudioSessionCursorState,
    DiscoveredClapAudioProcessorSpecResolver, IsolatedAudioProcessorResolver,
};
use mondrian_media::{
    AudioBuffer, AudioOutputAdapter, AudioPlayback, AudioPlaybackConfig, AudioPlaybackSnapshot,
    AudioPlaybackState, RealtimeAudioOutputContract, RealtimeAudioOutputControlError,
    RealtimeAudioOutputEnqueueError, RealtimeAudioOutputEvent, RealtimeAudioOutputQuiescenceToken,
    RealtimeAudioOutputSnapshot,
};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const RATE: u32 = 48_000;
const SOURCE_FRAMES: usize = RATE as usize * 40;
const INACTIVE_CALLBACK_FRAMES: usize = 256;
const CONSUMED: usize = 137;
const ECHO_FRAMES: usize = 256;

#[derive(Clone, Copy)]
struct Scenario {
    mode: u8,
    chunk: usize,
    watermark: usize,
    capture_windows: usize,
    output_capacity: usize,
    lookahead: i64,
}

const DELAY: Scenario = Scenario {
    mode: 0,
    chunk: 256,
    watermark: 8_192,
    capture_windows: 80,
    output_capacity: 16_384,
    lookahead: 3,
};
const ECHO: Scenario = Scenario {
    mode: 12,
    chunk: 8_192,
    watermark: 65_536,
    capture_windows: 20,
    output_capacity: 96_000,
    lookahead: 0,
};

// Two coprime periods have a joint period over four billion frames, so the
// stereo pair never repeats within this finite 40-second WAV. Exact binary
// steps keep a one-sample error larger than the unchanged 1e-6 PCM tolerance.
fn input_sample(frame: usize) -> [f32; 2] {
    [
        0.125 + (frame % 65_521) as f32 / 131_072.0,
        -0.0625 - (frame % 65_519) as f32 / 131_072.0,
    ]
}

struct CapturedWindow {
    clear_serial: u64,
    samples: Vec<f32>,
}

#[derive(Clone, Copy, Debug)]
struct Discard {
    clear_serial: u64,
    frames: usize,
}

struct CaptureState {
    scenario: Scenario,
    snapshot: RealtimeAudioOutputSnapshot,
    revision: u64,
    // Output.clear grouping only; never a Media generation or AudioContinuityEpoch.
    clear_serial: u64,
    queue: VecDeque<[f32; 2]>,
    windows: Vec<CapturedWindow>,
    cleared: Vec<Discard>,
    activation_discarded: usize,
    overflow: bool,
}

impl CaptureState {
    fn new(scenario: Scenario) -> Self {
        Self {
            scenario,
            snapshot: RealtimeAudioOutputSnapshot {
                captured_at: Instant::now(),
                stream_generation: 1,
                contract: RealtimeAudioOutputContract {
                    sample_rate: RATE,
                    channel_layout: AudioChannelLayout::Stereo,
                    sample_format: mondrian_media::RealtimeAudioSampleFormat::F32,
                    channel_semantics:
                        mondrian_media::RealtimeAudioChannelSemantics::StereoConvention,
                    supported_buffer_size:
                        mondrian_media::RealtimeAudioSupportedBufferSize::Unknown,
                    candidates: mondrian_media::RealtimeAudioCandidateCounts {
                        enumerated: 1,
                        matching_channels: 1,
                        matching_sample_rate: 1,
                        executable: 1,
                    },
                },
                callback_consumed_frames: 0,
                active_callback_consumed_frames: 0,
                active_duration: None,
                callback_count: 0,
                underrun_frames: 0,
                last_callback_frames: 0,
                last_callback_playback_delay: Some(Duration::ZERO),
                last_callback_playback_delay_uncertainty: Some(Duration::ZERO),
                last_callback_age: None,
                buffered_frames: 0,
                stream_failed: false,
                active: false,
            },
            revision: 0,
            clear_serial: 0,
            queue: VecDeque::with_capacity(scenario.output_capacity),
            windows: Vec::with_capacity(scenario.capture_windows),
            cleared: Vec::with_capacity(8),
            activation_discarded: 0,
            overflow: false,
        }
    }

    // Like audio.rs render_f32_output_block, an inactive callback returns
    // silence without draining PCM, but still records stream-wide frames and
    // latest-callback facts. Only an active block advances its active interval.
    // This is a synchronous software callback, not hardware timestamp evidence.
    fn callback(&mut self, frames: usize, observed_at: Instant) -> Vec<[f32; 2]> {
        assert!(frames > 0 && frames <= INACTIVE_CALLBACK_FRAMES);
        let active_block = self.snapshot.active;
        let output = if active_block {
            assert!(frames <= self.queue.len());
            self.queue.drain(..frames).collect()
        } else {
            vec![[0.0; 2]; frames]
        };
        self.snapshot.callback_consumed_frames += frames as u64;
        if active_block {
            self.snapshot.active_callback_consumed_frames += frames as u64;
            self.snapshot.active_duration = Some(Duration::from_nanos(
                self.snapshot.active_callback_consumed_frames * 1_000_000_000 / u64::from(RATE),
            ));
        }
        self.snapshot.callback_count += 1;
        self.snapshot.last_callback_frames = frames as u32;
        self.snapshot.last_callback_age = Some(Duration::ZERO);
        self.snapshot.captured_at = observed_at;
        self.snapshot.buffered_frames = self.queue.len();
        output
    }

    // There is no background callback owner; the same mutex serializes this
    // actual FIFO drain with deactivate/clear until the whole callback returns.
    fn consume(&mut self, frames: usize, observed_at: Instant) -> Vec<[f32; 2]> {
        assert!(self.snapshot.active);
        self.callback(frames, observed_at)
    }
}

struct CaptureOutput(Arc<Mutex<CaptureState>>);

impl AudioOutputAdapter for CaptureOutput {
    fn poll(&mut self) -> Option<RealtimeAudioOutputEvent> {
        None
    }

    fn enqueue(&mut self, buffer: &AudioBuffer) -> Result<(), RealtimeAudioOutputEnqueueError> {
        let mut state = self.0.lock();
        if buffer.sample_rate != RATE {
            return Err(RealtimeAudioOutputEnqueueError::SampleRateMismatch {
                expected: RATE,
                actual: buffer.sample_rate,
            });
        }
        if buffer.channel_layout != AudioChannelLayout::Stereo {
            return Err(RealtimeAudioOutputEnqueueError::ChannelLayoutMismatch {
                expected: AudioChannelLayout::Stereo,
                actual: buffer.channel_layout,
            });
        }
        if buffer.samples.len() % 2 != 0 {
            return Err(
                RealtimeAudioOutputEnqueueError::IncompleteInterleavedFrame {
                    samples: buffer.samples.len(),
                    channels: 2,
                },
            );
        }
        let frames = buffer.frame_count();
        if state.windows.len() == state.scenario.capture_windows
            || frames > state.scenario.chunk
            || frames > state.scenario.output_capacity - state.queue.len()
        {
            state.overflow = true;
            return Err(RealtimeAudioOutputEnqueueError::InsufficientCapacity {
                required_samples: buffer.samples.len(),
                available_samples: 0,
            });
        }
        let clear_serial = state.clear_serial;
        state
            .windows
            .push(CapturedWindow { clear_serial, samples: buffer.samples.clone() });
        state
            .queue
            .extend(buffer.samples.chunks_exact(2).map(|pair| [pair[0], pair[1]]));
        state.snapshot.buffered_frames = state.queue.len();
        Ok(())
    }

    fn clear(&mut self) {
        let mut state = self.0.lock();
        let discard = Discard {
            clear_serial: state.clear_serial,
            frames: state.queue.len(),
        };
        if state.cleared.len() == 8 {
            state.overflow = true;
        } else {
            state.cleared.push(discard);
        }
        state.queue.clear();
        state.snapshot.buffered_frames = 0;
        state.clear_serial += 1;
    }

    fn validate_deactivation(&self) -> Result<(), RealtimeAudioOutputControlError> {
        let state = self.0.lock();
        if state.snapshot.active && state.revision == u64::MAX {
            return Err(
                RealtimeAudioOutputControlError::QuiescenceRevisionExhausted {
                    stream_generation: state.snapshot.stream_generation,
                },
            );
        }
        Ok(())
    }

    fn deactivate(
        &self,
    ) -> Result<Option<RealtimeAudioOutputQuiescenceToken>, RealtimeAudioOutputControlError> {
        self.validate_deactivation()?;
        let mut state = self.0.lock();
        if state.snapshot.active {
            state.revision += 1;
            state.snapshot.active = false;
        }
        Ok(Some(RealtimeAudioOutputQuiescenceToken::new(
            state.snapshot.stream_generation,
            state.revision,
        )))
    }

    fn is_quiescent(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
    ) -> Result<bool, RealtimeAudioOutputControlError> {
        let state = self.0.lock();
        if token.stream_generation() != state.snapshot.stream_generation {
            return Err(RealtimeAudioOutputControlError::StreamGenerationMismatch {
                token_generation: token.stream_generation(),
                current_generation: state.snapshot.stream_generation,
            });
        }
        if token.revision() != state.revision {
            return Err(
                RealtimeAudioOutputControlError::QuiescenceRevisionMismatch {
                    token_revision: token.revision(),
                    current_revision: state.revision,
                },
            );
        }
        // Synchronous callback calls hold this same mutex until they return.
        Ok(!state.snapshot.active)
    }

    fn activate_after_discard(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
        frames: usize,
    ) -> Result<(), RealtimeAudioOutputControlError> {
        if !self.is_quiescent(token)? {
            return Err(RealtimeAudioOutputControlError::CallbackNotQuiescent {
                revision: token.revision(),
            });
        }
        let mut state = self.0.lock();
        if frames > state.queue.len() {
            return Err(
                RealtimeAudioOutputControlError::InsufficientBufferedFrames {
                    requested_frames: frames,
                    buffered_frames: state.queue.len(),
                },
            );
        }
        state.queue.drain(..frames);
        state.activation_discarded += frames;
        state.snapshot.buffered_frames = state.queue.len();
        state.snapshot.active_callback_consumed_frames = 0;
        state.snapshot.active_duration = Some(Duration::ZERO);
        // Activation resets only the active interval, preserving the stream's
        // most recent callback metadata until a real next callback updates it.
        state.snapshot.active = true;
        Ok(())
    }

    fn buffered_frames(&self) -> usize {
        self.0.lock().queue.len()
    }

    fn capacity_frames(&self) -> Option<usize> {
        Some(self.0.lock().scenario.output_capacity)
    }

    fn snapshot(&self) -> Option<RealtimeAudioOutputSnapshot> {
        Some(self.0.lock().snapshot)
    }
}

#[derive(Default)]
struct CursorFacts {
    session: Option<AudioSessionCursorEvidence>,
    processor: Option<AudioProcessorCursorEvidence>,
}

impl AudioContinuityEvidenceVisitor for CursorFacts {
    fn session(&mut self, fact: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        if self.session.replace(fact).is_some() {
            return Err(AudioExecutionError::BufferTooLarge);
        }
        Ok(())
    }

    fn processor(&mut self, fact: AudioProcessorCursorEvidence) -> Result<(), AudioExecutionError> {
        if self.processor.replace(fact).is_some() {
            return Err(AudioExecutionError::BufferTooLarge);
        }
        Ok(())
    }
}

fn cursor_facts(state: &AppState) -> CursorFacts {
    let renderer = state.native_audio_renderer.as_ref().unwrap().upgrade().unwrap();
    let mut facts = CursorFacts::default();
    renderer.state.lock().delivery.visit_continuity_evidence(&mut facts).unwrap();
    facts
}

fn assert_heads(facts: &CursorFacts, generation: u64, next_public: i64, scenario: Scenario) {
    let session = facts.session.expect("one actual Session");
    assert_eq!(
        session.public_output_lookahead_frames,
        scenario.lookahead as usize
    );
    let AudioSessionCursorState::Active {
        epoch,
        next_public_sample,
        next_execution_sample,
        alignment_primed,
    } = session.state
    else {
        panic!("actual completed Session: {session:?}")
    };
    assert_eq!(epoch.get(), generation);
    assert!(alignment_primed);
    assert_eq!(next_public_sample.sample(), next_public);
    assert_eq!(
        next_execution_sample.sample(),
        next_public + scenario.lookahead
    );
    let processor = facts.processor.expect("one actual native occurrence");
    assert_eq!(processor.identity, session.identity);
    let AudioProcessorCursorState::Active { epoch, next_input_signal_sample } = processor.state
    else {
        panic!("actual completed native processor: {processor:?}")
    };
    assert_eq!(epoch.get(), generation);
    assert_eq!(
        next_input_signal_sample.sample(),
        next_public + scenario.lookahead
    );
}

fn assert_healthy(snapshot: AudioPlaybackSnapshot, generation: u64) {
    assert_eq!(
        snapshot.generation, generation,
        "no silent recovery rotation"
    );
    assert!(matches!(
        snapshot.state,
        AudioPlaybackState::Active | AudioPlaybackState::Prerolling
    ));
    assert_eq!(snapshot.render_substitution_count, 0);
    assert_eq!(snapshot.render_generation_recovery_count, 0);
    assert_eq!(snapshot.underrun_recovery_count, 0);
    assert_eq!(snapshot.active_interval_underrun_frames, 0);
    assert_eq!(snapshot.output.expect("software output").underrun_frames, 0);
    assert!(!snapshot.output.unwrap().stream_failed);
}

fn fill_to_watermark(
    state: &mut AppState,
    generation: u64,
    scenario: Scenario,
) -> AudioPlaybackSnapshot {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let observed_at = state.playback_observation_instant_anchor;
        let snapshot = poll_audio_playback_at_for_test(state, observed_at, false)
            .expect("original deterministic App/Media poll");
        assert_healthy(snapshot, generation);
        if snapshot.in_flight == 0 && snapshot.output.unwrap().buffered_frames == scenario.watermark
        {
            assert!(
                snapshot.activation_preroll_satisfied
                    && snapshot.output.unwrap().active
                    && snapshot.media_anchor.is_some(),
                "full PCM queue requires actual software activation and anchor: {snapshot:?}"
            );
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "owned native PCM completion deadline: {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn native_sample(scenario: Scenario, absolute: usize) -> [f32; 2] {
    // mode0's latency3 is compensated by public lookahead3. Mode12 has no
    // latency; its causal tap must remain part of the public native signal.
    let mut sample = input_sample(absolute);
    if scenario.mode == 12 && absolute >= ECHO_FRAMES {
        let tap = input_sample(absolute - ECHO_FRAMES);
        for channel in 0..2 {
            sample[channel] += 0.5 * tap[channel];
        }
    }
    sample
}

fn assert_window_shape(
    state: &CaptureState,
    first_window: usize,
    clear_serial: u64,
    scenario: Scenario,
) {
    let windows = &state.windows[first_window..];
    assert_eq!(windows.len(), scenario.watermark / scenario.chunk);
    for window in windows {
        assert_eq!(window.clear_serial, clear_serial);
        assert_eq!(window.samples.len(), scenario.chunk * 2);
    }
    assert_eq!(
        windows.iter().map(|window| window.samples.len()).sum::<usize>(),
        scenario.watermark * 2,
        "all original complete windows, including the first"
    );
    assert!(!state.overflow);
}

fn assert_windows(
    state: &CaptureState,
    first_window: usize,
    clear_serial: u64,
    start_sample: i64,
    fader_db: f64,
    scenario: Scenario,
) {
    assert_window_shape(state, first_window, clear_serial, scenario);
    let gain = 10.0_f64.powf(fader_db / 20.0);
    let mut frames = 0;
    for window in &state.windows[first_window..] {
        for pair in window.samples.chunks_exact(2) {
            let absolute = usize::try_from(start_sample + frames as i64).unwrap();
            assert!(
                absolute < SOURCE_FRAMES,
                "reference stays inside the real WAV"
            );
            let expected = native_sample(scenario, absolute);
            for channel in 0..2 {
                let expected = (f64::from(expected[channel]) * gain) as f32;
                assert!(
                    (pair[channel] - expected).abs() <= 1e-6,
                    "clear{clear_serial} public{absolute} channel{channel}: {} != {expected}",
                    pair[channel]
                );
            }
            frames += 1;
        }
    }
}

// The old completed PCM is an observation, never a DSP checkpoint or permission
// to restore it. Check every other new sample before returning the bounded W
// discrepancy, so a target RED cannot hide later ordinary PCM failures.
fn echo_history_discrepancy(
    state: &CaptureState,
    first_window: usize,
    anchor: i64,
    frontier: i64,
    scenario: Scenario,
) -> Option<String> {
    let end = anchor.checked_add(ECHO_FRAMES as i64);
    eprintln!("finite echo witness: A={anchor} F={frontier} W=[{anchor},{end:?}) H={ECHO_FRAMES}");
    assert!(
        anchor >= ECHO_FRAMES as i64 && end.is_some_and(|end| end <= frontier),
        "NOT_COVERED_ECHO_HISTORY: require 256 <= A and checked A+256 <= completed F; A={anchor} F={frontier} end={end:?}"
    );
    let end = end.unwrap();
    assert_eq!(end - anchor, ECHO_FRAMES as i64);
    let gain = 10.0_f64.powf(-6.0 / 20.0);
    let mut first = None;
    let mut max_old = 0.0_f32;
    let mut max_formula = 0.0_f32;
    let mut witness_frames = 0;
    let mut frames = 0;
    for window in &state.windows[first_window..] {
        for pair in window.samples.chunks_exact(2) {
            let absolute = usize::try_from(anchor + frames as i64).unwrap();
            let formula = native_sample(scenario, absolute);
            for channel in 0..2 {
                let expected = (f64::from(formula[channel]) * gain) as f32;
                if (absolute as i64) < end {
                    // Original windows start at public0 and cover all of F.
                    let old = state.windows[absolute / scenario.chunk].samples
                        [(absolute % scenario.chunk) * 2 + channel];
                    let old_scaled = (f64::from(old) * gain) as f32;
                    let tap = input_sample(absolute - ECHO_FRAMES)[channel];
                    assert!(
                        (f64::from(tap) * 0.5 * gain).abs() > 1e-3,
                        "history stimulus exceeds the unchanged 1e-6 tolerance"
                    );
                    let old_error = (pair[channel] - old_scaled).abs();
                    let formula_error = (pair[channel] - expected).abs();
                    max_old = max_old.max(old_error);
                    max_formula = max_formula.max(formula_error);
                    if (old_error > 1e-6
                        || formula_error > 1e-6
                        || !old_error.is_finite()
                        || !formula_error.is_finite())
                        && first.is_none()
                    {
                        first = Some((absolute, channel, pair[channel], old_scaled, expected));
                    }
                } else {
                    assert!(
                        (pair[channel] - expected).abs() <= 1e-6,
                        "ECHO_NON_WITNESS_PCM: public{absolute} channel{channel}: {} != {expected}",
                        pair[channel]
                    );
                }
            }
            if (absolute as i64) < end {
                witness_frames += 1;
            }
            frames += 1;
        }
    }
    assert_eq!(frames, scenario.watermark);
    assert_eq!(witness_frames, ECHO_FRAMES);
    first.map(|first| format!("A={anchor} F={frontier} W=[{anchor},{end}) frames={witness_frames} first(public,channel,actual,old_scaled,formula)={first:?} max_old={max_old} max_formula={max_formula}"))
}

#[test]
#[ignore = "current normal Product and owned CLAP sidechain mode0 DLL; software callback only"]
fn native_clap_playing_host_fader_preserves_latency_compensated_pcm() {
    run_host_fader_continuity(DELAY);
}

#[test]
#[ignore = "current normal Product and owned CLAP sidechain mode12 DLL; finite software history witness only"]
fn native_clap_playing_host_fader_preserves_finite_echo_history() {
    run_host_fader_continuity(ECHO);
}

fn run_host_fader_continuity(scenario: Scenario) {
    let helper = std::env::var_os("MONDRIAN_CLAP_TEST_HELPER")
        .map(std::path::PathBuf::from)
        .expect("current normal Product");
    let plugin = std::env::var_os("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN")
        .map(std::path::PathBuf::from)
        .expect("owned CLAP sidechain DLL");
    assert!(helper.is_absolute() && helper.is_file());
    assert!(plugin.is_absolute() && plugin.is_file());
    let installed = DiscoveredClapAudioProcessorSpecResolver::discover(helper, [plugin])
        .expect("actual owned CLAP discovery");
    let mut opaque = vec![scenario.mode];
    opaque.extend_from_slice(&0.5_f32.to_le_bytes());
    let instance = installed
        .create_instance(
            "org.mondrian.fixture.sidechain",
            reference_render_contract(),
            Some(opaque.clone()),
        )
        .expect("actual owned native author state");
    assert_eq!(
        instance.opaque_state.as_ref().map(|state| state.as_slice()),
        Some(opaque.as_slice())
    );
    let resolver = Arc::new(IsolatedAudioProcessorResolver::new(Arc::new(installed)));
    let (_directory, library, mut sequence) =
        native_gain_media_fixture_with_samples(SOURCE_FRAMES, input_sample);
    let output_id = sequence.audio_program.outputs[0].id;
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
    let mut state = AppState::with_audio_processor_resolver(resolver);
    assert!(state.shutdown_audio_playback_and_wait().all_workers_terminated());
    let output = Arc::new(Mutex::new(CaptureState::new(scenario)));
    state.audio_playback = AppAudioPlayback::Available(Box::new(
        AudioPlayback::with_output(
            AudioPlaybackConfig {
                chunk_frames: scenario.chunk,
                preroll_frames: scenario.chunk,
                high_watermark_frames: scenario.watermark,
                max_in_flight: 1,
                ..AudioPlaybackConfig::product_default()
            },
            Box::new(CaptureOutput(Arc::clone(&output))),
        )
        .expect("original no-device Media workers"),
    ));
    state.test_set_sequence(Some(sequence));
    state.test_set_asset_library(Some(library));

    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.play().expect("original App play prepares the actual native Source");
        // The existing headless video model supplies both prerequisites of
        // original play_ready: Ready delivery, then the matching preroll.
        assert!(!state.observe_viewer_frame_delivery(mondrian_playback::FrameDeliveryKind::Ready));
        // Ready consumes the pending terminal ticket; preroll still names the
        // same Engine demand, as the original play_ready fixture does.
        let demand = state
            .playback_engine
            .frame_demand()
            .map(|demand| demand.identity())
            .expect("original Engine video demand after Ready");
        // Existing deterministic video-preroll seam only; this audio gate does
        // not assert actual Viewer/GPU presentation or full public UI pumping.
        assert!(state.observe_video_preroll_at_wall(
            demand,
            0,
            0,
            true,
            true,
            state.playback_observation_instant_anchor,
        ));
        assert_eq!(
            state.playback_engine.snapshot().state,
            mondrian_playback::TransportState::Playing
        );
        {
            let mut capture = output.lock();
            assert!(!capture.snapshot.active);
            let queued_before = capture.queue.len();
            let silent = capture.callback(
                INACTIVE_CALLBACK_FRAMES,
                state.playback_observation_instant_anchor,
            );
            assert_eq!(silent, vec![[0.0; 2]; INACTIVE_CALLBACK_FRAMES]);
            assert_eq!(capture.queue.len(), queued_before);
            assert_eq!(capture.snapshot.buffered_frames, queued_before);
            assert_eq!(
                capture.snapshot.callback_consumed_frames,
                INACTIVE_CALLBACK_FRAMES as u64
            );
            assert_eq!(capture.snapshot.active_callback_consumed_frames, 0);
            assert_eq!(capture.snapshot.active_duration, None);
            assert_eq!(capture.snapshot.callback_count, 1);
            assert_eq!(
                capture.snapshot.last_callback_frames,
                INACTIVE_CALLBACK_FRAMES as u32
            );
            assert_eq!(capture.snapshot.last_callback_age, Some(Duration::ZERO));
            assert_eq!(
                capture.snapshot.captured_at,
                state.playback_observation_instant_anchor
            );
            assert!(!capture.snapshot.active);
        }
        let generation = state.audio_playback_snapshot().generation;
        let warmed = fill_to_watermark(&mut state, generation, scenario);
        assert_eq!(warmed.media_anchor.unwrap().sample(), 0);
        assert_eq!(warmed.next_start_sample, scenario.watermark as i64);
        assert_heads(
            &cursor_facts(&state),
            generation,
            scenario.watermark as i64,
            scenario,
        );
        let old_clear_serial = output.lock().clear_serial;
        assert_windows(&output.lock(), 0, old_clear_serial, 0, 0.0, scenario);

        let observed_at = state.playback_observation_instant_anchor
            + Duration::from_nanos((CONSUMED as u64 * 1_000_000_000).div_ceil(u64::from(RATE)));
        let consumed = output.lock().consume(CONSUMED, observed_at);
        for (frame, actual) in consumed.iter().enumerate() {
            assert_eq!(
                *actual,
                native_sample(scenario, frame),
                "actual original software FIFO consumption"
            );
        }
        let consumed_snapshot = poll_audio_playback_at_for_test(&mut state, observed_at, true)
            .expect("original App clock converter observes the synchronous software callback");
        assert_healthy(consumed_snapshot, generation);
        assert_eq!(
            consumed_snapshot.output.unwrap().active_callback_consumed_frames,
            CONSUMED as u64
        );
        assert_eq!(
            consumed_snapshot.output.unwrap().callback_consumed_frames,
            (INACTIVE_CALLBACK_FRAMES + CONSUMED) as u64
        );
        // The fixed partial callback is smaller than either indivisible chunk.
        // (watermark - 137) + chunk > watermark, so no refill can be admitted.
        assert_eq!(consumed_snapshot.in_flight, 0);
        assert_eq!(
            state.playback_clock_master(),
            Some(mondrian_playback::ClockMaster::AudioDevice)
        );
        let before_engine = state.playback_engine.snapshot();
        let authority = state
            .authoritative_audio_anchor(
                &state.playback_engine,
                state.playback_engine.monotonic_high_water(),
                "fader test",
            )
            .unwrap();
        assert_eq!(authority.sample(), CONSUMED as i64);
        assert!(authority.sample() > 0 && authority.sample() < warmed.next_start_sample);
        assert_eq!(output.lock().queue.len(), scenario.watermark - CONSUMED);
        assert_heads(
            &cursor_facts(&state),
            generation,
            scenario.watermark as i64,
            scenario,
        );
        let before_facts = cursor_facts(&state);
        let before_author = state.project_author_generation();
        let before_history = state.authoring_history().unwrap().diagnostics();
        let old_renderer = state.native_audio_renderer.as_ref().unwrap().clone();
        let verifier = old_renderer.upgrade().unwrap().owner_installation_verifier().unwrap();
        let old_claim = verifier.current_claim().unwrap();
        let clears_before = output.lock().cleared.len();
        assert!(verifier.validate(&old_claim).is_ok());

        // Real author actions sample Instant::now. Let wall time catch up to
        // this already-observed callback point (at most the fixed ~3ms), then
        // preserve any legitimate postcommit progression instead of pinning137.
        std::thread::sleep(observed_at.saturating_duration_since(Instant::now()));
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::EditChannelStrip(
                    mondrian_timeline::AudioChannelStripEditRequest {
                        owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput {
                            output_id,
                        },
                        edit: mondrian_timeline::AudioChannelStripEdit::SetFaderDb { value: -6.0 },
                    },
                ))
                .into_external_action(),
            )
            .expect("real Product Host fader transaction");
        assert_eq!(state.project_author_generation(), before_author + 1);
        assert_eq!(
            state.authoring_history().unwrap().diagnostics().undo_entries,
            before_history.undo_entries + 1
        );
        assert_eq!(
            state.active_sequence().unwrap().audio_program.outputs[0].strip.fader_db,
            -6.0
        );
        let after_engine = state.playback_engine.snapshot();
        assert_eq!(after_engine.epoch, before_engine.epoch);
        assert_eq!(after_engine.rate, before_engine.rate);
        assert_eq!(
            after_engine.state,
            mondrian_playback::TransportState::Playing
        );
        // The real author transaction synchronizes Engine at Instant::now.
        // That legal phase advance is independent of the old DSP/queue head.
        let post_authority = state
            .authoritative_audio_anchor(
                &state.playback_engine,
                state.playback_engine.monotonic_high_water(),
                "postcommit fader test",
            )
            .unwrap();
        assert!(post_authority.sample() >= authority.sample());
        assert!(post_authority
            .sample()
            .checked_add(scenario.watermark as i64)
            .is_some_and(|end| end <= SOURCE_FRAMES as i64));
        let changed = state.audio_playback_snapshot();
        assert_healthy(changed, generation + 1);
        assert_eq!(
            changed.next_start_sample,
            post_authority.sample(),
            "fresh Source uses exact postcommit Engine authority"
        );
        assert_eq!(changed.in_flight, 0);
        assert!(verifier.validate(&old_claim).is_err());
        {
            let captured = output.lock();
            assert_eq!(captured.cleared.len(), clears_before + 1);
            let discard = captured.cleared.last().unwrap();
            assert_eq!(discard.clear_serial, old_clear_serial);
            assert_eq!(
                discard.frames,
                scenario.watermark - CONSUMED,
                "retire all old unconsumed PCM"
            );
            assert!(captured.queue.is_empty());
            assert_eq!(captured.activation_discarded, 0);
            assert!(!captured.overflow);
        }
        // prepare's first clear retired the old queue. The original Media poll
        // must acknowledge quiescence and clear racing old PCM once more before
        // admitting the new windows; that clear is not another audio generation.
        let (new_windows_start, pending_clear_count, pending_clear_serial) = {
            let capture = output.lock();
            (
                capture.windows.len(),
                capture.cleared.len(),
                capture.clear_serial,
            )
        };
        let continued = fill_to_watermark(&mut state, generation + 1, scenario);
        assert_eq!(continued.media_anchor, Some(post_authority));
        assert_eq!(
            continued.next_start_sample,
            post_authority.sample() + scenario.watermark as i64
        );
        let after_facts = cursor_facts(&state);
        assert_heads(
            &after_facts,
            generation + 1,
            post_authority.sample() + scenario.watermark as i64,
            scenario,
        );
        eprintln!(
            "mode{} finite continuity: old={:?}; new={:?}",
            scenario.mode, before_facts.session, after_facts.session
        );
        let history_failure;
        {
            let capture = output.lock();
            assert_eq!(capture.cleared.len(), pending_clear_count + 1);
            let acknowledgement_clear = capture.cleared[pending_clear_count];
            assert_eq!(acknowledgement_clear.clear_serial, pending_clear_serial);
            assert_eq!(
                acknowledgement_clear.frames, 0,
                "no new PCM may be discarded before admission"
            );
            assert_eq!(capture.clear_serial, pending_clear_serial + 1);
            // Inspect the entire newly captured suffix, not a filtered latest
            // group that could hide an earlier discarded or extra window.
            let new_windows = &capture.windows[new_windows_start..];
            assert_eq!(new_windows.len(), scenario.watermark / scenario.chunk);
            assert!(new_windows.iter().all(|window| window.clear_serial == capture.clear_serial));
            assert_eq!(
                new_windows.iter().map(|window| window.samples.len()).sum::<usize>(),
                scenario.watermark * 2
            );
            assert_window_shape(&capture, new_windows_start, capture.clear_serial, scenario);
            history_failure = if scenario.mode == 12 {
                echo_history_discrepancy(
                    &capture,
                    new_windows_start,
                    post_authority.sample(),
                    warmed.next_start_sample,
                    scenario,
                )
            } else {
                assert_windows(
                    &capture,
                    new_windows_start,
                    capture.clear_serial,
                    post_authority.sample(),
                    -6.0,
                    scenario,
                );
                None
            };
        }
        assert_eq!(output.lock().snapshot.active_callback_consumed_frames, 0);
        assert_eq!(
            output.lock().snapshot.callback_consumed_frames,
            (INACTIVE_CALLBACK_FRAMES + CONSUMED) as u64
        );
        assert_eq!(output.lock().activation_discarded, 0);
        // Identity change and the five-byte configuration are not a diagnosis
        // of a ring/reset cause. All ordinary PCM and control checks precede W.
        assert!(
            history_failure.is_none(),
            "HISTORY_DISCONTINUITY: {}",
            history_failure.unwrap_or_default()
        );
    }));

    let deadline = Instant::now() + Duration::from_secs(2);
    let pause = state.pause();
    let audio = state.shutdown_audio_playback_until(deadline);
    let cleanup = state.shutdown_for_endurance(deadline); // consumes AppState
    assert!(
        pause.is_ok() && audio.all_workers_terminated() && cleanup.all_resources_released(),
        "complete finite cleanup: pause={pause:?}, audio={audio:?}, app={cleanup:?}"
    );
    if let Err(panic) = target {
        std::panic::resume_unwind(panic);
    }
}
