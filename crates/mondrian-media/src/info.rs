//! 媒体文件元数据探针
//!
//! 使用 FFmpeg `avformat_open_input` 读取媒体文件的流信息，
//! 不进行解码，仅提取元数据。

use ffmpeg_next as ffmpeg;
use mondrian_core::types::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::Instant;

// ─── 视频编解码器 ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    H265,
    Av1,
    Vp9,
    ProRes(ProResVariant),
    DnxHd,
    DnxHr,
    Cineform,
    Raw,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProResVariant {
    Proxy,
    Lt,
    Standard,
    Hq,
    R4444,
    R4444Xq,
}

// ─── 音频编解码器 ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioCodec {
    Aac,
    Mp3,
    Flac,
    Pcm { bit_depth: u8 },
    Opus,
    Vorbis,
    Other(String),
}

// ─── 像素格式 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    Yuv420p,
    Yuv422p,
    Yuv444p,
    Yuv420p10le,
    Yuv422p10le,
    Yuv444p10le,
    Rgb24,
    Rgba,
    Nv12, // GPU 硬解常见格式
    P010, // 10-bit NV12
}

impl PixelFormat {
    pub fn bit_depth(self) -> u8 {
        match self {
            Self::Yuv420p10le | Self::Yuv422p10le | Self::Yuv444p10le | Self::P010 => 10,
            _ => 8,
        }
    }

    pub fn has_alpha(self) -> bool {
        matches!(self, Self::Rgba)
    }
}

// ─── 声道布局 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelLayout {
    Mono,
    Stereo,
    Surround51,
    Surround71,
    Other(u8),
}

// ─── 流信息 ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoStreamInfo {
    pub index: u32,
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    pub pixel_format: PixelFormat,
    pub color_space: ColorSpace,
    pub bit_depth: u8,
    pub has_alpha: bool,
    pub avg_bitrate: u64, // bits/s
    pub total_frames: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStreamInfo {
    pub index: u32,
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub channels: u8,
    pub channel_layout: ChannelLayout,
    pub bit_depth: u16,
    pub avg_bitrate: u64,
}

// ─── MediaInfo ────────────────────────────────────────────────────────────────

/// 媒体文件完整元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub path: PathBuf,
    pub duration: Duration,
    pub file_size: u64,
    pub container: String, // "mp4", "mov", "mkv", ...
    pub video_streams: Vec<VideoStreamInfo>,
    pub audio_streams: Vec<AudioStreamInfo>,
    /// 是否有视频流
    pub has_video: bool,
    /// 是否有音频流
    pub has_audio: bool,
}

impl MediaInfo {
    /// 探针媒体文件（同步，通过 FFmpeg AVFormatContext）
    ///
    /// # 注意
    /// 此函数在后台线程调用，避免阻塞 UI 线程。
    pub fn probe(path: &Path) -> mondrian_core::Result<Self> {
        let started_at = Instant::now();
        tracing::info!("Probing media file: {:?}", path);

        if !path.exists() {
            return Err(mondrian_core::MondrianError::MediaOpen {
                path: path.display().to_string(),
                reason: "文件不存在".to_string(),
            });
        }

        let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        ffmpeg::init().map_err(|e| mondrian_core::MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: format!("ffmpeg init failed: {e}"),
        })?;

        let input =
            ffmpeg::format::input(path).map_err(|e| mondrian_core::MondrianError::MediaOpen {
                path: path.display().to_string(),
                reason: e.to_string(),
            })?;

        let duration = if input.duration() > 0 {
            Duration::from_micros(input.duration() as u64)
        } else {
            Duration::ZERO
        };

        let container = input.format().name().to_lowercase();

        let mut video_streams = Vec::new();
        let mut audio_streams = Vec::new();

        for stream in input.streams() {
            let params = stream.parameters();
            match params.medium() {
                ffmpeg::media::Type::Video => {
                    let mut width = 0;
                    let mut height = 0;
                    let mut pixel_format = PixelFormat::Yuv420p;
                    let mut bit_depth = 8;
                    let mut has_alpha = false;

                    if let Ok(context) =
                        ffmpeg::codec::context::Context::from_parameters(params.clone())
                    {
                        if let Ok(decoder) = context.decoder().video() {
                            width = decoder.width();
                            height = decoder.height();
                            pixel_format = map_pixel_format(decoder.format());
                            bit_depth = pixel_format.bit_depth();
                            has_alpha = pixel_format.has_alpha();
                        }
                    }

                    let frame_rate = map_rational(stream.avg_frame_rate());
                    let total_frames = if stream.frames() > 0 {
                        Some(stream.frames() as u64)
                    } else {
                        None
                    };

                    video_streams.push(VideoStreamInfo {
                        index: stream.index() as u32,
                        codec: map_video_codec(params.id()),
                        width,
                        height,
                        frame_rate,
                        pixel_format,
                        color_space: ColorSpace::Rec709,
                        bit_depth,
                        has_alpha,
                        avg_bitrate: 0,
                        total_frames,
                    });
                }
                ffmpeg::media::Type::Audio => {
                    let mut sample_rate = 0u32;
                    let mut channels = 0u8;
                    let mut channel_layout = ChannelLayout::Other(0);
                    let mut bit_depth = 16u16;

                    if let Ok(context) =
                        ffmpeg::codec::context::Context::from_parameters(params.clone())
                    {
                        if let Ok(decoder) = context.decoder().audio() {
                            sample_rate = decoder.rate() as u32;
                            channels = decoder.channels() as u8;
                            channel_layout = map_channel_layout(channels);
                            bit_depth = 16;
                        }
                    }

                    audio_streams.push(AudioStreamInfo {
                        index: stream.index() as u32,
                        codec: map_audio_codec(params.id()),
                        sample_rate,
                        channels,
                        channel_layout,
                        bit_depth,
                        avg_bitrate: 0,
                    });
                }
                _ => {}
            }
        }

