//! 音频缓冲区与混合器

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_queue::ArrayQueue;
use mondrian_core::{AudioChannelLayout, MondrianError, Result};
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

pub struct RealtimeAudioOutput {
    sample_rate: u32,
    channels: u8,
    queue: Arc<ArrayQueue<f32>>,
    muted: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    activation_consumed_frames: Arc<AtomicU64>,
    activation_elapsed_ns: Arc<AtomicU64>,
    telemetry: Arc<RealtimeAudioOutputTelemetry>,
    _stream: cpal::Stream,
}

/// Sendable control/observation handle for a stream owned by its device thread.
#[derive(Clone)]
pub(crate) struct RealtimeAudioOutputHandle {
    sample_rate: u32,
    channels: u8,
    queue: Arc<ArrayQueue<f32>>,
    muted: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    activation_consumed_frames: Arc<AtomicU64>,
    activation_elapsed_ns: Arc<AtomicU64>,
    telemetry: Arc<RealtimeAudioOutputTelemetry>,
}

/// Callback-derived audio output evidence. This is not an exact hardware playhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimeAudioOutputSnapshot {
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
    callback_consumed_frames: AtomicU64,
    callback_count: AtomicU64,
    underrun_frames: AtomicU64,
    last_callback_frames: AtomicU64,
    last_callback_playback_delay_ns: AtomicU64,
    last_callback_elapsed_ns: AtomicU64,
    stream_failed: AtomicBool,
}

impl RealtimeAudioOutputTelemetry {
    fn new() -> Self {
        static NEXT_STREAM_GENERATION: AtomicU64 = AtomicU64::new(1);
        Self {
            stream_generation: NEXT_STREAM_GENERATION.fetch_add(1, Ordering::Relaxed),
            origin: Instant::now(),
            callback_consumed_frames: AtomicU64::new(0),
            callback_count: AtomicU64::new(0),
            underrun_frames: AtomicU64::new(0),
            last_callback_frames: AtomicU64::new(0),
            last_callback_playback_delay_ns: AtomicU64::new(0),
            last_callback_elapsed_ns: AtomicU64::new(0),
            stream_failed: AtomicBool::new(false),
        }
    }

    fn record_callback(&self, frames: usize, underrun_frames: usize, playback_delay: Duration) {
        self.callback_consumed_frames.fetch_add(frames as u64, Ordering::Relaxed);
        self.callback_count.fetch_add(1, Ordering::Relaxed);
        self.underrun_frames.fetch_add(underrun_frames as u64, Ordering::Relaxed);
        self.last_callback_frames.store(frames as u64, Ordering::Relaxed);
        self.last_callback_playback_delay_ns.store(
            playback_delay.as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        let elapsed_ns = self.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.last_callback_elapsed_ns.store(elapsed_ns, Ordering::Release);
    }
}

/// Monotonic cursor used to choose audio render windows; never a Clock Master.
#[derive(Debug, Clone)]
pub struct AudioRenderCursor {
    pub sample_rate: u32,
    started_at: Instant,
    offset_samples: i64,
}

impl AudioRenderCursor {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            started_at: Instant::now(),
            offset_samples: 0,
        }
    }

    pub fn now_samples(&self) -> i64 {
        let elapsed = self.started_at.elapsed().as_secs_f64();
        self.offset_samples + (elapsed * self.sample_rate as f64).round() as i64
    }

    pub fn now_seconds(&self) -> f64 {
        self.now_samples() as f64 / self.sample_rate as f64
    }

    pub fn seek_to_samples(&mut self, samples: i64) {
        self.started_at = Instant::now();
        self.offset_samples = samples;
    }
}

