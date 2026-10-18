//! Finite owned mode0 delay/PDC continuity, using a deterministic software
//! callback driver. This does not qualify native DSP migration, exact create/
//! Enter call counts, the complete UI pump, arbitrary tails or a physical DAC.

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
const CHUNK: usize = 256;
const WATERMARK: usize = 8_192;
const CONSUMED: usize = 137;
const CAPTURE_WINDOWS: usize = 80;
const OUTPUT_CAPACITY: usize = 16_384;

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
    epoch: u64,
    samples: Vec<f32>,
}

#[derive(Clone, Copy, Debug)]
struct Discard {
    epoch: u64,
    frames: usize,
}

struct CaptureState {
    snapshot: RealtimeAudioOutputSnapshot,
    revision: u64,
    epoch: u64,
    queue: VecDeque<[f32; 2]>,
    windows: Vec<CapturedWindow>,
    cleared: Vec<Discard>,
    activation_discarded: usize,
    overflow: bool,
}

impl CaptureState {
    fn new() -> Self {
        Self {
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
            epoch: 0,
            queue: VecDeque::with_capacity(OUTPUT_CAPACITY),
            windows: Vec::with_capacity(CAPTURE_WINDOWS),
            cleared: Vec::with_capacity(8),
            activation_discarded: 0,
            overflow: false,
        }
    }

    // There is no background callback owner. A complete synchronous callback
    // removes real queued frames under the same mutex as deactivate/clear.
    // Its explicit test-clock cadence is not hardware timestamp evidence.
    fn consume(&mut self, frames: usize, observed_at: Instant) -> Vec<[f32; 2]> {
        assert!(self.snapshot.active);
        assert!(frames <= self.queue.len());
        let consumed = self.queue.drain(..frames).collect();
        self.snapshot.callback_consumed_frames += frames as u64;
        self.snapshot.active_callback_consumed_frames += frames as u64;
        self.snapshot.callback_count += 1;
        self.snapshot.last_callback_frames = frames as u32;
        self.snapshot.active_duration = Some(Duration::from_nanos(
            self.snapshot.active_callback_consumed_frames * 1_000_000_000 / u64::from(RATE),
        ));
        self.snapshot.last_callback_age = Some(Duration::ZERO);
        self.snapshot.captured_at = observed_at;
        self.snapshot.buffered_frames = self.queue.len();
        consumed
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
        if state.windows.len() == CAPTURE_WINDOWS
            || frames > CHUNK
            || frames > OUTPUT_CAPACITY - state.queue.len()
        {
            state.overflow = true;
            return Err(RealtimeAudioOutputEnqueueError::InsufficientCapacity {
                required_samples: buffer.samples.len(),
                available_samples: 0,
            });
        }
        let epoch = state.epoch;
        state.windows.push(CapturedWindow { epoch, samples: buffer.samples.clone() });
        state
            .queue
            .extend(buffer.samples.chunks_exact(2).map(|pair| [pair[0], pair[1]]));
        state.snapshot.buffered_frames = state.queue.len();
        Ok(())
    }

    fn clear(&mut self) {
        let mut state = self.0.lock();
        let discard = Discard { epoch: state.epoch, frames: state.queue.len() };
        if state.cleared.len() == 8 {
            state.overflow = true;
        } else {
            state.cleared.push(discard);
        }
        state.queue.clear();
        state.snapshot.buffered_frames = 0;
        state.epoch += 1;
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
        state.snapshot.last_callback_frames = 0;
        state.snapshot.last_callback_age = None;
        state.snapshot.active = true;
        Ok(())
    }

    fn buffered_frames(&self) -> usize {
        self.0.lock().queue.len()
    }