        let info = Self {
            path: path.to_path_buf(),
            duration,
            file_size,
            container,
            has_video: !video_streams.is_empty(),
            has_audio: !audio_streams.is_empty(),
            video_streams,
            audio_streams,
        };

        let elapsed_ms = started_at.elapsed().as_millis() as u64;
        if elapsed_ms >= media_probe_slow_threshold_ms() {
            tracing::warn!(
                "[media-probe] slow probe: {}ms path={:?} has_video={} has_audio={} streams(v/a)={}/{}",
                elapsed_ms,
                path,
                info.has_video,
                info.has_audio,
                info.video_streams.len(),
                info.audio_streams.len(),
            );
        } else {
            tracing::info!("[media-probe] done: {}ms path={:?}", elapsed_ms, path);
        }

        Ok(info)
    }

    /// 获取主要视频流（第一个视频流）
    pub fn primary_video(&self) -> Option<&VideoStreamInfo> {
        self.video_streams.first()
    }

    /// 获取主要音频流（第一个音频流）
    pub fn primary_audio(&self) -> Option<&AudioStreamInfo> {
        self.audio_streams.first()
    }

    /// 获取视频总帧数（估算）
    pub fn estimated_frames(&self) -> Option<u64> {
        let video = self.primary_video()?;
        let fps = video.frame_rate.to_f64();
        let secs = self.duration.as_secs_f64();
        Some((secs * fps).ceil() as u64)
    }
}

fn map_rational(value: ffmpeg::Rational) -> Rational {
    let num = value.numerator();
    let den = value.denominator();
    if den == 0 {
        Rational::FPS_25
    } else {
        Rational::new(num as i64, den as i64)
    }
}

fn map_pixel_format(pixel: ffmpeg::util::format::pixel::Pixel) -> PixelFormat {
    use ffmpeg::util::format::pixel::Pixel;

    match pixel {
        Pixel::YUV420P => PixelFormat::Yuv420p,
        Pixel::YUV422P => PixelFormat::Yuv422p,
        Pixel::YUV444P => PixelFormat::Yuv444p,
        Pixel::YUV420P10LE => PixelFormat::Yuv420p10le,
        Pixel::YUV422P10LE => PixelFormat::Yuv422p10le,
        Pixel::YUV444P10LE => PixelFormat::Yuv444p10le,
        Pixel::RGB24 => PixelFormat::Rgb24,
        Pixel::RGBA => PixelFormat::Rgba,
        Pixel::NV12 => PixelFormat::Nv12,
        Pixel::P010LE => PixelFormat::P010,
        _ => PixelFormat::Yuv420p,
    }
}

fn map_channel_layout(channels: u8) -> ChannelLayout {
    match channels {
        1 => ChannelLayout::Mono,
        2 => ChannelLayout::Stereo,
        6 => ChannelLayout::Surround51,
        8 => ChannelLayout::Surround71,
        _ => ChannelLayout::Other(channels),
    }
}

fn map_video_codec(id: ffmpeg::codec::Id) -> VideoCodec {
    use ffmpeg::codec::Id;

    match id {
        Id::H264 => VideoCodec::H264,
        Id::HEVC => VideoCodec::H265,
        Id::AV1 => VideoCodec::Av1,
        Id::VP9 => VideoCodec::Vp9,
        Id::PRORES => VideoCodec::ProRes(ProResVariant::Standard),
        Id::DNXHD => VideoCodec::DnxHd,
        Id::CFHD => VideoCodec::Cineform,
        Id::RAWVIDEO => VideoCodec::Raw,
        other => VideoCodec::Other(format!("{other:?}")),
    }
}

fn media_probe_slow_threshold_ms() -> u64 {
    static THRESHOLD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        std::env::var("MONDRIAN_MEDIA_PROBE_SLOW_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value >= 10)
            .unwrap_or(120)
    })
}

fn map_audio_codec(id: ffmpeg::codec::Id) -> AudioCodec {
    use ffmpeg::codec::Id;

    match id {
        Id::AAC => AudioCodec::Aac,
        Id::MP3 => AudioCodec::Mp3,
        Id::FLAC => AudioCodec::Flac,
        Id::OPUS => AudioCodec::Opus,
        Id::VORBIS => AudioCodec::Vorbis,
        Id::PCM_S16LE => AudioCodec::Pcm { bit_depth: 16 },
        Id::PCM_S24LE => AudioCodec::Pcm { bit_depth: 24 },
        Id::PCM_S32LE => AudioCodec::Pcm { bit_depth: 32 },
        other => AudioCodec::Other(format!("{other:?}")),
    }
}
