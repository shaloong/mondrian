//! Deep realtime Audio Playback scheduling Module.

use crate::{
    AudioBuffer, RealtimeAudioOutputEvent, RealtimeAudioOutputManager, RealtimeAudioOutputSnapshot,
};
use mondrian_core::{FramePosition, Rational};
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;
use thiserror::Error;

/// Versioned scheduling policy for realtime Audio Playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPlaybackConfig {
    /// Output sample rate used by render and device Adapters.
    pub sample_rate: u32,
    /// Interleaved output channel count.
    pub channels: u8,
    /// Frames in each independently rendered PCM window.
    pub chunk_frames: usize,
    /// Queued frames required before callback consumption starts.
    pub preroll_frames: usize,
    /// Desired queued plus in-flight frames during playback.
    pub high_watermark_frames: usize,
    /// Maximum current-generation render windows admitted at once.
    pub max_in_flight: usize,
    /// Missing active-consumption frames required to enter underrun recovery.
    pub underrun_recovery_threshold_frames: u64,
}

impl AudioPlaybackConfig {
    /// Product defaults: 48 kHz stereo, 80 ms chunks, 120 ms preroll, and 460 ms high watermark.
    pub const fn product_default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            chunk_frames: 3_840,
            preroll_frames: 5_760,
            high_watermark_frames: 22_080,
            max_in_flight: 8,
            underrun_recovery_threshold_frames: 960,
        }
    }
}

/// Invalid Audio Playback scheduling policy.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlaybackConfigError {
    /// Sample rate, channels, frame budgets, and in-flight capacity must be positive.
    #[error("audio playback configuration values must be positive")]
    ZeroValue,
    /// Preroll must fit inside the high watermark.
    #[error("audio playback preroll must not exceed the high watermark")]
    PrerollExceedsHighWatermark,
}

/// Exact PCM window requested at the render Adapter Seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPcmRenderRequest {
    /// First timeline-media sample frame in the window.
    pub start_sample: i64,
    /// Exact number of output frames required.
    pub frame_count: usize,
    /// Required output sample rate.
    pub sample_rate: u32,
    /// Required interleaved output channel count.
    pub channels: u8,
}

/// Adapter Interface used by Audio Playback to render timeline PCM.
///
/// Implementations may decode and mix, but must return exactly the requested
/// rate, channels, and frame count. Errors become same-duration silence plus
/// structured evidence so queued media position never shifts.
pub trait AudioPcmRenderer: Send + Sync + 'static {
    /// Render one exact timeline-media window.
    fn render(&self, request: AudioPcmRenderRequest) -> mondrian_core::Result<AudioBuffer>;
}

/// Transport permission presented to the Audio Playback Module on each poll.
///
/// The variants deliberately separate filling PCM from allowing the output
/// callback to consume it. In particular, video/startup `Priming` maps to
/// [`Self::Preroll`], so a slow first frame cannot start audio early and then
/// force a generation-resetting phase correction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlaybackMode {
    /// Do not schedule new PCM and keep device consumption disabled.
    Idle,
    /// Render and queue PCM, but do not permit device consumption yet.
    Preroll,
    /// Render PCM and permit activation once the preroll watermark is met.
    Consume,
}

impl AudioPlaybackMode {
    const fn renders_pcm(self) -> bool {
        matches!(self, Self::Preroll | Self::Consume)
    }

    const fn permits_consumption(self) -> bool {
        matches!(self, Self::Consume)
    }
}

/// Runtime Audio Playback condition exposed to callers and headless harnesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlaybackState {
    /// No concrete output stream is currently available.
    DeviceUnavailable,
    /// Output exists but transport is not consuming PCM.
    Idle,
    /// Playback has no timeline PCM Adapter configured.
    WaitingForSource,
    /// Current generation is filling preroll or is ready but not yet permitted to consume.
    Prerolling,
    /// Sustained underrun forced a Synthetic handoff and fresh preroll.
    Recovering,
    /// Callback consumption is active for a preroll-qualified generation.
    Active,
}

