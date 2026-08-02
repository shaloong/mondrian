//! 音频缓冲区与混合器

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_queue::ArrayQueue;
use mondrian_core::{AudioChannelLayout, MondrianError, Result};
use parking_lot::Mutex;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;

/// 原始 PCM 音频缓冲区（f32 交错格式）
#[derive(Debug, Clone)]
pub struct AudioBuffer {
    /// 交错 PCM f32 样本 [L0, R0, L1, R1, ...]
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    /// Semantic layout and canonical interleaving order.
    pub channel_layout: AudioChannelLayout,
}

impl AudioBuffer {
    pub fn silent(sample_rate: u32, channel_layout: AudioChannelLayout, frames: usize) -> Self {
        Self {
            samples: vec![0.0; frames * channel_layout.channel_count()],
            sample_rate,
            channel_layout,
        }
    }

    /// Channel count derived from the buffer's sole semantic layout authority.
    pub const fn channel_count(&self) -> usize {
        self.channel_layout.channel_count()
    }

    /// 帧数（样本数 / 声道数）
    pub fn frame_count(&self) -> usize {
        self.samples.len() / self.channel_count()
    }

    /// 时长（秒）
    pub fn duration_secs(&self) -> f64 {
        self.frame_count() as f64 / self.sample_rate as f64
    }

    /// 就地增益调整（线性乘数）
    pub fn apply_gain(&mut self, gain: f32) {
        for s in &mut self.samples {
            *s *= gain;
        }
    }

    /// 混合另一个缓冲区（加法混合，无响度归一化）
    pub fn mix_from(&mut self, other: &Self, gain: f32) {
        let len = self.samples.len().min(other.samples.len());
        for i in 0..len {
            self.samples[i] += other.samples[i] * gain;
        }
    }

    pub fn slice_frames(&self, start_frame: usize, frame_count: usize) -> Self {
        let channels = self.channel_count();
        let total_frames = self.frame_count();
        if start_frame >= total_frames || frame_count == 0 {
            return Self::silent(self.sample_rate, self.channel_layout, 0);
        }

        let end_frame = (start_frame + frame_count).min(total_frames);
        let start = start_frame * channels;
        let end = end_frame * channels;

        Self {
            samples: self.samples[start..end].to_vec(),
            sample_rate: self.sample_rate,
            channel_layout: self.channel_layout,
        }
    }
}

pub(crate) struct RealtimeAudioOutput {
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    queue: Arc<ArrayQueue<f32>>,
    callback_control: Arc<RealtimeAudioCallbackControl>,
    activation_elapsed_ns: Arc<AtomicU64>,
    telemetry: Arc<RealtimeAudioOutputTelemetry>,
    snapshot_cache: Arc<Mutex<Option<RealtimeAudioOutputSnapshot>>>,
    _stream: cpal::Stream,
}

/// Sendable control/observation handle for a stream owned by its device thread.
pub(crate) struct RealtimeAudioOutputHandle {
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    queue: Arc<ArrayQueue<f32>>,
    callback_control: Arc<RealtimeAudioCallbackControl>,
    activation_elapsed_ns: Arc<AtomicU64>,
    telemetry: Arc<RealtimeAudioOutputTelemetry>,
    snapshot_cache: Arc<Mutex<Option<RealtimeAudioOutputSnapshot>>>,
}

/// Cloneable read-only evidence handle retained by the device worker after
/// the concrete CPAL stream is destroyed.
pub(crate) struct RealtimeAudioOutputObserver {
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    queue: Arc<ArrayQueue<f32>>,
    callback_control: Arc<RealtimeAudioCallbackControl>,
    activation_elapsed_ns: Arc<AtomicU64>,
    telemetry: Arc<RealtimeAudioOutputTelemetry>,
    snapshot_cache: Arc<Mutex<Option<RealtimeAudioOutputSnapshot>>>,
}

/// Identity of one callback-quiescence obligation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RealtimeAudioOutputQuiescenceToken {
    pub(crate) stream_generation: u64,
    pub(crate) revision: u64,
}

/// Checked failures at the realtime output control/queue boundary.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeAudioOutputControlError {
    /// No concrete output stream is currently installed.
    #[error("no concrete realtime output stream is available")]
    OutputUnavailable,
    /// A callback deactivation revision could not advance without reuse.
    #[error("callback quiescence revision exhausted for stream {stream_generation}")]
    QuiescenceRevisionExhausted { stream_generation: u64 },
    /// A quiescence token belongs to a different concrete stream.
    #[error(
        "callback quiescence token stream {token_generation} does not match current stream {current_generation}"
    )]
    StreamGenerationMismatch {
        token_generation: u64,
        current_generation: u64,
    },
    /// A stale token cannot activate a newer callback-control revision.
    #[error("callback quiescence token revision {token_revision} is stale; current revision is {current_revision}")]
    QuiescenceRevisionMismatch {
        token_revision: u64,
        current_revision: u64,
    },
    /// Activation was attempted before every active callback block completed.
    #[error("callback quiescence revision {revision} has not been confirmed")]
    CallbackNotQuiescent { revision: u64 },
    /// A frame-to-interleaved-sample calculation overflowed.
    #[error("realtime output interleaved sample coordinate overflow")]
    SampleCoordinateOverflow,
    /// Exact prefix discard requested more complete frames than are queued.
    #[error("cannot discard {requested_frames} PCM frames from {buffered_frames} buffered frames")]
    InsufficientBufferedFrames {
        requested_frames: usize,
        buffered_frames: usize,
    },
}

/// Rejection of one complete interleaved PCM buffer at the device queue seam.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(crate) enum RealtimeAudioOutputEnqueueError {
    /// No concrete output stream is bound to the Manager.
    #[error("no concrete realtime audio output stream is available")]
    OutputUnavailable,
    /// Buffer rate differs from the concrete stream rate.
    #[error("PCM sample rate {actual} does not match output rate {expected}")]
    SampleRateMismatch { expected: u32, actual: u32 },
    /// Buffer semantic layout differs from the concrete stream layout.
    #[error("PCM layout {actual} does not match output layout {expected}")]
    ChannelLayoutMismatch {
        expected: AudioChannelLayout,
        actual: AudioChannelLayout,
    },
    /// Interleaved samples do not form complete frames in the declared layout.
    #[error("PCM sample count {samples} is not divisible by {channels} channels")]
    IncompleteInterleavedFrame { samples: usize, channels: usize },
    /// The whole buffer cannot fit; no sample was admitted.
    #[error("PCM output queue has {available_samples} samples free but needs {required_samples}")]
    InsufficientCapacity {
        required_samples: usize,
        available_samples: usize,
    },
    /// Queue length exceeded its fixed capacity, violating the queue contract.
    #[error("PCM output queue length exceeded its fixed capacity")]
    InvalidQueueOccupancy,
}