    fn capacity_frames(&self) -> Option<usize> {
        Some(OUTPUT_CAPACITY)
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

fn assert_heads(facts: &CursorFacts, generation: u64, next_public: i64) {
    let session = facts.session.expect("one actual Session");
    assert_eq!(session.public_output_lookahead_frames, 3);
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
    assert_eq!(next_execution_sample.sample(), next_public + 3);
    let processor = facts.processor.expect("one actual native occurrence");
    assert_eq!(processor.identity, session.identity);
    let AudioProcessorCursorState::Active { epoch, next_input_signal_sample } = processor.state
    else {
        panic!("actual completed native processor: {processor:?}")
    };
    assert_eq!(epoch.get(), generation);
    assert_eq!(next_input_signal_sample.sample(), next_public + 3);
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

fn fill_to_watermark(state: &mut AppState, generation: u64) -> AudioPlaybackSnapshot {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let observed_at = state.playback_observation_instant_anchor;
        let snapshot = poll_audio_playback_at_for_test(state, observed_at, false)
            .expect("original deterministic App/Media poll");
        assert_healthy(snapshot, generation);
        if snapshot.in_flight == 0 && snapshot.output.unwrap().buffered_frames == WATERMARK {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "owned native PCM completion deadline: {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn assert_windows(state: &CaptureState, epoch: u64, start_sample: i64, fader_db: f64) {
    // mode0 uses main weight2 and saved gain.5; unconnected auxiliary inputs
    // are zero. Its latency3 is compensated by public lookahead3, so expected
    // public sample n is the original WAV[n], with no extra +/-3 shift.
    let gain = 10.0_f64.powf(fader_db / 20.0);
    let mut frames = 0;
    for window in state.windows.iter().filter(|window| window.epoch == epoch) {
        assert_eq!(window.samples.len(), CHUNK * 2);
        for pair in window.samples.chunks_exact(2) {
            let absolute = usize::try_from(start_sample + frames as i64).unwrap();
            assert!(
                absolute < SOURCE_FRAMES,
                "reference stays inside the real WAV"
            );
            let expected = input_sample(absolute);
            for channel in 0..2 {
                let expected = (f64::from(expected[channel]) * gain) as f32;
                assert!(
                    (pair[channel] - expected).abs() <= 1e-6,
                    "epoch{epoch} public{absolute} channel{channel}: {} != {expected}",
                    pair[channel]
                );
            }
            frames += 1;
        }
    }
    assert_eq!(
        frames, WATERMARK,
        "all original complete windows, including the first"
    );
    assert!(!state.overflow);
}

#[test]
#[ignore = "current normal Product and owned CLAP sidechain mode0 DLL; software callback only"]
fn native_clap_playing_host_fader_preserves_latency_compensated_pcm() {
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
    let mut opaque = vec![0];
    opaque.extend_from_slice(&0.5_f32.to_le_bytes());
    let instance = installed
        .create_instance(
            "org.mondrian.fixture.sidechain",
            reference_render_contract(),
            Some(opaque.clone()),
        )
        .expect("actual mode0 native author state");
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
    let output = Arc::new(Mutex::new(CaptureState::new()));
    state.audio_playback = AppAudioPlayback::Available(Box::new(
        AudioPlayback::with_output(
            AudioPlaybackConfig {
                chunk_frames: CHUNK,
                preroll_frames: CHUNK,
                high_watermark_frames: WATERMARK,
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
        let generation = state.audio_playback_snapshot().generation;
        let warmed = fill_to_watermark(&mut state, generation);
        assert_eq!(warmed.media_anchor.unwrap().sample(), 0);
        assert_eq!(warmed.next_start_sample, WATERMARK as i64);
        assert_heads(&cursor_facts(&state), generation, WATERMARK as i64);
        let old_epoch = output.lock().epoch;
        assert_windows(&output.lock(), old_epoch, 0, 0.0);

        let observed_at = state.playback_observation_instant_anchor
            + Duration::from_nanos((CONSUMED as u64 * 1_000_000_000).div_ceil(u64::from(RATE)));
        let consumed = output.lock().consume(CONSUMED, observed_at);
        for (frame, actual) in consumed.iter().enumerate() {
            assert_eq!(
                *actual,
                input_sample(frame),
                "actual original software FIFO consumption"
            );
        }
        let consumed_snapshot = poll_audio_playback_at_for_test(&mut state, observed_at, true)
            .expect("original App clock converter observes the synchronous software callback");
        assert_healthy(consumed_snapshot, generation);
        // 8055 queued + the next indivisible 256-frame admission exceeds
        // watermark8192. This partial callback cannot trigger a refill.
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
        assert_eq!(output.lock().queue.len(), WATERMARK - CONSUMED);
        assert_heads(&cursor_facts(&state), generation, WATERMARK as i64);
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
        assert!(post_authority.sample() + WATERMARK as i64 <= SOURCE_FRAMES as i64);
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
            assert_eq!(discard.epoch, old_epoch);
            assert_eq!(
                discard.frames,
                WATERMARK - CONSUMED,
                "retire all old unconsumed PCM"
            );
            assert!(captured.queue.is_empty());
            assert_eq!(captured.activation_discarded, 0);
            assert!(!captured.overflow);
        }
        let new_epoch = output.lock().epoch;
        let continued = fill_to_watermark(&mut state, generation + 1);
        assert_eq!(continued.media_anchor, Some(post_authority));
        assert_eq!(
            continued.next_start_sample,
            post_authority.sample() + WATERMARK as i64
        );
        let after_facts = cursor_facts(&state);
        assert_heads(
            &after_facts,
            generation + 1,
            post_authority.sample() + WATERMARK as i64,
        );
        eprintln!(
            "mode0 compensated continuity: old={:?}; new={:?}",
            before_facts.session, after_facts.session
        );
        assert_windows(&output.lock(), new_epoch, post_authority.sample(), -6.0);
        assert_eq!(output.lock().snapshot.active_callback_consumed_frames, 0);
        assert_eq!(
            output.lock().snapshot.callback_consumed_frames,
            CONSUMED as u64
        );
        assert_eq!(output.lock().activation_discarded, 0);
        // Identity change is an observation, not an exact create/Enter counter
        // or a failure oracle. mode0's whole finite history is PDC-recoverable.
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
