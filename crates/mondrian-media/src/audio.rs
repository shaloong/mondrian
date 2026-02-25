//! 音频缓冲区与混合器

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use mondrian_core::{MondrianError, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 原始 PCM 音频缓冲区（f32 交错格式）
#[derive(Debug, Clone)]
pub struct AudioBuffer {
    /// 交错 PCM f32 样本 [L0, R0, L1, R1, ...]
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u8,
}

impl AudioBuffer {
    pub fn silent(sample_rate: u32, channels: u8, frames: usize) -> Self {
        Self {
            samples: vec![0.0; frames * channels as usize],
            sample_rate,
            channels,
        }
    }

    /// 帧数（样本数 / 声道数）
    pub fn frame_count(&self) -> usize {
        self.samples.len() / self.channels as usize
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
        let channels = self.channels as usize;
        let total_frames = self.frame_count();
        if start_frame >= total_frames || frame_count == 0 {
            return Self::silent(self.sample_rate, self.channels, 0);
        }

        let end_frame = (start_frame + frame_count).min(total_frames);
        let start = start_frame * channels;
        let end = end_frame * channels;

        Self {
            samples: self.samples[start..end].to_vec(),
            sample_rate: self.sample_rate,
            channels: self.channels,
        }
    }
}

/// 多轨音频混合器
pub struct AudioMixer {
    pub output_sample_rate: u32,
    pub output_channels: u8,
}

pub struct RealtimeAudioOutput {
    sample_rate: u32,
    channels: u8,
    queue: Arc<Mutex<VecDeque<f32>>>,
    _stream: cpal::Stream,
}

pub struct AudioSourceCache {
    sample_rate: u32,
    channels: u8,
    decoded: Mutex<HashMap<PathBuf, Arc<AudioBuffer>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockRole {
    VideoMaster,
    AudioMaster,
}

#[derive(Debug, Clone)]
pub struct AudioClock {
    pub sample_rate: u32,
    started_at: Instant,
    offset_samples: i64,
}

impl AudioClock {
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

#[derive(Debug, Clone)]
pub struct SyncCorrection {
    pub drift_seconds: f64,
    pub playback_rate: f64,
    pub padding_frames: usize,
    pub drop_frames: usize,
}

/// 音视频主从时钟同步器。
///
/// 默认以视频为主时钟，对音频进行轻微速率修正；当漂移过大时退化为补零/丢帧。
pub struct AudioSyncController {
    pub role: ClockRole,
    pub max_soft_drift: Duration,
    pub max_hard_drift: Duration,
    pub no_sync_threshold: Duration,
    pub max_rate_adjust_percent: f64,
}

impl Default for AudioSyncController {
    fn default() -> Self {
        Self {
            role: ClockRole::AudioMaster,
            max_soft_drift: Duration::from_millis(40),
            max_hard_drift: Duration::from_millis(100),
            no_sync_threshold: Duration::from_secs(10),
            max_rate_adjust_percent: 3.0,
        }
    }
}

impl AudioSyncController {
    pub fn compute_correction(
        &self,
        master_seconds: f64,
        slave_seconds: f64,
        sample_rate: u32,
    ) -> SyncCorrection {
        let drift = master_seconds - slave_seconds;
        let abs = drift.abs();
        let soft = self.max_soft_drift.as_secs_f64();
        let hard = self.max_hard_drift.as_secs_f64();
        let no_sync = self.no_sync_threshold.as_secs_f64();
        let sr = sample_rate as f64;

        if abs >= no_sync {
            return SyncCorrection {
                drift_seconds: drift,
                playback_rate: 1.0,
                padding_frames: 0,
                drop_frames: 0,
            };
        }

        if abs <= soft {
            return SyncCorrection {
                drift_seconds: drift,
                playback_rate: 1.0,
                padding_frames: 0,
                drop_frames: 0,
            };
        }

        if abs >= hard {
            let frames = (abs * sr).round().max(0.0) as usize;
            return if drift > 0.0 {
                SyncCorrection {
                    drift_seconds: drift,
                    playback_rate: 1.0,
                    padding_frames: frames,
                    drop_frames: 0,
                }
            } else {
                SyncCorrection {
                    drift_seconds: drift,
                    playback_rate: 1.0,
                    padding_frames: 0,
                    drop_frames: frames,
                }
            };
        }

        let adjust_ratio = (drift / hard) * (self.max_rate_adjust_percent / 100.0);
        SyncCorrection {
            drift_seconds: drift,
            playback_rate: (1.0 + adjust_ratio).clamp(
                1.0 - self.max_rate_adjust_percent / 100.0,
                1.0 + self.max_rate_adjust_percent / 100.0,
            ),
            padding_frames: 0,
            drop_frames: 0,
        }
    }
}

impl AudioMixer {
    pub fn new(sample_rate: u32, channels: u8) -> Self {
        Self {
            output_sample_rate: sample_rate,
            output_channels: channels,
        }
    }

    /// 混合多个音频轨道
    pub fn mix(&self, tracks: &[AudioTrackData]) -> AudioBuffer {
        if tracks.is_empty() {
            return AudioBuffer::silent(self.output_sample_rate, self.output_channels, 0);
        }

        let has_solo = tracks.iter().any(|t| t.config.is_solo && !t.config.is_muted);

        let max_frames = tracks.iter().map(|t| t.buffer.frame_count()).max().unwrap_or(0);

        let mut output =
            AudioBuffer::silent(self.output_sample_rate, self.output_channels, max_frames);

        for track in tracks {
            if track.config.is_muted {
                continue;
            }
            if has_solo && !track.config.is_solo {
                continue;
            }
            self.mix_track_with_pan(&mut output, &track.buffer, &track.config);
        }

        self.soft_clip(&mut output);
        output
    }

    pub fn apply_clock_correction(
        &self,
        mut buffer: AudioBuffer,
        correction: &SyncCorrection,
    ) -> AudioBuffer {
        if correction.drop_frames > 0 {
            let samples_to_drop = correction.drop_frames.saturating_mul(buffer.channels as usize);
            if samples_to_drop < buffer.samples.len() {
                buffer.samples.drain(0..samples_to_drop);
            } else {
                buffer.samples.clear();
            }
        }

        if correction.padding_frames > 0 {
            let extra = correction.padding_frames.saturating_mul(buffer.channels as usize);
            let mut padded = vec![0.0f32; extra];
            padded.extend_from_slice(&buffer.samples);
            buffer.samples = padded;
        }

        buffer
    }

    fn mix_track_with_pan(
        &self,
        output: &mut AudioBuffer,
        source: &AudioBuffer,
        config: &AudioTrackConfig,
    ) {
        let src_channels = source.channels as usize;
        let dst_channels = output.channels as usize;
        let frames = source.frame_count().min(output.frame_count());
        let pan = config.pan.clamp(-1.0, 1.0);
        let left_gain = ((1.0 - pan) * 0.5).sqrt();
        let right_gain = ((1.0 + pan) * 0.5).sqrt();

        for frame in 0..frames {
            let src_base = frame * src_channels;
            let dst_base = frame * dst_channels;

            let src_l = source.samples.get(src_base).copied().unwrap_or(0.0);
            let src_r = if src_channels > 1 {
                source.samples.get(src_base + 1).copied().unwrap_or(src_l)
            } else {
                src_l
            };

            if dst_channels >= 2 {
                output.samples[dst_base] += src_l * config.volume * left_gain;
                output.samples[dst_base + 1] += src_r * config.volume * right_gain;
            } else if dst_channels == 1 {
                output.samples[dst_base] += ((src_l + src_r) * 0.5) * config.volume;
            }
        }
    }

    fn soft_clip(&self, output: &mut AudioBuffer) {
        for sample in &mut output.samples {
            *sample = sample.tanh();
        }
    }
}

impl RealtimeAudioOutput {
    pub fn try_new(sample_rate: u32, channels: u8) -> Result<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or_else(|| MondrianError::Other(
            anyhow::anyhow!("未找到默认音频输出设备"),
        ))?;

        let config = cpal::StreamConfig {
            channels: channels.max(1) as u16,
            sample_rate: cpal::SampleRate(sample_rate.max(8_000)),
            buffer_size: cpal::BufferSize::Default,
        };

        let queue = Arc::new(Mutex::new(VecDeque::with_capacity(sample_rate as usize)));
        let queue_for_cb = Arc::clone(&queue);
        let err_fn = |err| tracing::error!("音频输出流错误: {}", err);

        let default_config = device.default_output_config().map_err(|e| MondrianError::Other(
            anyhow::anyhow!("读取默认输出配置失败: {e}"),
        ))?;

        let stream = match default_config.sample_format() {
            cpal::SampleFormat::F32 => build_f32_stream(&device, &config, queue_for_cb, err_fn)
                .map_err(|e| MondrianError::Other(anyhow::anyhow!("创建 F32 输出流失败: {e}")))?,
            cpal::SampleFormat::I16 => {
                let queue_for_cb = Arc::clone(&queue);
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [i16], _| {
                            let mut guard = queue_for_cb.lock();
                            for s in data {
                                let v = guard.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0);
                                *s = (v * i16::MAX as f32) as i16;
                            }
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| MondrianError::Other(anyhow::anyhow!("创建 I16 输出流失败: {e}")))?
            }
            cpal::SampleFormat::U16 => {
                let queue_for_cb = Arc::clone(&queue);
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [u16], _| {
                            let mut guard = queue_for_cb.lock();
                            for s in data {
                                let v = guard.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0);
                                *s = ((v * 0.5 + 0.5) * u16::MAX as f32) as u16;
                            }
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| MondrianError::Other(anyhow::anyhow!("创建 U16 输出流失败: {e}")))?
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
            _stream: stream,
        })
    }

    pub fn enqueue(&self, buffer: &AudioBuffer) {
        if buffer.samples.is_empty() {
            return;
        }
        let mut guard = self.queue.lock();
        guard.extend(buffer.samples.iter().copied());

        let max_samples = self.sample_rate as usize * self.channels as usize * 2;
        while guard.len() > max_samples {
            let _ = guard.pop_front();
        }
    }

    pub fn clear(&self) {
        self.queue.lock().clear();
    }

    pub fn buffered_frames(&self) -> usize {
        self.queue.lock().len() / self.channels.max(1) as usize
    }
}