/// Failure to construct one concrete realtime output generation.
#[derive(Debug, Error)]
pub(crate) enum RealtimeAudioOutputCreateError {
    /// Every non-zero stream-generation identity has already been issued.
    #[error("realtime audio stream generation identity space is exhausted")]
    StreamGenerationExhausted,
    /// Concrete backend discovery or stream creation failed.
    #[error(transparent)]
    Backend(#[from] MondrianError),
}

/// Callback-derived audio output evidence. This is not an exact hardware playhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimeAudioOutputSnapshot {
    /// Exact process-monotonic instant at which this immutable snapshot was
    /// captured. Consumers must bind callback age and active duration to this
    /// instant rather than to a later queue-processing time.
    pub captured_at: Instant,
    /// Concrete CPAL stream generation.
    pub stream_generation: u64,
    /// Configured device sample rate.
    pub sample_rate: u32,
    /// Configured interleaved channel count.
    pub channels: u8,
    /// Frames requested by callbacks since stream creation.
    pub callback_consumed_frames: u64,
    /// Callback-consumed frames since the output was most recently activated.
    pub active_callback_consumed_frames: u64,
    /// Monotonic wall time since the current consumption interval began.
    pub active_duration: Option<Duration>,
    /// Number of output callbacks observed.
    pub callback_count: u64,
    /// Active callback frames filled with silence because PCM was unavailable.
    pub underrun_frames: u64,
    /// Frame count requested by the latest callback.
    pub last_callback_frames: u32,
    /// Predicted callback-to-device playback delay reported by the audio host.
    pub last_callback_playback_delay: Option<Duration>,
    /// Runtime age of the latest callback, or `None` before the first callback.
    pub last_callback_age: Option<Duration>,
    /// PCM frames currently waiting in the output queue.
    pub buffered_frames: usize,
    /// Whether CPAL reported an asynchronous stream error.
    pub stream_failed: bool,
    /// Whether playback consumption is currently enabled.
    pub active: bool,
}

struct RealtimeAudioOutputTelemetry {
    stream_generation: u64,
    origin: Instant,
    snapshot_update_in_progress: AtomicBool,
    snapshot_revision: AtomicU64,
    callback_consumed_frames: AtomicU64,
    active_callback_consumed_frames: AtomicU64,
    callback_count: AtomicU64,
    underrun_frames: AtomicU64,
    last_callback_frames: AtomicU64,
    last_callback_playback_delay_ns: AtomicU64,
    last_callback_elapsed_ns: AtomicU64,
    stream_failed: AtomicBool,
}

struct RealtimeAudioCallbackControl {
    control_transition: Mutex<()>,
    snapshot_update_in_progress: AtomicBool,
    snapshot_revision: AtomicU64,
    active: AtomicBool,
    deactivation_revision: AtomicU64,
    confirmed_quiescence_revision: AtomicU64,
    active_blocks_in_flight: AtomicU64,
}

impl RealtimeAudioCallbackControl {
    fn new() -> Self {
        Self {
            control_transition: Mutex::new(()),
            snapshot_update_in_progress: AtomicBool::new(false),
            snapshot_revision: AtomicU64::new(0),
            active: AtomicBool::new(false),
            deactivation_revision: AtomicU64::new(0),
            confirmed_quiescence_revision: AtomicU64::new(0),
            active_blocks_in_flight: AtomicU64::new(0),
        }
    }