impl RealtimeAudioOutput {
    pub fn try_new(sample_rate: u32, channels: u8) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| MondrianError::Other(anyhow::anyhow!("未找到默认音频输出设备")))?;

        let config = cpal::StreamConfig {
            channels: channels.max(1) as u16,
            sample_rate: cpal::SampleRate(sample_rate.max(8_000)),
            buffer_size: cpal::BufferSize::Default,
        };

        let queue_capacity = sample_rate as usize * channels.max(1) as usize * 2;
        let queue = Arc::new(ArrayQueue::new(queue_capacity.max(1)));
        let queue_for_cb = Arc::clone(&queue);
        let muted = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicBool::new(false));
        let telemetry = Arc::new(RealtimeAudioOutputTelemetry::new());
        let telemetry_for_error = Arc::clone(&telemetry);
        let err_fn = move |_error| {
            telemetry_for_error.stream_failed.store(true, Ordering::Release);
        };

        let default_config = device
            .default_output_config()
            .map_err(|e| MondrianError::Other(anyhow::anyhow!("读取默认输出配置失败: {e}")))?;

        let stream = match default_config.sample_format() {
            cpal::SampleFormat::F32 => {
                let muted_for_cb = Arc::clone(&muted);
                build_f32_stream(
                    &device,
                    &config,
                    queue_for_cb,
                    muted_for_cb,
                    Arc::clone(&active),
                    Arc::clone(&telemetry),
                    err_fn,
                )
                .map_err(|e| MondrianError::Other(anyhow::anyhow!("创建 F32 输出流失败: {e}")))?
            }
            cpal::SampleFormat::I16 => {
                let queue_for_cb = Arc::clone(&queue);
                let muted_for_cb = Arc::clone(&muted);
                let active_for_cb = Arc::clone(&active);
                let telemetry_for_cb = Arc::clone(&telemetry);
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [i16], info| {
                            let frames = data.len() / channels.max(1) as usize;
                            let playback_delay = callback_playback_delay(info);
                            if muted_for_cb.load(Ordering::Relaxed)
                                || !active_for_cb.load(Ordering::Relaxed)
                            {
                                data.fill(0);
                                telemetry_for_cb.record_callback(frames, 0, playback_delay);
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
                                frames,
                                missing_samples / channels.max(1) as usize,
                                playback_delay,
                            );
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
                let muted_for_cb = Arc::clone(&muted);
                let active_for_cb = Arc::clone(&active);
                let telemetry_for_cb = Arc::clone(&telemetry);
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [u16], info| {
                            let frames = data.len() / channels.max(1) as usize;
                            let playback_delay = callback_playback_delay(info);
                            if muted_for_cb.load(Ordering::Relaxed)
                                || !active_for_cb.load(Ordering::Relaxed)
                            {
                                data.fill(u16::MAX / 2);
                                telemetry_for_cb.record_callback(frames, 0, playback_delay);
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
                                frames,
                                missing_samples / channels.max(1) as usize,
                                playback_delay,
                            );
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| {
                        MondrianError::Other(anyhow::anyhow!("创建 U16 输出流失败: {e}"))
                    })?
            }
            _ => {
                return Err(MondrianError::Other(anyhow::anyhow!(
                    "当前音频设备采样格式不受支持"
                )));
            }
        };

        stream
            .play()
            .map_err(|e| MondrianError::Other(anyhow::anyhow!("启动音频输出流失败: {e}")))?;

        Ok(Self {
            sample_rate,
            channels,
            queue,
            muted,
            active,
            activation_consumed_frames: Arc::new(AtomicU64::new(0)),
            activation_elapsed_ns: Arc::new(AtomicU64::new(0)),
            telemetry,
            _stream: stream,
        })
    }

    pub(crate) fn handle(&self) -> RealtimeAudioOutputHandle {
        RealtimeAudioOutputHandle {
            sample_rate: self.sample_rate,
            channels: self.channels,
            queue: Arc::clone(&self.queue),
            muted: Arc::clone(&self.muted),
            active: Arc::clone(&self.active),
            activation_consumed_frames: Arc::clone(&self.activation_consumed_frames),
            activation_elapsed_ns: Arc::clone(&self.activation_elapsed_ns),
            telemetry: Arc::clone(&self.telemetry),
        }
    }

    pub fn enqueue(&self, buffer: &AudioBuffer) {
        if buffer.samples.is_empty() || buffer.channel_count() != usize::from(self.channels) {
            return;
        }
        for sample in &buffer.samples {
            let mut pending = *sample;
            loop {
                match self.queue.push(pending) {
                    Ok(()) => break,
                    Err(returned) => {
                        pending = returned;
                        let _ = self.queue.pop();
                    }
                }
            }
        }
    }

    pub fn clear(&self) {
        while self.queue.pop().is_some() {}
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Begin or end one playback-consumption interval.
    pub fn set_active(&self, active: bool) {
        let was_active = self.active.swap(active, Ordering::AcqRel);
        if active && !was_active {
            let elapsed_ns =
                self.telemetry.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            self.activation_consumed_frames.store(
                self.telemetry.callback_consumed_frames.load(Ordering::Acquire),
                Ordering::Release,
            );
            self.activation_elapsed_ns.store(elapsed_ns, Ordering::Release);
        }
    }

    pub fn buffered_frames(&self) -> usize {
        self.queue.len() / self.channels.max(1) as usize
    }

    /// Capture callback-consumption and health evidence without touching CPAL.
    pub fn snapshot(&self) -> RealtimeAudioOutputSnapshot {
        let last_elapsed_ns = self.telemetry.last_callback_elapsed_ns.load(Ordering::Acquire);
        let now_ns = self.telemetry.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let callback_consumed_frames =
            self.telemetry.callback_consumed_frames.load(Ordering::Acquire);
        let active = self.active.load(Ordering::Acquire);
        RealtimeAudioOutputSnapshot {
            stream_generation: self.telemetry.stream_generation,
            sample_rate: self.sample_rate,
            channels: self.channels,
            callback_consumed_frames,
            active_callback_consumed_frames: callback_consumed_frames
                .saturating_sub(self.activation_consumed_frames.load(Ordering::Acquire)),
            active_duration: active.then(|| {
                Duration::from_nanos(
                    now_ns.saturating_sub(self.activation_elapsed_ns.load(Ordering::Acquire)),
                )
            }),
            callback_count: self.telemetry.callback_count.load(Ordering::Relaxed),
            underrun_frames: self.telemetry.underrun_frames.load(Ordering::Relaxed),
            last_callback_frames: self
                .telemetry
                .last_callback_frames
                .load(Ordering::Relaxed)
                .min(u32::MAX as u64) as u32,
            last_callback_playback_delay: (last_elapsed_ns > 0).then(|| {
                Duration::from_nanos(
                    self.telemetry.last_callback_playback_delay_ns.load(Ordering::Relaxed),
                )
            }),
            last_callback_age: (last_elapsed_ns > 0)
                .then(|| Duration::from_nanos(now_ns.saturating_sub(last_elapsed_ns))),
            buffered_frames: self.buffered_frames(),
            stream_failed: self.telemetry.stream_failed.load(Ordering::Acquire),
            active,
        }
    }
}