impl AudioSourceCache {
    pub fn new(sample_rate: u32, channels: u8) -> Self {
        Self {
            sample_rate,
            channels: channels.max(1),
            decoded: Mutex::new(HashMap::new()),
        }
    }

    pub fn get_or_decode(&self, path: &Path) -> Result<Arc<AudioBuffer>> {
        if let Some(hit) = self.decoded.lock().get(path).cloned() {
            return Ok(hit);
        }

        let decoded = Arc::new(decode_audio_file_with_ffmpeg_cli(
            path,
            self.sample_rate,
            self.channels,
        )?);

        self.decoded
            .lock()
            .insert(path.to_path_buf(), Arc::clone(&decoded));
        Ok(decoded)
    }

    pub fn cache_entry_count(&self) -> usize {
        self.decoded.lock().len()
    }

    pub fn clear(&self) {
        self.decoded.lock().clear();
    }
}

fn build_f32_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: Arc<Mutex<VecDeque<f32>>>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> std::result::Result<cpal::Stream, cpal::BuildStreamError> {
    device.build_output_stream(
        config,
        move |data: &mut [f32], _| {
            let mut guard = queue.lock();
            for s in data {
                *s = guard.pop_front().unwrap_or(0.0);
            }
        },
        err_fn,
        None,
    )
}

fn decode_audio_file_with_ffmpeg_cli(path: &Path, sample_rate: u32, channels: u8) -> Result<AudioBuffer> {
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
        .arg("-ac")
        .arg(channels.max(1).to_string())
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
        channels: channels.max(1),
    })
}

/// 单轨音频数据
pub struct AudioTrackData {
    pub buffer: AudioBuffer,
    pub config: AudioTrackConfig,
}

/// 轨道配置（用于 serde 持久化）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioTrackConfig {
    pub volume: f32,
    pub pan: f32, // -1.0 左声道 ~ 1.0 右声道
    pub is_muted: bool,
    pub is_solo: bool,
}

impl Default for AudioTrackConfig {
    fn default() -> Self {
        Self {
            volume: 1.0,
            pan: 0.0,
            is_muted: false,
            is_solo: false,
        }
    }
}