    fn request_deactivation(
        &self,
        stream_generation: u64,
        telemetry: &RealtimeAudioOutputTelemetry,
    ) -> std::result::Result<RealtimeAudioOutputQuiescenceToken, RealtimeAudioOutputControlError>
    {
        let _transition = self.control_transition.lock();
        let revision = if self.active.load(Ordering::Acquire) {
            let previous = match self.deactivation_revision.fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |current| current.checked_add(1),
            ) {
                Ok(previous) => previous,
                Err(_) => {
                    // Identity exhaustion cannot permit further PCM
                    // consumption even though no reusable token can be issued.
                    self.active.store(false, Ordering::Release);
                    return Err(
                        RealtimeAudioOutputControlError::QuiescenceRevisionExhausted {
                            stream_generation,
                        },
                    );
                }
            };
            let revision = previous + 1;
            if !self.begin_snapshot_update(telemetry) {
                self.active.store(false, Ordering::Release);
                self.confirm_if_quiescent();
                return Ok(RealtimeAudioOutputQuiescenceToken { stream_generation, revision });
            }
            self.active.store(false, Ordering::Release);
            self.finish_snapshot_update();
            revision
        } else {
            self.deactivation_revision.load(Ordering::Acquire)
        };
        self.confirm_if_quiescent();
        Ok(RealtimeAudioOutputQuiescenceToken { stream_generation, revision })
    }

    fn validate_deactivation(
        &self,
        stream_generation: u64,
    ) -> std::result::Result<(), RealtimeAudioOutputControlError> {
        let _transition = self.control_transition.lock();
        if self.active.load(Ordering::Acquire)
            && self.deactivation_revision.load(Ordering::Acquire) == u64::MAX
        {
            Err(RealtimeAudioOutputControlError::QuiescenceRevisionExhausted { stream_generation })
        } else {
            Ok(())
        }
    }

    fn begin_callback_block(&self, telemetry: &RealtimeAudioOutputTelemetry) -> bool {
        // Read the revision on both sides of the active/reservation
        // observation. This closes the deactivate -> acknowledge ->
        // reactivate ABA race: a callback that began under the retired
        // revision can never consume PCM queued for the new interval.
        let observed_revision = self.deactivation_revision.load(Ordering::Acquire);
        if !self.active.load(Ordering::Acquire) {
            return false;
        }
        if self
            .active_blocks_in_flight
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .is_err()
        {
            telemetry.stream_failed.store(true, Ordering::Release);
            return false;
        }
        if self.active.load(Ordering::Acquire)
            && self.deactivation_revision.load(Ordering::Acquire) == observed_revision
        {
            return true;
        }
        let previous = self.active_blocks_in_flight.fetch_sub(1, Ordering::AcqRel);
        if previous == 0 {
            telemetry.stream_failed.store(true, Ordering::Release);
            self.active_blocks_in_flight.store(0, Ordering::Release);
        }
        self.confirm_if_quiescent();
        false
    }

    fn finish_callback_block(
        &self,
        consumed_active_pcm: bool,
        telemetry: &RealtimeAudioOutputTelemetry,
    ) {
        if consumed_active_pcm {
            let previous = self.active_blocks_in_flight.fetch_sub(1, Ordering::AcqRel);
            if previous == 0 {
                telemetry.stream_failed.store(true, Ordering::Release);
                self.active_blocks_in_flight.store(0, Ordering::Release);
            }
        }
        self.confirm_if_quiescent();
    }

    fn confirm_if_quiescent(&self) {
        if !self.active.load(Ordering::Acquire)
            && self.active_blocks_in_flight.load(Ordering::Acquire) == 0
        {
            let requested = self.deactivation_revision.load(Ordering::Acquire);
            self.confirmed_quiescence_revision.fetch_max(requested, Ordering::AcqRel);
        }
    }

    fn is_quiescent(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
        stream_generation: u64,
    ) -> std::result::Result<bool, RealtimeAudioOutputControlError> {
        validate_quiescence_token(token, stream_generation, self)?;
        Ok(self.confirmed_quiescence_revision.load(Ordering::Acquire) >= token.revision)
    }

    fn activate_with_preparation(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
        stream_generation: u64,
        telemetry: &RealtimeAudioOutputTelemetry,
        activation_elapsed_ns: &AtomicU64,
        prepare: impl FnOnce() -> std::result::Result<(), RealtimeAudioOutputControlError>,
    ) -> std::result::Result<(), RealtimeAudioOutputControlError> {
        let _transition = self.control_transition.lock();
        validate_quiescence_token(token, stream_generation, self)?;
        if self.confirmed_quiescence_revision.load(Ordering::Acquire) < token.revision {
            return Err(RealtimeAudioOutputControlError::CallbackNotQuiescent {
                revision: token.revision,
            });
        }
        prepare()?;
        if !self.begin_snapshot_update(telemetry) {
            return Err(
                RealtimeAudioOutputControlError::QuiescenceRevisionExhausted { stream_generation },
            );
        }
        telemetry.active_callback_consumed_frames.store(0, Ordering::Release);
        let elapsed_ns = telemetry.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        activation_elapsed_ns.store(elapsed_ns, Ordering::Release);
        self.active.store(true, Ordering::Release);
        self.finish_snapshot_update();
        Ok(())
    }

    fn begin_snapshot_update(&self, telemetry: &RealtimeAudioOutputTelemetry) -> bool {
        if self.snapshot_revision.load(Ordering::Acquire) == u64::MAX
            || self
                .snapshot_update_in_progress
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            telemetry.stream_failed.store(true, Ordering::Release);
            return false;
        }
        true
    }

    fn finish_snapshot_update(&self) {
        let advanced = self
            .snapshot_revision
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .is_ok();
        debug_assert!(advanced, "snapshot revision was preflighted");
        self.snapshot_update_in_progress.store(false, Ordering::Release);
    }
}

fn validate_quiescence_token(
    token: RealtimeAudioOutputQuiescenceToken,
    stream_generation: u64,
    control: &RealtimeAudioCallbackControl,
) -> std::result::Result<(), RealtimeAudioOutputControlError> {
    if token.stream_generation != stream_generation {
        return Err(RealtimeAudioOutputControlError::StreamGenerationMismatch {
            token_generation: token.stream_generation,
            current_generation: stream_generation,
        });
    }
    let current_revision = control.deactivation_revision.load(Ordering::Acquire);
    if token.revision != current_revision {
        return Err(
            RealtimeAudioOutputControlError::QuiescenceRevisionMismatch {
                token_revision: token.revision,
                current_revision,
            },
        );
    }
    Ok(())
}

fn allocate_stream_generation(
    last_issued: &AtomicU64,
) -> std::result::Result<u64, RealtimeAudioOutputCreateError> {
    last_issued
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .map(|previous| previous + 1)
        .map_err(|_| RealtimeAudioOutputCreateError::StreamGenerationExhausted)
}

impl RealtimeAudioOutputTelemetry {
    fn new() -> std::result::Result<Self, RealtimeAudioOutputCreateError> {
        static LAST_STREAM_GENERATION: AtomicU64 = AtomicU64::new(0);
        Ok(Self {
            stream_generation: allocate_stream_generation(&LAST_STREAM_GENERATION)?,
            origin: Instant::now(),
            snapshot_update_in_progress: AtomicBool::new(false),
            snapshot_revision: AtomicU64::new(0),
            callback_consumed_frames: AtomicU64::new(0),
            active_callback_consumed_frames: AtomicU64::new(0),
            callback_count: AtomicU64::new(0),
            underrun_frames: AtomicU64::new(0),
            last_callback_frames: AtomicU64::new(0),
            last_callback_playback_delay_ns: AtomicU64::new(0),
            last_callback_elapsed_ns: AtomicU64::new(0),
            stream_failed: AtomicBool::new(false),
        })
    }