/// Structured lifecycle and render evidence emitted by one poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioPlaybackEvent {
    /// A concrete output stream opened and the render generation was reset.
    DeviceOpened { stream_generation: u64 },
    /// A concrete output stream was lost.
    DeviceLost { stream_generation: u64 },
    /// One open attempt failed and will be retried.
    DeviceOpenFailed {
        retry_after: Duration,
        reason: String,
    },
    /// A render failed or violated its PCM contract; exact-duration silence was queued.
    RenderSubstitutedWithSilence {
        generation: u64,
        start_sample: i64,
        reason: String,
    },
    /// New callback starvation was observed but may remain below recovery policy.
    UnderrunObserved {
        stream_generation: u64,
        delta_frames: u64,
        interval_total_frames: u64,
    },
    /// Missing frames reached policy; output was deactivated and reprime began.
    UnderrunRecoveryStarted {
        stream_generation: u64,
        missing_frames: u64,
        threshold_frames: u64,
        final_output: RealtimeAudioOutputSnapshot,
        final_media_anchor: FramePosition,
    },
}

/// Immutable Audio Playback state after one non-blocking poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPlaybackSnapshot {
    /// Current render generation.
    pub generation: u64,
    /// Current-generation windows executing or queued on the render worker.
    pub in_flight: usize,
    /// First sample frame not yet admitted to the render worker.
    pub next_start_sample: i64,
    /// Exact media time corresponding to active-consumption frame zero.
    pub media_anchor: Option<FramePosition>,
    /// Whether the current generation has met the activation preroll requirement.
    /// This does not imply that device consumption is currently permitted or active.
    pub activation_preroll_satisfied: bool,
    /// Current high-level Audio Playback condition.
    pub state: AudioPlaybackState,
    /// Concrete output callback evidence, when a device exists.
    pub output: Option<RealtimeAudioOutputSnapshot>,
    /// Current-generation render failures replaced by exact-duration silence.
    pub render_substitution_count: u64,
    /// Old-generation completions discarded before reaching the output queue.
    pub stale_completion_count: u64,
    /// Queued render windows canceled synchronously by generation invalidation.
    pub canceled_render_count: u64,
    /// Missing frames accumulated in the current active interval.
    pub active_interval_underrun_frames: u64,
    /// Number of sustained-underrun reprime cycles.
    pub underrun_recovery_count: u64,
}

/// Result of advancing Audio Playback without blocking the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPlaybackPoll {
    /// State after lifecycle, completion, scheduling, and activation work.
    pub snapshot: AudioPlaybackSnapshot,
    /// Ordered evidence observed during this poll.
    pub events: Vec<AudioPlaybackEvent>,
}

struct RenderWork {
    generation: u64,
    request: AudioPcmRenderRequest,
    renderer: Arc<dyn AudioPcmRenderer>,
}

struct RenderCompletion {
    generation: u64,
    request: AudioPcmRenderRequest,
    result: mondrian_core::Result<AudioBuffer>,
}

struct RenderQueueState {
    pending: VecDeque<RenderWork>,
    stopped: bool,
}

struct RenderWorkQueue {
    state: Mutex<RenderQueueState>,
    wake: Condvar,
    capacity: usize,
}

impl RenderWorkQueue {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(RenderQueueState {
                pending: VecDeque::with_capacity(capacity),
                stopped: false,
            }),
            wake: Condvar::new(),
            capacity,
        }
    }

    fn push(&self, work: RenderWork) -> Result<(), RenderWork> {
        let mut state = self.state.lock();
        if state.stopped || state.pending.len() >= self.capacity {
            return Err(work);
        }
        state.pending.push_back(work);
        self.wake.notify_one();
        Ok(())
    }

    fn pop(&self) -> Option<RenderWork> {
        let mut state = self.state.lock();
        loop {
            if let Some(work) = state.pending.pop_front() {
                return Some(work);
            }
            if state.stopped {
                return None;
            }
            self.wake.wait(&mut state);
        }
    }

    fn clear_pending(&self) -> usize {
        let mut state = self.state.lock();
        let count = state.pending.len();
        state.pending.clear();
        count
    }

    fn stop(&self) {
        let mut state = self.state.lock();
        state.stopped = true;
        state.pending.clear();
        self.wake.notify_all();
    }
}

trait AudioOutputAdapter {
    fn poll(&mut self) -> Option<RealtimeAudioOutputEvent>;
    fn enqueue(&self, buffer: &AudioBuffer);
    fn clear(&self);
    fn set_active(&self, active: bool);
    fn buffered_frames(&self) -> usize;
    fn snapshot(&self) -> Option<RealtimeAudioOutputSnapshot>;
}