impl RealtimeAudioOutputHandle {
    pub(crate) fn enqueue(&self, buffer: &AudioBuffer) {
        for sample in &buffer.samples {
            let mut pending = *sample;
            loop {
                match self.queue.push(pending) {
                    Ok(()) => break,
                    Err(returned) => {
                        pending = returned;
                        let _ = self.queue.pop();
                    }
                }
            }
        }
    }

    pub(crate) fn clear(&self) {
        while self.queue.pop().is_some() {}
    }

    pub(crate) fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub(crate) fn set_active(&self, active: bool) {
        let was_active = self.active.swap(active, Ordering::AcqRel);
        if active && !was_active {
            let elapsed_ns =
                self.telemetry.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            self.activation_consumed_frames.store(
                self.telemetry.callback_consumed_frames.load(Ordering::Acquire),
                Ordering::Release,
            );
            self.activation_elapsed_ns.store(elapsed_ns, Ordering::Release);
        }
    }

    pub(crate) fn buffered_frames(&self) -> usize {
        self.queue.len() / self.channels.max(1) as usize
    }

    pub(crate) fn snapshot(&self) -> RealtimeAudioOutputSnapshot {
        let last_elapsed_ns = self.telemetry.last_callback_elapsed_ns.load(Ordering::Acquire);
        let now_ns = self.telemetry.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let callback_consumed_frames =
            self.telemetry.callback_consumed_frames.load(Ordering::Acquire);
        let active = self.active.load(Ordering::Acquire);
        RealtimeAudioOutputSnapshot {
            stream_generation: self.telemetry.stream_generation,
            sample_rate: self.sample_rate,
            channels: self.channels,
            callback_consumed_frames,
            active_callback_consumed_frames: callback_consumed_frames
                .saturating_sub(self.activation_consumed_frames.load(Ordering::Acquire)),
            active_duration: active.then(|| {
                Duration::from_nanos(
                    now_ns.saturating_sub(self.activation_elapsed_ns.load(Ordering::Acquire)),
                )
            }),
            callback_count: self.telemetry.callback_count.load(Ordering::Relaxed),
            underrun_frames: self.telemetry.underrun_frames.load(Ordering::Relaxed),
            last_callback_frames: self
                .telemetry
                .last_callback_frames
                .load(Ordering::Relaxed)
                .min(u32::MAX as u64) as u32,
            last_callback_playback_delay: (last_elapsed_ns > 0).then(|| {
                Duration::from_nanos(
                    self.telemetry.last_callback_playback_delay_ns.load(Ordering::Relaxed),
                )
            }),
            last_callback_age: (last_elapsed_ns > 0)
                .then(|| Duration::from_nanos(now_ns.saturating_sub(last_elapsed_ns))),
            buffered_frames: self.buffered_frames(),
            stream_failed: self.telemetry.stream_failed.load(Ordering::Acquire),
            active,
        }
    }
}