    fn record_callback(
        &self,
        active_block: bool,
        frames: usize,
        underrun_frames: usize,
        playback_delay: Duration,
    ) {
        // CPAL serializes callbacks for one stream. The checked writer flag
        // makes that contract observable and lets readers reject a torn set of
        // independently atomic callback facts without ever locking this path.
        if self.snapshot_revision.load(Ordering::Acquire) == u64::MAX
            || self
                .snapshot_update_in_progress
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            self.stream_failed.store(true, Ordering::Release);
            return;
        }
        let frames = match u64::try_from(frames) {
            Ok(frames) => frames,
            Err(_) => {
                self.stream_failed.store(true, Ordering::Release);
                u64::MAX
            }
        };
        let underrun_frames = u64::try_from(underrun_frames).unwrap_or(u64::MAX);
        saturating_atomic_add(&self.callback_consumed_frames, frames);
        saturating_atomic_add(&self.callback_count, 1);
        saturating_atomic_add(&self.underrun_frames, underrun_frames);
        self.last_callback_frames.store(frames, Ordering::Relaxed);
        if active_block
            && self
                .active_callback_consumed_frames
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    current.checked_add(frames)
                })
                .is_err()
        {
            self.stream_failed.store(true, Ordering::Release);
        }
        self.last_callback_playback_delay_ns.store(
            playback_delay.as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        let elapsed_ns = self.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.last_callback_elapsed_ns.store(elapsed_ns, Ordering::Release);
        let advanced = self
            .snapshot_revision
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .is_ok();
        if !advanced {
            self.stream_failed.store(true, Ordering::Release);
        }
        self.snapshot_update_in_progress.store(false, Ordering::Release);
    }
}

fn saturating_atomic_add(value: &AtomicU64, increment: u64) {
    let _ = value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(increment))
    });
}

impl RealtimeAudioOutput {
    pub(crate) fn try_new(
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
    ) -> std::result::Result<
        (Self, RealtimeAudioOutputHandle, RealtimeAudioOutputObserver),
        RealtimeAudioOutputCreateError,
    > {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| MondrianError::Other(anyhow::anyhow!("未找到默认音频输出设备")))?;

        let channels = channel_layout.channel_count_u8();
        let config = cpal::StreamConfig {
            channels: u16::from(channels),
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let queue_capacity = usize::try_from(sample_rate)
            .ok()
            .and_then(|rate| rate.checked_mul(usize::from(channels)))
            .and_then(|samples_per_second| samples_per_second.checked_mul(2))
            .ok_or_else(|| {
                MondrianError::Other(anyhow::anyhow!("audio output queue capacity overflow"))
            })?;
        let queue = Arc::new(ArrayQueue::new(queue_capacity));
        let queue_for_cb = Arc::clone(&queue);
        let callback_control = Arc::new(RealtimeAudioCallbackControl::new());
        let telemetry = Arc::new(RealtimeAudioOutputTelemetry::new()?);
        let telemetry_for_error = Arc::clone(&telemetry);
        let err_fn = move |_error| {
            telemetry_for_error.stream_failed.store(true, Ordering::Release);
        };

        let default_config = device
            .default_output_config()
            .map_err(|e| MondrianError::Other(anyhow::anyhow!("读取默认输出配置失败: {e}")))?;

        let stream = match default_config.sample_format() {
            cpal::SampleFormat::F32 => build_f32_stream(
                &device,
                &config,
                queue_for_cb,
                Arc::clone(&callback_control),
                Arc::clone(&telemetry),
                err_fn,
            )
            .map_err(|e| MondrianError::Other(anyhow::anyhow!("创建 F32 输出流失败: {e}")))?,
            cpal::SampleFormat::I16 => {
                let queue_for_cb = Arc::clone(&queue);
                let callback_control_for_cb = Arc::clone(&callback_control);
                let telemetry_for_cb = Arc::clone(&telemetry);
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [i16], info| {
                            let frames = data.len() / channels.max(1) as usize;
                            let playback_delay = callback_playback_delay(info);
                            let active_block =
                                callback_control_for_cb.begin_callback_block(&telemetry_for_cb);
                            if !active_block {
                                data.fill(0);
                                telemetry_for_cb.record_callback(false, frames, 0, playback_delay);
                                callback_control_for_cb
                                    .finish_callback_block(false, &telemetry_for_cb);
                                return;
                            }
                            let mut missing_samples = 0usize;
                            for s in data {
                                let v = queue_for_cb
                                    .pop()
                                    .unwrap_or_else(|| {
                                        missing_samples = missing_samples.saturating_add(1);
                                        0.0
                                    })
                                    .clamp(-1.0, 1.0);
                                *s = (v * i16::MAX as f32) as i16;
                            }
                            telemetry_for_cb.record_callback(
                                true,
                                frames,
                                missing_samples / channels.max(1) as usize,
                                playback_delay,
                            );
                            callback_control_for_cb.finish_callback_block(true, &telemetry_for_cb);
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| {
                        MondrianError::Other(anyhow::anyhow!("创建 I16 输出流失败: {e}"))
                    })?
            }
            cpal::SampleFormat::U16 => {
                let queue_for_cb = Arc::clone(&queue);
                let callback_control_for_cb = Arc::clone(&callback_control);
                let telemetry_for_cb = Arc::clone(&telemetry);
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [u16], info| {
                            let frames = data.len() / channels.max(1) as usize;
                            let playback_delay = callback_playback_delay(info);
                            let active_block =
                                callback_control_for_cb.begin_callback_block(&telemetry_for_cb);
                            if !active_block {
                                data.fill(u16::MAX / 2);
                                telemetry_for_cb.record_callback(false, frames, 0, playback_delay);
                                callback_control_for_cb
                                    .finish_callback_block(false, &telemetry_for_cb);
                                return;
                            }
                            let mut missing_samples = 0usize;
                            for s in data {
                                let v = queue_for_cb
                                    .pop()
                                    .unwrap_or_else(|| {
                                        missing_samples = missing_samples.saturating_add(1);
                                        0.0
                                    })
                                    .clamp(-1.0, 1.0);
                                *s = ((v * 0.5 + 0.5) * u16::MAX as f32) as u16;
                            }
                            telemetry_for_cb.record_callback(
                                true,
                                frames,
                                missing_samples / channels.max(1) as usize,
                                playback_delay,
                            );
                            callback_control_for_cb.finish_callback_block(true, &telemetry_for_cb);
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| {
                        MondrianError::Other(anyhow::anyhow!("创建 U16 输出流失败: {e}"))
                    })?
            }
            _ => {
                return Err(
                    MondrianError::Other(anyhow::anyhow!("当前音频设备采样格式不受支持")).into(),
                );
            }
        };

        stream
            .play()
            .map_err(|e| MondrianError::Other(anyhow::anyhow!("启动音频输出流失败: {e}")))?;

        let snapshot_cache = Arc::new(Mutex::new(None));
        let output = Self {
            sample_rate,
            channel_layout,
            queue,
            callback_control,
            activation_elapsed_ns: Arc::new(AtomicU64::new(0)),
            telemetry,
            snapshot_cache,
            _stream: stream,
        };
        let handle = RealtimeAudioOutputHandle {
            sample_rate,
            channel_layout,
            queue: Arc::clone(&output.queue),
            callback_control: Arc::clone(&output.callback_control),
            activation_elapsed_ns: Arc::clone(&output.activation_elapsed_ns),
            telemetry: Arc::clone(&output.telemetry),
            snapshot_cache: Arc::clone(&output.snapshot_cache),
        };
        let observer = RealtimeAudioOutputObserver {
            sample_rate,
            channel_layout,
            queue: Arc::clone(&output.queue),
            callback_control: Arc::clone(&output.callback_control),
            activation_elapsed_ns: Arc::clone(&output.activation_elapsed_ns),
            telemetry: Arc::clone(&output.telemetry),
            snapshot_cache: Arc::clone(&output.snapshot_cache),
        };
        Ok((output, handle, observer))
    }