impl AudioOutputAdapter for RealtimeAudioOutputManager {
    fn poll(&mut self) -> Option<RealtimeAudioOutputEvent> {
        RealtimeAudioOutputManager::poll(self)
    }

    fn enqueue(&self, buffer: &AudioBuffer) {
        RealtimeAudioOutputManager::enqueue(self, buffer);
    }

    fn clear(&self) {
        RealtimeAudioOutputManager::clear(self);
    }

    fn set_active(&self, active: bool) {
        RealtimeAudioOutputManager::set_active(self, active);
    }

    fn buffered_frames(&self) -> usize {
        RealtimeAudioOutputManager::buffered_frames(self)
    }

    fn snapshot(&self) -> Option<RealtimeAudioOutputSnapshot> {
        RealtimeAudioOutputManager::snapshot(self)
    }
}

/// Deep Module owning realtime output, render worker, generations, watermarks, and preroll.
pub struct AudioPlayback {
    config: AudioPlaybackConfig,
    output: Box<dyn AudioOutputAdapter>,
    render_queue: Arc<RenderWorkQueue>,
    completion_rx: mpsc::Receiver<RenderCompletion>,
    renderer: Option<Arc<dyn AudioPcmRenderer>>,
    generation: u64,
    in_flight: usize,
    next_start_sample: i64,
    media_anchor: Option<FramePosition>,
    activation_preroll_satisfied: bool,
    render_substitution_count: u64,
    stale_completion_count: u64,
    canceled_render_count: u64,
    underrun_baseline_frames: u64,
    last_underrun_frames: u64,
    underrun_recovery_count: u64,
    recovering_from_underrun: bool,
}

impl AudioPlayback {
    /// Construct Audio Playback with the validated product policy.
    pub fn product_default() -> Self {
        let config = AudioPlaybackConfig::product_default();
        Self::with_output(
            config,
            Box::new(RealtimeAudioOutputManager::new(
                config.sample_rate,
                config.channels,
            )),
        )
    }

    /// Construct production Audio Playback with a dedicated CPAL lifecycle thread and render worker.
    pub fn new(config: AudioPlaybackConfig) -> Result<Self, AudioPlaybackConfigError> {
        validate_config(config)?;
        let output = RealtimeAudioOutputManager::new(config.sample_rate, config.channels);
        Ok(Self::with_output(config, Box::new(output)))
    }