fn build_f32_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: Arc<ArrayQueue<f32>>,
    muted: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    telemetry: Arc<RealtimeAudioOutputTelemetry>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> std::result::Result<cpal::Stream, cpal::BuildStreamError> {
    let channels = config.channels.max(1) as usize;
    device.build_output_stream(
        config,
        move |data: &mut [f32], info| {
            let frames = data.len() / channels;
            let playback_delay = callback_playback_delay(info);
            if muted.load(Ordering::Relaxed) || !active.load(Ordering::Relaxed) {
                data.fill(0.0);
                telemetry.record_callback(frames, 0, playback_delay);
                return;
            }
            let mut missing_samples = 0usize;
            for s in data {
                *s = queue.pop().unwrap_or_else(|| {
                    missing_samples = missing_samples.saturating_add(1);
                    0.0
                });
            }
            telemetry.record_callback(frames, missing_samples / channels, playback_delay);
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
        .arg(match channel_layout {
            AudioChannelLayout::Mono => "mono",
            AudioChannelLayout::Stereo => "stereo",
            AudioChannelLayout::Surround51 => "5.1(side)",
        })
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
    #[test]
    fn callback_telemetry_accumulates_consumption_and_underrun_without_locking() {
        let telemetry = RealtimeAudioOutputTelemetry::new();

        telemetry.record_callback(480, 0, Duration::from_millis(10));
        telemetry.record_callback(480, 32, Duration::from_millis(12));

        assert_eq!(
            telemetry.callback_consumed_frames.load(Ordering::Relaxed),
            960
        );
        assert_eq!(telemetry.callback_count.load(Ordering::Relaxed), 2);
        assert_eq!(telemetry.underrun_frames.load(Ordering::Relaxed), 32);
        assert_eq!(telemetry.last_callback_frames.load(Ordering::Relaxed), 480);
        assert_eq!(
            telemetry.last_callback_playback_delay_ns.load(Ordering::Relaxed),
            12_000_000
        );
    }
}