    pub(crate) fn deactivate(
        &self,
    ) -> std::result::Result<RealtimeAudioOutputQuiescenceToken, RealtimeAudioOutputControlError>
    {
        self.callback_control
            .request_deactivation(self.telemetry.stream_generation, &self.telemetry)
    }

    pub(crate) fn force_inactive_for_retirement(&self) {
        let _transition = self.callback_control.control_transition.lock();
        let snapshot_update = self.callback_control.begin_snapshot_update(&self.telemetry);
        self.callback_control.active.store(false, Ordering::Release);
        self.telemetry.stream_failed.store(true, Ordering::Release);
        if snapshot_update {
            self.callback_control.finish_snapshot_update();
        }
        self.callback_control.confirm_if_quiescent();
    }

    /// Capture callback-consumption and health evidence without touching CPAL.
    pub(crate) fn snapshot(&self) -> RealtimeAudioOutputSnapshot {
        capture_output_snapshot(
            self.sample_rate,
            self.channel_layout,
            &self.queue,
            &self.callback_control,
            &self.activation_elapsed_ns,
            &self.telemetry,
            &self.snapshot_cache,
        )
    }
}

impl RealtimeAudioOutputHandle {
    pub(crate) fn enqueue(
        &mut self,
        buffer: &AudioBuffer,
    ) -> std::result::Result<(), RealtimeAudioOutputEnqueueError> {
        if buffer.sample_rate != self.sample_rate {
            return Err(RealtimeAudioOutputEnqueueError::SampleRateMismatch {
                expected: self.sample_rate,
                actual: buffer.sample_rate,
            });
        }
        if buffer.channel_layout != self.channel_layout {
            return Err(RealtimeAudioOutputEnqueueError::ChannelLayoutMismatch {
                expected: self.channel_layout,
                actual: buffer.channel_layout,
            });
        }
        let channels = self.channel_layout.channel_count();
        if !buffer.samples.len().is_multiple_of(channels) {
            return Err(
                RealtimeAudioOutputEnqueueError::IncompleteInterleavedFrame {
                    samples: buffer.samples.len(),
                    channels,
                },
            );
        }
        let available_samples = self
            .queue
            .capacity()
            .checked_sub(self.queue.len())
            .ok_or(RealtimeAudioOutputEnqueueError::InvalidQueueOccupancy)?;
        if buffer.samples.len() > available_samples {
            return Err(RealtimeAudioOutputEnqueueError::InsufficientCapacity {
                required_samples: buffer.samples.len(),
                available_samples,
            });
        }
        // This handle is deliberately non-Clone and owned by the Manager, so
        // it is the queue's sole producer. The device callback only pops. Once
        // the whole-buffer capacity preflight succeeds, every push is proven to
        // succeed and the interleaved buffer is admitted atomically.
        for sample in &buffer.samples {
            assert!(
                self.queue.push(*sample).is_ok(),
                "single-producer PCM capacity proof was violated"
            );
        }
        Ok(())
    }

    pub(crate) fn clear(&self) {
        while self.queue.pop().is_some() {}
    }

    pub(crate) fn deactivate(
        &self,
    ) -> std::result::Result<RealtimeAudioOutputQuiescenceToken, RealtimeAudioOutputControlError>
    {
        self.callback_control
            .request_deactivation(self.telemetry.stream_generation, &self.telemetry)
    }

    pub(crate) fn validate_deactivation(&self) -> Result<(), RealtimeAudioOutputControlError> {
        self.callback_control.validate_deactivation(self.telemetry.stream_generation)
    }

    pub(crate) fn is_quiescent(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
    ) -> std::result::Result<bool, RealtimeAudioOutputControlError> {
        self.callback_control.is_quiescent(token, self.telemetry.stream_generation)
    }

    #[cfg(test)]
    pub(crate) fn activate(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
    ) -> std::result::Result<(), RealtimeAudioOutputControlError> {
        self.callback_control.activate_with_preparation(
            token,
            self.telemetry.stream_generation,
            &self.telemetry,
            &self.activation_elapsed_ns,
            || Ok(()),
        )
    }

    pub(crate) fn activate_after_discard(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
        frames: usize,
    ) -> std::result::Result<(), RealtimeAudioOutputControlError> {
        self.callback_control.activate_with_preparation(
            token,
            self.telemetry.stream_generation,
            &self.telemetry,
            &self.activation_elapsed_ns,
            || self.discard_frames(frames),
        )
    }

    pub(crate) fn discard_frames(
        &self,
        frames: usize,
    ) -> std::result::Result<(), RealtimeAudioOutputControlError> {
        let channels = self.channel_layout.channel_count();
        let requested_samples = frames
            .checked_mul(channels)
            .ok_or(RealtimeAudioOutputControlError::SampleCoordinateOverflow)?;
        let buffered_samples = self.queue.len();
        if requested_samples > buffered_samples {
            return Err(
                RealtimeAudioOutputControlError::InsufficientBufferedFrames {
                    requested_frames: frames,
                    buffered_frames: buffered_samples / channels,
                },
            );
        }
        for _ in 0..requested_samples {
            if self.queue.pop().is_none() {
                return Err(
                    RealtimeAudioOutputControlError::InsufficientBufferedFrames {
                        requested_frames: frames,
                        buffered_frames: self.queue.len() / channels,
                    },
                );
            }
        }
        Ok(())
    }