    fn with_output(config: AudioPlaybackConfig, output: Box<dyn AudioOutputAdapter>) -> Self {
        let render_queue = Arc::new(RenderWorkQueue::new(config.max_in_flight));
        let worker_queue = Arc::clone(&render_queue);
        let (completion_tx, completion_rx) = mpsc::channel::<RenderCompletion>();
        let _ = thread::Builder::new().name("mondrian-audio-render".to_owned()).spawn(move || {
            while let Some(work) = worker_queue.pop() {
                let result = work.renderer.render(work.request);
                if completion_tx
                    .send(RenderCompletion {
                        generation: work.generation,
                        request: work.request,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            config,
            output,
            render_queue,
            completion_rx,
            renderer: None,
            generation: 1,
            in_flight: 0,
            next_start_sample: 0,
            media_anchor: None,
            activation_preroll_satisfied: false,
            render_substitution_count: 0,
            stale_completion_count: 0,
            canceled_render_count: 0,
            underrun_baseline_frames: 0,
            last_underrun_frames: 0,
            underrun_recovery_count: 0,
            recovering_from_underrun: false,
        }
    }

    /// Install one immutable timeline PCM Adapter and start a new generation at `anchor`.
    pub fn prepare(&mut self, anchor: FramePosition, renderer: Arc<dyn AudioPcmRenderer>) {
        self.renderer = Some(renderer);
        self.reprime(anchor);
    }

    /// Remove timeline PCM rendering and invalidate all outstanding work.
    pub fn clear_source(&mut self, anchor: FramePosition) {
        self.renderer = None;
        self.reprime(anchor);
    }

    /// Invalidate outstanding work and restart PCM scheduling at an exact timeline anchor.
    pub fn reprime(&mut self, anchor: FramePosition) {
        self.reprime_internal(anchor, false);
    }

    fn reprime_internal(&mut self, anchor: FramePosition, recovering_from_underrun: bool) {
        let start_sample = time_code_to_sample_frame(anchor, self.config.sample_rate);
        self.output.set_active(false);
        self.output.clear();
        self.canceled_render_count = self
            .canceled_render_count
            .saturating_add(self.render_queue.clear_pending() as u64);
        self.generation = self.generation.saturating_add(1);
        self.in_flight = 0;
        self.next_start_sample = start_sample;
        self.media_anchor = self.renderer.as_ref().map(|_| {
            FramePosition::new(
                start_sample,
                Rational::new(1, i64::from(self.config.sample_rate)),
            )
        });
        self.activation_preroll_satisfied = false;
        let underrun_frames = self.output.snapshot().map_or(0, |output| output.underrun_frames);
        self.underrun_baseline_frames = underrun_frames;
        self.last_underrun_frames = underrun_frames;
        self.recovering_from_underrun = recovering_from_underrun;
    }

    /// Poll lifecycle, completions, watermarks, and preroll without waiting on workers.
    pub fn poll(&mut self, mode: AudioPlaybackMode, position: FramePosition) -> AudioPlaybackPoll {
        let mut events = Vec::new();
        let should_poll_output =
            self.output.snapshot().is_some() || (mode.renders_pcm() && self.renderer.is_some());
        if should_poll_output {
            while let Some(event) = self.output.poll() {
                match event {
                    RealtimeAudioOutputEvent::Opened { stream_generation } => {
                        self.reprime(position);
                        events.push(AudioPlaybackEvent::DeviceOpened { stream_generation });
                    }
                    RealtimeAudioOutputEvent::Lost { stream_generation } => {
                        self.media_anchor = None;
                        self.activation_preroll_satisfied = false;
                        self.underrun_baseline_frames = 0;
                        self.last_underrun_frames = 0;
                        self.recovering_from_underrun = false;
                        events.push(AudioPlaybackEvent::DeviceLost { stream_generation });
                    }
                    RealtimeAudioOutputEvent::OpenFailed { retry_after, reason } => {
                        events.push(AudioPlaybackEvent::DeviceOpenFailed { retry_after, reason });
                    }
                }
            }
        }

        while let Ok(completion) = self.completion_rx.try_recv() {
            if completion.generation != self.generation {
                self.stale_completion_count = self.stale_completion_count.saturating_add(1);
                continue;
            }
            self.in_flight = self.in_flight.saturating_sub(1);
            match validate_rendered_buffer(completion.request, completion.result) {
                Ok(buffer) => self.output.enqueue(&buffer),
                Err(reason) => {
                    let silence = AudioBuffer::silent(
                        completion.request.sample_rate,
                        completion.request.channels,
                        completion.request.frame_count,
                    );
                    self.output.enqueue(&silence);
                    self.render_substitution_count =
                        self.render_substitution_count.saturating_add(1);
                    events.push(AudioPlaybackEvent::RenderSubstitutedWithSilence {
                        generation: completion.generation,
                        start_sample: completion.request.start_sample,
                        reason,
                    });
                }
            }
        }

        if mode == AudioPlaybackMode::Idle {
            self.output.set_active(false);
            self.output.clear();
            self.activation_preroll_satisfied = false;
            self.recovering_from_underrun = false;
            return AudioPlaybackPoll { snapshot: self.snapshot(mode), events };
        }

        if !mode.permits_consumption() {
            self.output.set_active(false);
        }

        let active_output = if mode.permits_consumption() {
            self.output.snapshot().filter(|output| output.active)
        } else {
            None
        };
        if let Some(output) = active_output {
            if output.underrun_frames > self.last_underrun_frames {
                let delta_frames = output.underrun_frames - self.last_underrun_frames;
                let interval_total_frames =
                    output.underrun_frames.saturating_sub(self.underrun_baseline_frames);
                self.last_underrun_frames = output.underrun_frames;
                events.push(AudioPlaybackEvent::UnderrunObserved {
                    stream_generation: output.stream_generation,
                    delta_frames,
                    interval_total_frames,
                });
                if interval_total_frames >= self.config.underrun_recovery_threshold_frames {
                    let final_media_anchor = self.media_anchor.unwrap_or(position);
                    events.push(AudioPlaybackEvent::UnderrunRecoveryStarted {
                        stream_generation: output.stream_generation,
                        missing_frames: interval_total_frames,
                        threshold_frames: self.config.underrun_recovery_threshold_frames,
                        final_output: output,
                        final_media_anchor,
                    });
                    self.underrun_recovery_count = self.underrun_recovery_count.saturating_add(1);
                    self.reprime_internal(position, true);
                }
            }
        }

        if let (Some(renderer), Some(_)) = (self.renderer.as_ref(), self.output.snapshot()) {
            while self
                .output
                .buffered_frames()
                .saturating_add(self.in_flight.saturating_mul(self.config.chunk_frames))
                < self.config.high_watermark_frames
                && self.in_flight < self.config.max_in_flight
            {
                let request = AudioPcmRenderRequest {
                    start_sample: self.next_start_sample,
                    frame_count: self.config.chunk_frames,
                    sample_rate: self.config.sample_rate,
                    channels: self.config.channels,
                };
                let work = RenderWork {
                    generation: self.generation,
                    request,
                    renderer: Arc::clone(renderer),
                };
                if self.render_queue.push(work).is_err() {
                    break;
                }
                self.in_flight = self.in_flight.saturating_add(1);
                self.next_start_sample = self
                    .next_start_sample
                    .saturating_add(self.config.chunk_frames.min(i64::MAX as usize) as i64);
            }
            if self.output.buffered_frames() >= self.config.preroll_frames {
                self.activation_preroll_satisfied = true;
                if mode.permits_consumption()
                    && self.output.snapshot().is_some_and(|snapshot| !snapshot.active)
                {
                    self.output.set_active(true);
                    self.recovering_from_underrun = false;
                }
            }
        }

        AudioPlaybackPoll { snapshot: self.snapshot(mode), events }
    }

    /// Return immutable state without advancing workers or lifecycle.
    pub fn snapshot(&self, mode: AudioPlaybackMode) -> AudioPlaybackSnapshot {
        let output = self.output.snapshot();
        let state = match output {
            None => AudioPlaybackState::DeviceUnavailable,
            Some(_) if mode == AudioPlaybackMode::Idle => AudioPlaybackState::Idle,
            Some(_) if self.renderer.is_none() => AudioPlaybackState::WaitingForSource,
            Some(_) if self.recovering_from_underrun => AudioPlaybackState::Recovering,
            Some(snapshot) if snapshot.active => AudioPlaybackState::Active,
            Some(_) => AudioPlaybackState::Prerolling,
        };
        AudioPlaybackSnapshot {
            generation: self.generation,
            in_flight: self.in_flight,
            next_start_sample: self.next_start_sample,
            media_anchor: self.media_anchor,
            activation_preroll_satisfied: self.activation_preroll_satisfied,
            state,
            output,
            render_substitution_count: self.render_substitution_count,
            stale_completion_count: self.stale_completion_count,
            canceled_render_count: self.canceled_render_count,
            active_interval_underrun_frames: output.map_or(0, |output| {
                output.underrun_frames.saturating_sub(self.underrun_baseline_frames)
            }),
            underrun_recovery_count: self.underrun_recovery_count,
        }
    }
}

impl Drop for AudioPlayback {
    fn drop(&mut self) {
        self.render_queue.stop();
    }
}

fn validate_config(config: AudioPlaybackConfig) -> Result<(), AudioPlaybackConfigError> {
    if config.sample_rate == 0
        || config.channels == 0
        || config.chunk_frames == 0
        || config.preroll_frames == 0
        || config.high_watermark_frames == 0
        || config.max_in_flight == 0
        || config.underrun_recovery_threshold_frames == 0
    {
        return Err(AudioPlaybackConfigError::ZeroValue);
    }
    if config.preroll_frames > config.high_watermark_frames {
        return Err(AudioPlaybackConfigError::PrerollExceedsHighWatermark);
    }
    Ok(())
}

fn time_code_to_sample_frame(anchor: FramePosition, sample_rate: u32) -> i64 {
    if anchor.time_base.num <= 0 || anchor.time_base.den <= 0 {
        return 0;
    }
    let numerator = (anchor.frame as i128)
        .saturating_mul(anchor.time_base.num as i128)
        .saturating_mul(sample_rate as i128);
    let denominator = anchor.time_base.den as i128;
    let rounded = if numerator >= 0 {
        numerator.saturating_add(denominator / 2) / denominator
    } else {
        numerator.saturating_sub(denominator / 2) / denominator
    };
    rounded.clamp(0, i64::MAX as i128) as i64
}

fn validate_rendered_buffer(
    request: AudioPcmRenderRequest,
    result: mondrian_core::Result<AudioBuffer>,
) -> Result<AudioBuffer, String> {
    let buffer = result.map_err(|error| error.to_string())?;
    if buffer.sample_rate != request.sample_rate {
        return Err(format!(
            "rendered sample rate {} does not match requested {}",
            buffer.sample_rate, request.sample_rate
        ));
    }
    if buffer.channels != request.channels {
        return Err(format!(
            "rendered channel count {} does not match requested {}",
            buffer.channels, request.channels
        ));
    }
    if buffer.frame_count() != request.frame_count {
        return Err(format!(
            "rendered frame count {} does not match requested {}",
            buffer.frame_count(),
            request.frame_count
        ));
    }
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    #[derive(Default)]
    struct FakeOutputState {
        events: VecDeque<RealtimeAudioOutputEvent>,
        snapshot: Option<RealtimeAudioOutputSnapshot>,
        queued_frames: usize,
    }

    struct FakeOutput {
        state: Arc<Mutex<FakeOutputState>>,
    }

    impl AudioOutputAdapter for FakeOutput {
        fn poll(&mut self) -> Option<RealtimeAudioOutputEvent> {
            self.state.lock().events.pop_front()
        }

        fn enqueue(&self, buffer: &AudioBuffer) {
            let mut state = self.state.lock();
            state.queued_frames = state.queued_frames.saturating_add(buffer.frame_count());
            let queued_frames = state.queued_frames;
            if let Some(snapshot) = state.snapshot.as_mut() {
                snapshot.buffered_frames = queued_frames;
            }
        }

        fn clear(&self) {
            let mut state = self.state.lock();
            state.queued_frames = 0;
            if let Some(snapshot) = state.snapshot.as_mut() {
                snapshot.buffered_frames = 0;
            }
        }

        fn set_active(&self, active: bool) {
            if let Some(snapshot) = self.state.lock().snapshot.as_mut() {
                snapshot.active = active;
            }
        }

        fn buffered_frames(&self) -> usize {
            self.state.lock().queued_frames
        }

        fn snapshot(&self) -> Option<RealtimeAudioOutputSnapshot> {
            self.state.lock().snapshot
        }
    }

    struct RecordingRenderer {
        requests: Arc<Mutex<Vec<AudioPcmRenderRequest>>>,
        wrong_frame_count: bool,
    }

    struct GateRenderer {
        entered: Arc<AtomicBool>,
        released: Arc<AtomicBool>,
    }

    impl AudioPcmRenderer for GateRenderer {
        fn render(&self, request: AudioPcmRenderRequest) -> mondrian_core::Result<AudioBuffer> {
            self.entered.store(true, Ordering::Release);
            while !self.released.load(Ordering::Acquire) {
                thread::yield_now();
            }
            Ok(AudioBuffer::silent(
                request.sample_rate,
                request.channels,
                request.frame_count,
            ))
        }
    }

    impl AudioPcmRenderer for RecordingRenderer {
        fn render(&self, request: AudioPcmRenderRequest) -> mondrian_core::Result<AudioBuffer> {
            self.requests.lock().push(request);
            let frames = if self.wrong_frame_count {
                request.frame_count.saturating_sub(1)
            } else {
                request.frame_count
            };
            Ok(AudioBuffer::silent(
                request.sample_rate,
                request.channels,
                frames,
            ))
        }
    }

    fn test_config() -> AudioPlaybackConfig {
        AudioPlaybackConfig {
            sample_rate: 1_000,
            channels: 2,
            chunk_frames: 10,
            preroll_frames: 20,
            high_watermark_frames: 30,
            max_in_flight: 3,
            underrun_recovery_threshold_frames: 10,
        }
    }

    fn fake_output() -> (Box<dyn AudioOutputAdapter>, Arc<Mutex<FakeOutputState>>) {
        let snapshot = RealtimeAudioOutputSnapshot {
            stream_generation: 4,
            sample_rate: 1_000,
            channels: 2,
            callback_consumed_frames: 0,
            active_callback_consumed_frames: 0,
            active_duration: None,
            callback_count: 0,
            underrun_frames: 0,
            last_callback_frames: 10,
            last_callback_age: None,
            buffered_frames: 0,
            stream_failed: false,
            active: false,
        };
        let state = Arc::new(Mutex::new(FakeOutputState {
            events: VecDeque::from([RealtimeAudioOutputEvent::Opened { stream_generation: 4 }]),
            snapshot: Some(snapshot),
            queued_frames: 0,
        }));
        (Box::new(FakeOutput { state: Arc::clone(&state) }), state)
    }

    fn poll_until_settled(
        playback: &mut AudioPlayback,
        position: FramePosition,
    ) -> Vec<AudioPlaybackEvent> {
        poll_until_settled_in_mode(playback, position, AudioPlaybackMode::Consume)
    }

    fn poll_until_settled_in_mode(
        playback: &mut AudioPlayback,
        position: FramePosition,
        mode: AudioPlaybackMode,
    ) -> Vec<AudioPlaybackEvent> {
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let poll = playback.poll(mode, position);
            events.extend(poll.events);
            if poll.snapshot.in_flight == 0
                && poll.snapshot.output.is_some_and(|output| output.buffered_frames >= 30)
            {
                return events;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("audio render worker did not settle");
    }

    #[test]
    fn headless_adapter_observes_integer_windows_watermark_and_preroll_activation() {
        let (output, _) = fake_output();
        let mut playback = AudioPlayback::with_output(test_config(), output);
        let requests = Arc::new(Mutex::new(Vec::new()));
        playback.prepare(
            FramePosition::new(1, Rational::new(1, 25)),
            Arc::new(RecordingRenderer {
                requests: Arc::clone(&requests),
                wrong_frame_count: false,
            }),
        );

        let events = poll_until_settled(&mut playback, FramePosition::new(1, Rational::new(1, 25)));
        let snapshot = playback.snapshot(AudioPlaybackMode::Consume);

        assert!(events.contains(&AudioPlaybackEvent::DeviceOpened { stream_generation: 4 }));
        assert_eq!(
            requests.lock().iter().map(|request| request.start_sample).collect::<Vec<_>>(),
            vec![40, 50, 60]
        );
        assert_eq!(snapshot.state, AudioPlaybackState::Active);
        assert!(snapshot.activation_preroll_satisfied);
        assert_eq!(
            snapshot.media_anchor,
            Some(FramePosition::new(40, Rational::new(1, 1_000)))
        );
    }

    #[test]
    fn preroll_fills_pcm_without_consuming_or_resetting_generation() {
        let (output, state) = fake_output();
        let mut playback = AudioPlayback::with_output(test_config(), output);
        playback.prepare(
            FramePosition::new(0, Rational::new(1, 25)),
            Arc::new(RecordingRenderer {
                requests: Arc::new(Mutex::new(Vec::new())),
                wrong_frame_count: false,
            }),
        );

        let events = poll_until_settled_in_mode(
            &mut playback,
            FramePosition::new(0, Rational::new(1, 25)),
            AudioPlaybackMode::Preroll,
        );
        let primed = playback.snapshot(AudioPlaybackMode::Preroll);
        let primed_generation = primed.generation;
        let primed_frames = state.lock().queued_frames;

        assert!(events.contains(&AudioPlaybackEvent::DeviceOpened { stream_generation: 4 }));
        assert_eq!(primed.state, AudioPlaybackState::Prerolling);
        assert!(primed.activation_preroll_satisfied);
        assert!(primed.output.is_some_and(|output| !output.active));
        assert_eq!(primed_frames, 30);

        let activated = playback
            .poll(
                AudioPlaybackMode::Consume,
                FramePosition::new(0, Rational::new(1, 25)),
            )
            .snapshot;

        assert_eq!(activated.generation, primed_generation);
        assert_eq!(activated.state, AudioPlaybackState::Active);
        assert!(activated.output.is_some_and(|output| output.active));
        assert_eq!(state.lock().queued_frames, primed_frames);
    }

    #[test]
    fn malformed_render_keeps_media_duration_with_silence_and_evidence() {
        let (output, state) = fake_output();
        let mut playback = AudioPlayback::with_output(test_config(), output);
        playback.prepare(
            FramePosition::new(0, Rational::new(1, 25)),
            Arc::new(RecordingRenderer {
                requests: Arc::new(Mutex::new(Vec::new())),
                wrong_frame_count: true,
            }),
        );

        let events = poll_until_settled(&mut playback, FramePosition::new(0, Rational::new(1, 25)));

        assert_eq!(state.lock().queued_frames, 30);
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).render_substitution_count,
            3
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    AudioPlaybackEvent::RenderSubstitutedWithSilence { .. }
                ))
                .count(),
            3
        );
    }

    #[test]
    fn invalid_policy_is_rejected_at_the_interface() {
        let mut config = test_config();
        config.preroll_frames = 31;
        assert_eq!(
            AudioPlayback::new(config).err(),
            Some(AudioPlaybackConfigError::PrerollExceedsHighWatermark)
        );
    }

    #[test]
    fn isolated_underrun_preserves_master_but_sustained_missing_frames_reprime() {
        let (output, state) = fake_output();
        let mut playback = AudioPlayback::with_output(test_config(), output);
        playback.prepare(
            FramePosition::new(0, Rational::new(1, 25)),
            Arc::new(RecordingRenderer {
                requests: Arc::new(Mutex::new(Vec::new())),
                wrong_frame_count: false,
            }),
        );
        poll_until_settled(&mut playback, FramePosition::new(0, Rational::new(1, 25)));

        state.lock().snapshot.as_mut().expect("fake output").underrun_frames = 4;
        let isolated = playback.poll(
            AudioPlaybackMode::Consume,
            FramePosition::new(0, Rational::new(1, 25)),
        );
        assert_eq!(isolated.snapshot.state, AudioPlaybackState::Active);
        assert_eq!(isolated.snapshot.underrun_recovery_count, 0);
        assert!(
            isolated.events.contains(&AudioPlaybackEvent::UnderrunObserved {
                stream_generation: 4,
                delta_frames: 4,
                interval_total_frames: 4,
            })
        );

        state.lock().snapshot.as_mut().expect("fake output").underrun_frames = 10;
        let recovering = playback.poll(
            AudioPlaybackMode::Consume,
            FramePosition::new(1, Rational::new(1, 25)),
        );
        assert_eq!(recovering.snapshot.state, AudioPlaybackState::Recovering);
        assert_eq!(recovering.snapshot.underrun_recovery_count, 1);
        assert!(recovering.events.iter().any(|event| matches!(
            event,
            AudioPlaybackEvent::UnderrunRecoveryStarted {
                stream_generation: 4,
                missing_frames: 10,
                threshold_frames: 10,
                final_output,
                final_media_anchor,
            } if final_output.active
                && final_output.underrun_frames == 10
                && *final_media_anchor == FramePosition::new(0, Rational::new(1, 1_000))
        )));
        assert!(recovering.snapshot.output.is_some_and(|output| !output.active));

        poll_until_settled(&mut playback, FramePosition::new(1, Rational::new(1, 25)));
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).state,
            AudioPlaybackState::Active
        );
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).active_interval_underrun_frames,
            0
        );
    }

    #[test]
    fn reprime_discards_old_generation_completion_before_output() {
        let (output, state) = fake_output();
        let mut config = test_config();
        config.preroll_frames = 10;
        let mut playback = AudioPlayback::with_output(config, output);
        let entered = Arc::new(AtomicBool::new(false));
        let released = Arc::new(AtomicBool::new(false));
        playback.prepare(
            FramePosition::new(0, Rational::new(1, 25)),
            Arc::new(GateRenderer {
                entered: Arc::clone(&entered),
                released: Arc::clone(&released),
            }),
        );
        playback.poll(
            AudioPlaybackMode::Consume,
            FramePosition::new(0, Rational::new(1, 25)),
        );
        let entered_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < entered_deadline {
            if entered.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(entered.load(Ordering::Acquire));

        let current_requests = Arc::new(Mutex::new(Vec::new()));
        playback.prepare(
            FramePosition::new(1, Rational::new(1, 25)),
            Arc::new(RecordingRenderer {
                requests: Arc::clone(&current_requests),
                wrong_frame_count: false,
            }),
        );
        released.store(true, Ordering::Release);

        let settled_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < settled_deadline {
            let snapshot = playback
                .poll(
                    AudioPlaybackMode::Consume,
                    FramePosition::new(1, Rational::new(1, 25)),
                )
                .snapshot;
            if snapshot.stale_completion_count == 1
                && snapshot.in_flight == 0
                && snapshot.output.is_some_and(|output| output.buffered_frames == 30)
            {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }

        let snapshot = playback.snapshot(AudioPlaybackMode::Consume);
        assert_eq!(snapshot.stale_completion_count, 1);
        assert_eq!(snapshot.canceled_render_count, 2);
        assert_eq!(state.lock().queued_frames, 30);
        assert_eq!(
            current_requests
                .lock()
                .iter()
                .map(|request| request.start_sample)
                .collect::<Vec<_>>(),
            vec![40, 50, 60]
        );
    }
}