    pub(crate) fn capacity_frames(&self) -> usize {
        self.queue.capacity() / self.channel_layout.channel_count()
    }

    pub(crate) fn buffered_frames(&self) -> usize {
        self.queue.len() / self.channel_layout.channel_count()
    }

    pub(crate) fn snapshot(&self) -> RealtimeAudioOutputSnapshot {
        capture_output_snapshot(
            self.sample_rate,
            self.channel_layout,
            &self.queue,
            &self.callback_control,
            &self.activation_elapsed_ns,
            &self.telemetry,
            &self.snapshot_cache,
        )
    }
}

impl RealtimeAudioOutputObserver {
    pub(crate) fn snapshot(&self) -> RealtimeAudioOutputSnapshot {
        capture_output_snapshot(
            self.sample_rate,
            self.channel_layout,
            &self.queue,
            &self.callback_control,
            &self.activation_elapsed_ns,
            &self.telemetry,
            &self.snapshot_cache,
        )
    }
}

fn capture_output_snapshot(
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    queue: &ArrayQueue<f32>,
    callback_control: &RealtimeAudioCallbackControl,
    activation_elapsed_ns: &AtomicU64,
    telemetry: &RealtimeAudioOutputTelemetry,
    snapshot_cache: &Mutex<Option<RealtimeAudioOutputSnapshot>>,
) -> RealtimeAudioOutputSnapshot {
    // One callback updates only a fixed handful of atomics. This bound permits
    // a reader on another core to observe the publication gap without allowing
    // a broken backend to spin a control/UI thread indefinitely.
    const MAX_CAPTURE_ATTEMPTS: usize = 4_096;
    for _ in 0..MAX_CAPTURE_ATTEMPTS {
        if telemetry.snapshot_update_in_progress.load(Ordering::Acquire)
            || callback_control.snapshot_update_in_progress.load(Ordering::Acquire)
        {
            std::hint::spin_loop();
            continue;
        }
        let callback_revision = telemetry.snapshot_revision.load(Ordering::Acquire);
        let control_revision = callback_control.snapshot_revision.load(Ordering::Acquire);
        let callback_consumed_frames = telemetry.callback_consumed_frames.load(Ordering::Relaxed);
        let active_callback_consumed_frames =
            telemetry.active_callback_consumed_frames.load(Ordering::Relaxed);
        let callback_count = telemetry.callback_count.load(Ordering::Relaxed);
        let underrun_frames = telemetry.underrun_frames.load(Ordering::Relaxed);
        let last_callback_frames = telemetry.last_callback_frames.load(Ordering::Relaxed);
        let last_callback_playback_delay_ns =
            telemetry.last_callback_playback_delay_ns.load(Ordering::Relaxed);
        let last_callback_elapsed_ns = telemetry.last_callback_elapsed_ns.load(Ordering::Relaxed);
        let stream_failed = telemetry.stream_failed.load(Ordering::Acquire);
        let active = callback_control.active.load(Ordering::Acquire);
        let activation_elapsed_ns = activation_elapsed_ns.load(Ordering::Acquire);
        // Capture time comes after the observed counters. A stable revision
        // therefore proves that no consumed frame can lie in this instant's
        // future, even if this reader was descheduled while sampling.
        let captured_at = Instant::now();
        let callback_stable = !telemetry.snapshot_update_in_progress.load(Ordering::Acquire)
            && telemetry.snapshot_revision.load(Ordering::Acquire) == callback_revision;
        let control_stable = !callback_control.snapshot_update_in_progress.load(Ordering::Acquire)
            && callback_control.snapshot_revision.load(Ordering::Acquire) == control_revision;
        if !callback_stable || !control_stable {
            std::hint::spin_loop();
            continue;
        }
        let now_ns =
            captured_at.duration_since(telemetry.origin).as_nanos().min(u64::MAX as u128) as u64;
        let snapshot = RealtimeAudioOutputSnapshot {
            captured_at,
            stream_generation: telemetry.stream_generation,
            sample_rate,
            channels: channel_layout.channel_count_u8(),
            callback_consumed_frames,
            active_callback_consumed_frames,
            active_duration: active
                .then(|| Duration::from_nanos(now_ns.saturating_sub(activation_elapsed_ns))),
            callback_count,
            underrun_frames,
            last_callback_frames: last_callback_frames.min(u32::MAX as u64) as u32,
            last_callback_playback_delay: (last_callback_elapsed_ns > 0)
                .then(|| Duration::from_nanos(last_callback_playback_delay_ns)),
            last_callback_age: (last_callback_elapsed_ns > 0)
                .then(|| Duration::from_nanos(now_ns.saturating_sub(last_callback_elapsed_ns))),
            buffered_frames: queue.len() / channel_layout.channel_count(),
            stream_failed,
            active,
        };
        *snapshot_cache.lock() = Some(snapshot);
        return snapshot;
    }

    // A preempted callback writer may temporarily prevent a current read. Reuse
    // the last immutable fact rather than fabricating progress or declaring a
    // healthy stream failed; consumers will age that fact conservatively.
    if let Some(snapshot) = *snapshot_cache.lock() {
        return snapshot;
    }

    // Before any fact has been cached, persistent contention leaves no truthful
    // evidence to publish and must force the normal typed recovery path.
    telemetry.stream_failed.store(true, Ordering::Release);
    RealtimeAudioOutputSnapshot {
        captured_at: Instant::now(),
        stream_generation: telemetry.stream_generation,
        sample_rate,
        channels: channel_layout.channel_count_u8(),
        callback_consumed_frames: 0,
        active_callback_consumed_frames: 0,
        active_duration: None,
        callback_count: 0,
        underrun_frames: 0,
        last_callback_frames: 0,
        last_callback_playback_delay: None,
        last_callback_age: None,
        buffered_frames: queue.len() / channel_layout.channel_count(),
        stream_failed: true,
        active: false,
    }
}

fn build_f32_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: Arc<ArrayQueue<f32>>,
    callback_control: Arc<RealtimeAudioCallbackControl>,
    telemetry: Arc<RealtimeAudioOutputTelemetry>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> std::result::Result<cpal::Stream, cpal::BuildStreamError> {
    let channels = config.channels.max(1) as usize;
    device.build_output_stream(
        config,
        move |data: &mut [f32], info| {
            let frames = data.len() / channels;
            let playback_delay = callback_playback_delay(info);
            let active_block = callback_control.begin_callback_block(&telemetry);
            if !active_block {
                data.fill(0.0);
                telemetry.record_callback(false, frames, 0, playback_delay);
                callback_control.finish_callback_block(false, &telemetry);
                return;
            }
            let mut missing_samples = 0usize;
            for s in data {
                let value = queue.pop().unwrap_or_else(|| {
                    missing_samples = missing_samples.saturating_add(1);
                    0.0
                });
                *s = value;
            }
            telemetry.record_callback(true, frames, missing_samples / channels, playback_delay);
            callback_control.finish_callback_block(true, &telemetry);
        },
        err_fn,
        None,
    )
}

fn callback_playback_delay(info: &cpal::OutputCallbackInfo) -> Duration {
    let timestamp = info.timestamp();
    timestamp.playback.duration_since(&timestamp.callback).unwrap_or(Duration::ZERO)
}

#[cfg(test)]
pub(crate) fn decode_audio_file_with_ffmpeg_cli(
    path: &Path,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
) -> Result<AudioBuffer> {
    let ffmpeg_layout = match channel_layout {
        AudioChannelLayout::Mono => Some("mono"),
        AudioChannelLayout::Stereo => Some("stereo"),
        AudioChannelLayout::Surround51Side => Some("5.1(side)"),
        AudioChannelLayout::Speakers(_) | AudioChannelLayout::Discrete(_) => None,
    }
    .ok_or_else(|| MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: format!("audio output layout {channel_layout:?} has no explicit FFmpeg lowering"),
    })?;
    let output = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-i")
        .arg(path)
        .arg("-vn")
        .arg("-sn")
        .arg("-dn")
        .arg("-f")
        .arg("f32le")
        .arg("-channel_layout")
        .arg(ffmpeg_layout)
        .arg("-ac")
        .arg(channel_layout.channel_count().to_string())
        .arg("-ar")
        .arg(sample_rate.max(8_000).to_string())
        .arg("pipe:1")
        .output()
        .map_err(|e| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!("调用 ffmpeg 失败: {e}"),
        })?;

    if !output.status.success() {
        return Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "ffmpeg 解码失败: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }

    let mut samples = Vec::with_capacity(output.stdout.len() / 4);
    for chunk in output.stdout.chunks_exact(4) {
        samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }

    Ok(AudioBuffer {
        samples,
        sample_rate: sample_rate.max(8_000),
        channel_layout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output_handle(capacity_samples: usize) -> (RealtimeAudioOutputHandle, Arc<ArrayQueue<f32>>) {
        let queue = Arc::new(ArrayQueue::new(capacity_samples));
        (
            RealtimeAudioOutputHandle {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                queue: Arc::clone(&queue),
                callback_control: Arc::new(RealtimeAudioCallbackControl::new()),
                activation_elapsed_ns: Arc::new(AtomicU64::new(0)),
                telemetry: Arc::new(
                    RealtimeAudioOutputTelemetry::new().expect("allocate test stream generation"),
                ),
                snapshot_cache: Arc::new(Mutex::new(None)),
            },
            queue,
        )
    }

    #[test]
    fn callback_telemetry_accumulates_consumption_and_underrun_without_locking() {
        let telemetry =
            RealtimeAudioOutputTelemetry::new().expect("allocate test stream generation");

        telemetry.record_callback(false, 480, 0, Duration::from_millis(10));
        telemetry.record_callback(true, 480, 32, Duration::from_millis(12));

        assert_eq!(
            telemetry.callback_consumed_frames.load(Ordering::Relaxed),
            960
        );
        assert_eq!(telemetry.callback_count.load(Ordering::Relaxed), 2);
        assert_eq!(
            telemetry.active_callback_consumed_frames.load(Ordering::Relaxed),
            480
        );
        assert_eq!(telemetry.underrun_frames.load(Ordering::Relaxed), 32);
        assert_eq!(telemetry.last_callback_frames.load(Ordering::Relaxed), 480);
        assert_eq!(
            telemetry.last_callback_playback_delay_ns.load(Ordering::Relaxed),
            12_000_000
        );
    }

    #[test]
    fn concurrent_output_snapshots_never_publish_torn_callback_facts() {
        let telemetry =
            Arc::new(RealtimeAudioOutputTelemetry::new().expect("allocate test stream generation"));
        let callback_control = RealtimeAudioCallbackControl::new();
        let activation_elapsed_ns = AtomicU64::new(0);
        let queue = ArrayQueue::new(2);
        let snapshot_cache = Mutex::new(None);
        let initial = capture_output_snapshot(
            48_000,
            AudioChannelLayout::Stereo,
            &queue,
            &callback_control,
            &activation_elapsed_ns,
            &telemetry,
            &snapshot_cache,
        );
        assert_eq!(initial.callback_count, 0);
        let writer_telemetry = Arc::clone(&telemetry);
        let writer = std::thread::spawn(move || {
            for _ in 0..100_000 {
                writer_telemetry.record_callback(true, 2, 1, Duration::from_millis(3));
                // A physical backend always has a non-callback interval. Yield
                // explicitly so this stress test preserves that contract while
                // still exercising far more updates than realtime playback.
                std::thread::yield_now();
            }
        });

        while !writer.is_finished() {
            let snapshot = capture_output_snapshot(
                48_000,
                AudioChannelLayout::Stereo,
                &queue,
                &callback_control,
                &activation_elapsed_ns,
                &telemetry,
                &snapshot_cache,
            );
            assert!(!snapshot.stream_failed);
            assert_eq!(
                snapshot.callback_consumed_frames,
                snapshot.callback_count * 2
            );
            assert_eq!(
                snapshot.active_callback_consumed_frames,
                snapshot.callback_count * 2
            );
            assert_eq!(snapshot.underrun_frames, snapshot.callback_count);
            if snapshot.callback_count > 0 {
                assert_eq!(snapshot.last_callback_frames, 2);
                assert_eq!(
                    snapshot.last_callback_playback_delay,
                    Some(Duration::from_millis(3))
                );
            }
        }
        writer.join().expect("callback writer");

        let snapshot = capture_output_snapshot(
            48_000,
            AudioChannelLayout::Stereo,
            &queue,
            &callback_control,
            &activation_elapsed_ns,
            &telemetry,
            &snapshot_cache,
        );
        assert_eq!(snapshot.callback_count, 100_000);
        assert_eq!(snapshot.callback_consumed_frames, 200_000);
        assert_eq!(snapshot.active_callback_consumed_frames, 200_000);
        assert_eq!(snapshot.underrun_frames, 100_000);
    }

    #[test]
    fn deactivation_waits_for_an_already_started_active_callback_block() {
        let (handle, _) = output_handle(16);
        let initial = handle.deactivate().expect("initial quiescence");
        handle.activate(initial).expect("activate test interval");
        assert!(handle.callback_control.begin_callback_block(&handle.telemetry));

        let deactivation = handle.deactivate().expect("checked deactivation");
        assert!(!handle.is_quiescent(deactivation).expect("query quiescence"));

        handle.telemetry.record_callback(true, 4, 0, Duration::ZERO);
        handle.callback_control.finish_callback_block(true, &handle.telemetry);
        assert!(handle.is_quiescent(deactivation).expect("query quiescence"));
        assert_eq!(
            handle.telemetry.active_callback_consumed_frames.load(Ordering::Acquire),
            4
        );
    }

    #[test]
    fn inactive_callbacks_never_advance_the_active_media_counter() {
        let telemetry =
            RealtimeAudioOutputTelemetry::new().expect("allocate test stream generation");

        telemetry.record_callback(false, 512, 0, Duration::ZERO);

        assert_eq!(
            telemetry.callback_consumed_frames.load(Ordering::Acquire),
            512
        );
        assert_eq!(
            telemetry.active_callback_consumed_frames.load(Ordering::Acquire),
            0
        );
    }

    #[test]
    fn active_media_counter_overflow_marks_stream_failed_without_wrapping() {
        let telemetry =
            RealtimeAudioOutputTelemetry::new().expect("allocate test stream generation");
        telemetry.active_callback_consumed_frames.store(u64::MAX - 1, Ordering::Release);

        telemetry.record_callback(true, 2, 0, Duration::ZERO);

        assert_eq!(
            telemetry.active_callback_consumed_frames.load(Ordering::Acquire),
            u64::MAX - 1
        );
        assert!(telemetry.stream_failed.load(Ordering::Acquire));
    }

    #[test]
    fn exhausted_quiescence_revision_forces_inactive_without_reusing_identity() {
        let (handle, _) = output_handle(16);
        handle.callback_control.deactivation_revision.store(u64::MAX, Ordering::Release);
        handle.callback_control.active.store(true, Ordering::Release);

        assert!(matches!(
            handle.validate_deactivation(),
            Err(RealtimeAudioOutputControlError::QuiescenceRevisionExhausted { .. })
        ));
        assert!(matches!(
            handle.deactivate(),
            Err(RealtimeAudioOutputControlError::QuiescenceRevisionExhausted { .. })
        ));
        assert!(!handle.callback_control.active.load(Ordering::Acquire));
        assert_eq!(
            handle.callback_control.deactivation_revision.load(Ordering::Acquire),
            u64::MAX
        );
    }

    #[test]
    fn stream_generation_allocator_issues_max_once_then_fails_closed() {
        let last_issued = AtomicU64::new(u64::MAX - 1);

        assert_eq!(
            allocate_stream_generation(&last_issued).expect("allocate final stream generation"),
            u64::MAX
        );
        assert!(matches!(
            allocate_stream_generation(&last_issued),
            Err(RealtimeAudioOutputCreateError::StreamGenerationExhausted)
        ));
        assert_eq!(last_issued.load(Ordering::Acquire), u64::MAX);
    }

    #[test]
    fn output_enqueue_rejects_capacity_before_admitting_any_sample() {
        let (mut handle, queue) = output_handle(4);
        queue.push(0.25).expect("prefill");
        queue.push(0.5).expect("prefill");
        let buffer = AudioBuffer::silent(48_000, AudioChannelLayout::Stereo, 2);

        let result = handle.enqueue(&buffer);

        assert_eq!(
            result,
            Err(RealtimeAudioOutputEnqueueError::InsufficientCapacity {
                required_samples: 4,
                available_samples: 2,
            })
        );
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop(), Some(0.25));
        assert_eq!(queue.pop(), Some(0.5));
    }

    #[test]
    fn output_enqueue_rejects_rate_layout_and_partial_frames_without_mutation() {
        let (mut handle, queue) = output_handle(16);
        let wrong_rate = AudioBuffer::silent(44_100, AudioChannelLayout::Stereo, 1);
        assert!(matches!(
            handle.enqueue(&wrong_rate),
            Err(RealtimeAudioOutputEnqueueError::SampleRateMismatch { .. })
        ));
        let wrong_layout = AudioBuffer::silent(48_000, AudioChannelLayout::Mono, 1);
        assert!(matches!(
            handle.enqueue(&wrong_layout),
            Err(RealtimeAudioOutputEnqueueError::ChannelLayoutMismatch { .. })
        ));
        let partial = AudioBuffer {
            samples: vec![0.0; 3],
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
        };
        assert!(matches!(
            handle.enqueue(&partial),
            Err(RealtimeAudioOutputEnqueueError::IncompleteInterleavedFrame { .. })
        ));
        assert!(queue.is_empty());
    }

    #[test]
    fn exact_prefix_discard_rejects_shortage_without_partial_mutation() {
        let (handle, queue) = output_handle(16);
        for sample in [0.1, 0.2, 0.3, 0.4] {
            queue.push(sample).expect("prefill complete stereo frames");
        }

        assert_eq!(
            handle.discard_frames(3),
            Err(
                RealtimeAudioOutputControlError::InsufficientBufferedFrames {
                    requested_frames: 3,
                    buffered_frames: 2,
                }
            )
        );
        assert_eq!(queue.len(), 4);
        assert_eq!(queue.pop(), Some(0.1));
        assert_eq!(queue.pop(), Some(0.2));
        assert_eq!(queue.pop(), Some(0.3));
        assert_eq!(queue.pop(), Some(0.4));
    }
}
