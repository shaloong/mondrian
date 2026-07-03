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

/// How a video stream's input color metadata was resolved by media probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorSpaceSource {
    /// Container/codec metadata explicitly identified the color space.
    Metadata,
    /// Metadata was missing or unsupported; callers must apply missing-metadata policy.
    MissingMetadata,
    /// FFmpeg could not open a decoder; callers must apply missing-metadata policy.
    DecoderUnavailable,
}

/// Method that produced a video stream's color metadata decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorDetectionMethod {
    /// Explicit acquisition/log metadata hint won.
    MetadataHint,
    /// Raw CICP/FFmpeg color tags resolved to a supported color space.
    CicpTags,
    /// No supported color metadata was found.
    MissingMetadata,
    /// Decoder could not be opened, so no color metadata could be inspected.
    DecoderUnavailable,
}

/// One raw CICP-style color tag reported by FFmpeg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoColorTag {
    /// Numeric CICP/FFmpeg enum value.
    pub code: i32,
    /// Stable FFmpeg tag name when FFmpeg exposes one.
    pub name: Option<String>,
    /// Whether this tag carries an explicit value rather than `unspecified`.
    pub specified: bool,
}

/// Raw container/codec color metadata for a decoded video stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoColorMetadata {
    /// Color primaries tag.
    pub primaries: VideoColorTag,
    /// Transfer characteristic tag.
    pub transfer: VideoColorTag,
    /// Matrix coefficients tag.
    pub matrix: VideoColorTag,
}

/// Scope where an acquisition/color metadata hint was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoColorMetadataHintScope {
    /// Container-level metadata.
    Container,
    /// Video stream-level metadata.
    Stream,
}

/// Metadata hint that identifies an acquisition or camera-log color space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoColorMetadataHint {
    /// Metadata scope.
    pub scope: VideoColorMetadataHintScope,
    /// Original metadata key.
    pub key: String,
    /// Original metadata value.
    pub value: String,
    /// Color space identified by the hint.
    pub detected_color_space: ColorSpace,
}

/// HDR-related stream side-data kind detected during media probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoHdrSideDataKind {
    /// SMPTE ST 2086 mastering display metadata.
    MasteringDisplayMetadata,
    /// MaxCLL / MaxFALL content light level metadata.
    ContentLightLevel,
    /// HDR10+ dynamic metadata.
    DynamicHdr10Plus,
    /// Dolby Vision configuration metadata.
    DolbyVisionConfig,
    /// ICC profile side data.
    IccProfile,
}

/// Summary of HDR-related side data on a video stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoHdrMetadataSummary {
    /// HDR side-data kind.
    pub kind: VideoHdrSideDataKind,
    /// Side-data payload size in bytes.
    pub payload_size: usize,
}

/// Diagnostic snapshot of a video stream's color metadata interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoColorDiagnostic {
    /// Explicitly detected Mondrian color space, if one was identified.
    pub detected_color_space: Option<ColorSpace>,
    /// Source state for the color metadata decision.
    pub source: VideoColorSpaceSource,
    /// Method that produced the color metadata decision.
    pub method: VideoColorDetectionMethod,
    /// Raw CICP-style metadata captured from FFmpeg, when available.
    pub metadata: Option<VideoColorMetadata>,
    /// Metadata hints that contributed to identifying acquisition/log color space.
    #[serde(default)]
    pub metadata_hints: Vec<VideoColorMetadataHint>,
    /// HDR-related stream side-data summaries.
    #[serde(default)]
    pub hdr_metadata: Vec<VideoHdrMetadataSummary>,
}

impl VideoColorTag {
    /// Compact diagnostic representation for logs and export errors.
    pub fn summary(&self) -> String {
        let name = self.name.as_deref().unwrap_or("unspecified");
        format!("{name}(code={},specified={})", self.code, self.specified)
    }
}

impl VideoColorMetadata {
    /// Compact diagnostic representation for logs and export errors.
    pub fn summary(&self) -> String {
        format!(
            "primaries={},transfer={},matrix={}",
            self.primaries.summary(),
            self.transfer.summary(),
            self.matrix.summary()
        )
    }
}

impl VideoColorDiagnostic {
    /// Build a color diagnostic snapshot from probed stream metadata.
    pub fn from_stream(stream: &VideoStreamInfo) -> Self {
        Self {
            detected_color_space: stream.detected_color_space,
            source: stream.color_space_source,
            method: stream.color_detection_method,
            metadata: stream.color_metadata.clone(),
            metadata_hints: stream.color_metadata_hints.clone(),
            hdr_metadata: stream.hdr_metadata.clone(),
        }
    }

    /// Compact diagnostic representation for logs and export errors.
    pub fn summary(&self) -> String {
        let detected = self
            .detected_color_space
            .map(|color_space| format!("{color_space:?}"))
            .unwrap_or_else(|| "None".to_string());
        let metadata = self
            .metadata
            .as_ref()
            .map(VideoColorMetadata::summary)
            .unwrap_or_else(|| "unavailable".to_string());
        let hints = if self.metadata_hints.is_empty() {
            "none".to_string()
        } else {
            self.metadata_hints
                .iter()
                .map(VideoColorMetadataHint::summary)
                .collect::<Vec<_>>()
                .join("|")
        };
        let hdr = if self.hdr_metadata.is_empty() {
            "none".to_string()
        } else {
            self.hdr_metadata
                .iter()
                .map(VideoHdrMetadataSummary::summary)
                .collect::<Vec<_>>()
                .join("|")
        };
        format!(
            "source={:?},method={:?},detected={},metadata={},hints={},hdr={}",
            self.source, self.method, detected, metadata, hints, hdr
        )
    }
}

impl VideoColorMetadataHint {
    /// Compact diagnostic representation for logs and export errors.
    pub fn summary(&self) -> String {
        format!(
            "{:?}:{}={}->{:?}",
            self.scope, self.key, self.value, self.detected_color_space
        )
    }
}

impl VideoHdrMetadataSummary {
    /// Compact diagnostic representation for logs and export errors.
    pub fn summary(&self) -> String {
        format!("{:?}(bytes={})", self.kind, self.payload_size)
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
    /// Color space explicitly detected from container/codec metadata, if present.
    pub detected_color_space: Option<ColorSpace>,
    /// Source of the detected color-space result.
    pub color_space_source: VideoColorSpaceSource,
    /// Method that produced the detected color-space result.
    pub color_detection_method: VideoColorDetectionMethod,
    /// Raw CICP-style color metadata reported by FFmpeg when the decoder opens.
    pub color_metadata: Option<VideoColorMetadata>,
    /// Acquisition/log metadata hints captured from container and stream metadata.
    #[serde(default)]
    pub color_metadata_hints: Vec<VideoColorMetadataHint>,
    /// HDR-related stream side-data summaries.
    #[serde(default)]
    pub hdr_metadata: Vec<VideoHdrMetadataSummary>,
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
    pub fn synthetic_adjustment_layer() -> Self {
        Self {
            path: PathBuf::from("mondrian://adjustment-layer"),
            duration: Duration::ZERO,
            file_size: 0,
            container: "adjustment-layer".to_string(),
            video_streams: Vec::new(),
            audio_streams: Vec::new(),
            has_video: false,
            has_audio: false,
        }
    }

    pub fn synthetic_solid_color() -> Self {
        Self {
            path: PathBuf::from("mondrian://solid-color"),
            duration: Duration::ZERO,
            file_size: 0,
            container: "solid-color".to_string(),
            video_streams: Vec::new(),
            audio_streams: Vec::new(),
            has_video: false,
            has_audio: false,
        }
    }

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
        let container_color_hints =
            collect_color_metadata_hints(VideoColorMetadataHintScope::Container, &input.metadata());

        let mut video_streams = Vec::new();
        let mut audio_streams = Vec::new();

        for stream in input.streams() {
            let params = stream.parameters();
            match params.medium() {
                ffmpeg::media::Type::Video => {
                    let mut color_metadata_hints = collect_color_metadata_hints(
                        VideoColorMetadataHintScope::Stream,
                        &stream.metadata(),
                    );
                    color_metadata_hints.extend(container_color_hints.clone());
                    let hdr_metadata = collect_hdr_metadata_summaries(&stream);
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
                            let raw_color_metadata = capture_color_metadata(
                                decoder.color_primaries(),
                                decoder.color_transfer_characteristic(),
                                decoder.color_space(),
                            );
                            let color_metadata = detect_color_space_from_metadata(
                                &raw_color_metadata,
                                &color_metadata_hints,
                            );
                            video_streams.push(VideoStreamInfo {
                                index: stream.index() as u32,
                                codec: map_video_codec(params.id()),
                                width,
                                height,
                                frame_rate: map_rational(stream.avg_frame_rate()),
                                pixel_format,
                                detected_color_space: color_metadata.detected,
                                color_space_source: color_metadata.source,
                                color_detection_method: color_metadata.method,
                                color_metadata: Some(raw_color_metadata),
                                color_metadata_hints,
                                hdr_metadata,
                                bit_depth,
                                has_alpha,
                                avg_bitrate: 0,
                                total_frames: if stream.frames() > 0 {
                                    Some(stream.frames() as u64)
                                } else {
                                    None
                                },
                            });
                            continue;
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
                        detected_color_space: None,
                        color_space_source: VideoColorSpaceSource::DecoderUnavailable,
                        color_detection_method: VideoColorDetectionMethod::DecoderUnavailable,
                        color_metadata: None,
                        color_metadata_hints,
                        hdr_metadata,
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
                            sample_rate = decoder.rate();
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VideoColorSpaceDetection {
    detected: Option<ColorSpace>,
    source: VideoColorSpaceSource,
    method: VideoColorDetectionMethod,
}

#[cfg(test)]
fn detect_color_space(
    primaries: ffmpeg::util::color::Primaries,
    transfer: ffmpeg::util::color::TransferCharacteristic,
    matrix: ffmpeg::util::color::Space,
) -> VideoColorSpaceDetection {
    let metadata = capture_color_metadata(primaries, transfer, matrix);
    detect_color_space_from_metadata(&metadata, &[])
}

fn detect_color_space_from_metadata(
    metadata: &VideoColorMetadata,
    metadata_hints: &[VideoColorMetadataHint],
) -> VideoColorSpaceDetection {
    if let Some(color_space) = metadata_hints.iter().map(|hint| hint.detected_color_space).next() {
        return VideoColorSpaceDetection {
            detected: Some(color_space),
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::MetadataHint,
        };
    }

    if let Some(color_space) = ColorSpace::from_ffmpeg_tag_hints(
        metadata.primaries.name.as_deref(),
        metadata.transfer.name.as_deref(),
        metadata.matrix.name.as_deref(),
    ) {
        return VideoColorSpaceDetection {
            detected: Some(color_space),
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::CicpTags,
        };
    }

    VideoColorSpaceDetection {
        detected: None,
        source: VideoColorSpaceSource::MissingMetadata,
        method: VideoColorDetectionMethod::MissingMetadata,
    }
}

fn collect_color_metadata_hints(
    scope: VideoColorMetadataHintScope,
    metadata: &ffmpeg::DictionaryRef<'_>,
) -> Vec<VideoColorMetadataHint> {
    metadata
        .iter()
        .filter_map(|(key, value)| detect_color_metadata_hint(scope, key, value))
        .collect()
}

fn detect_color_metadata_hint(
    scope: VideoColorMetadataHintScope,
    key: &str,
    value: &str,
) -> Option<VideoColorMetadataHint> {
    let haystack = normalize_metadata_hint_text(&format!("{key} {value}"));
    let detected_color_space = if contains_any(&haystack, &["applelog", "applelogprofile"]) {
        Some(ColorSpace::AppleLog)
    } else if contains_any(&haystack, &["slog3", "sgamut3cine", "sonyslog3"]) {
        Some(ColorSpace::SLog3)
    } else if contains_any(&haystack, &["logc4", "arrilogc4", "arrilogc"]) {
        Some(ColorSpace::ArriLogC4)
    } else {
        None
    }?;

    Some(VideoColorMetadataHint {
        scope,
        key: key.to_string(),
        value: value.to_string(),
        detected_color_space,
    })
}

fn normalize_metadata_hint_text(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn collect_hdr_metadata_summaries(
    stream: &ffmpeg::format::stream::Stream<'_>,
) -> Vec<VideoHdrMetadataSummary> {
    stream
        .side_data()
        .filter_map(|side_data| {
            map_hdr_side_data_kind(side_data.kind())
                .map(|kind| VideoHdrMetadataSummary { kind, payload_size: side_data.data().len() })
        })
        .collect()
}

fn map_hdr_side_data_kind(
    kind: ffmpeg::codec::packet::side_data::Type,
) -> Option<VideoHdrSideDataKind> {
    use ffmpeg::codec::packet::side_data::Type;

    match kind {
        Type::MasteringDisplayMetadata => Some(VideoHdrSideDataKind::MasteringDisplayMetadata),
        Type::ContentLightLevel => Some(VideoHdrSideDataKind::ContentLightLevel),
        Type::DYNAMIC_HDR10_PLUS => Some(VideoHdrSideDataKind::DynamicHdr10Plus),
        Type::DOVI_CONF => Some(VideoHdrSideDataKind::DolbyVisionConfig),
        Type::ICC_PROFILE => Some(VideoHdrSideDataKind::IccProfile),
        _ => None,
    }
}

fn capture_color_metadata(
    primaries: ffmpeg::util::color::Primaries,
    transfer: ffmpeg::util::color::TransferCharacteristic,
    matrix: ffmpeg::util::color::Space,
) -> VideoColorMetadata {
    VideoColorMetadata {
        primaries: primaries_tag(primaries),
        transfer: transfer_tag(transfer),
        matrix: matrix_tag(matrix),
    }
}

fn primaries_tag(value: ffmpeg::util::color::Primaries) -> VideoColorTag {
    let raw: ffmpeg::ffi::AVColorPrimaries = value.into();
    VideoColorTag {
        code: raw as i32,
        name: value.name().map(str::to_owned),
        specified: value != ffmpeg::util::color::Primaries::Unspecified,
    }
}

fn transfer_tag(value: ffmpeg::util::color::TransferCharacteristic) -> VideoColorTag {
    let raw: ffmpeg::ffi::AVColorTransferCharacteristic = value.into();
    VideoColorTag {
        code: raw as i32,
        name: value.name().map(str::to_owned),
        specified: value != ffmpeg::util::color::TransferCharacteristic::Unspecified,
    }
}

fn matrix_tag(value: ffmpeg::util::color::Space) -> VideoColorTag {
    let raw: ffmpeg::ffi::AVColorSpace = value.into();
    VideoColorTag {
        code: raw as i32,
        name: value.name().map(str::to_owned),
        specified: value != ffmpeg::util::color::Space::Unspecified,
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

#[cfg(test)]
mod tests {
    use super::*;
    use ffmpeg::util::color::{Primaries, Space, TransferCharacteristic};

    #[test]
    fn detect_color_space_marks_hdr_transfer_metadata() {
        let pq = detect_color_space(
            Primaries::BT2020,
            TransferCharacteristic::SMPTE2084,
            Space::BT2020NCL,
        );
        assert_eq!(pq.detected, Some(ColorSpace::Rec2100Pq));
        assert_eq!(pq.source, VideoColorSpaceSource::Metadata);
        assert_eq!(pq.method, VideoColorDetectionMethod::CicpTags);

        let hlg = detect_color_space(
            Primaries::BT2020,
            TransferCharacteristic::ARIB_STD_B67,
            Space::BT2020NCL,
        );
        assert_eq!(hlg.detected, Some(ColorSpace::Rec2100Hlg));
        assert_eq!(hlg.source, VideoColorSpaceSource::Metadata);
        assert_eq!(hlg.method, VideoColorDetectionMethod::CicpTags);
    }

    #[test]
    fn detect_color_space_marks_rgb_bt709_as_srgb_metadata() {
        let detection = detect_color_space(
            Primaries::BT709,
            TransferCharacteristic::Unspecified,
            Space::RGB,
        );

        assert_eq!(detection.detected, Some(ColorSpace::Srgb));
        assert_eq!(detection.source, VideoColorSpaceSource::Metadata);
        assert_eq!(detection.method, VideoColorDetectionMethod::CicpTags);
    }

    #[test]
    fn detect_color_space_uses_matrix_metadata_when_primaries_are_missing() {
        let detection = detect_color_space(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::BT709,
        );

        assert_eq!(detection.detected, Some(ColorSpace::Rec709));
        assert_eq!(detection.source, VideoColorSpaceSource::Metadata);
        assert_eq!(detection.method, VideoColorDetectionMethod::CicpTags);
    }

    #[test]
    fn detect_color_space_does_not_claim_missing_metadata_as_rec709() {
        let detection = detect_color_space(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );

        assert_eq!(detection.detected, None);
        assert_eq!(detection.source, VideoColorSpaceSource::MissingMetadata);
        assert_eq!(detection.method, VideoColorDetectionMethod::MissingMetadata);
    }

    #[test]
    fn metadata_hint_identifies_camera_log_spaces() {
        let slog3 = detect_color_metadata_hint(
            VideoColorMetadataHintScope::Stream,
            "com.sony.colorProfile",
            "S-Log3 / S-Gamut3.Cine",
        )
        .expect("slog3 metadata hint");
        assert_eq!(slog3.detected_color_space, ColorSpace::SLog3);

        let apple_log = detect_color_metadata_hint(
            VideoColorMetadataHintScope::Container,
            "com.apple.proapps.cameraLog",
            "Apple Log",
        )
        .expect("apple log metadata hint");
        assert_eq!(apple_log.detected_color_space, ColorSpace::AppleLog);

        let logc4 = detect_color_metadata_hint(
            VideoColorMetadataHintScope::Stream,
            "camera_profile",
            "ARRI LogC4",
        )
        .expect("arri logc4 metadata hint");
        assert_eq!(logc4.detected_color_space, ColorSpace::ArriLogC4);
    }

    #[test]
    fn metadata_hint_ignores_ambiguous_log_words() {
        assert_eq!(
            detect_color_metadata_hint(VideoColorMetadataHintScope::Container, "log", "enabled"),
            None
        );
    }

    #[test]
    fn metadata_hint_overrides_cicp_delivery_tags_for_camera_log() {
        let metadata = capture_color_metadata(
            Primaries::BT709,
            TransferCharacteristic::BT709,
            Space::BT709,
        );
        let hint = VideoColorMetadataHint {
            scope: VideoColorMetadataHintScope::Stream,
            key: "camera_profile".to_string(),
            value: "ARRI LogC4".to_string(),
            detected_color_space: ColorSpace::ArriLogC4,
        };

        let detection = detect_color_space_from_metadata(&metadata, &[hint]);

        assert_eq!(detection.detected, Some(ColorSpace::ArriLogC4));
        assert_eq!(detection.source, VideoColorSpaceSource::Metadata);
        assert_eq!(detection.method, VideoColorDetectionMethod::MetadataHint);
    }

    #[test]
    fn hdr_side_data_kind_mapping_identifies_static_and_dynamic_hdr_metadata() {
        use ffmpeg::codec::packet::side_data::Type;

        assert_eq!(
            map_hdr_side_data_kind(Type::MasteringDisplayMetadata),
            Some(VideoHdrSideDataKind::MasteringDisplayMetadata)
        );
        assert_eq!(
            map_hdr_side_data_kind(Type::ContentLightLevel),
            Some(VideoHdrSideDataKind::ContentLightLevel)
        );
        assert_eq!(
            map_hdr_side_data_kind(Type::DYNAMIC_HDR10_PLUS),
            Some(VideoHdrSideDataKind::DynamicHdr10Plus)
        );
        assert_eq!(map_hdr_side_data_kind(Type::Palette), None);
    }

    #[test]
    fn capture_color_metadata_preserves_raw_cicp_tags() {
        let metadata = capture_color_metadata(
            Primaries::BT2020,
            TransferCharacteristic::SMPTE2084,
            Space::BT2020NCL,
        );

        assert!(metadata.primaries.specified);
        assert!(metadata.transfer.specified);
        assert!(metadata.matrix.specified);
        assert_eq!(metadata.primaries.name.as_deref(), Some("bt2020"));
        assert_eq!(metadata.transfer.name.as_deref(), Some("smpte2084"));
        assert_eq!(metadata.matrix.name.as_deref(), Some("bt2020nc"));
    }

    #[test]
    fn capture_color_metadata_marks_unspecified_tags() {
        let metadata = capture_color_metadata(
            Primaries::Unspecified,
            TransferCharacteristic::Unspecified,
            Space::Unspecified,
        );

        assert!(!metadata.primaries.specified);
        assert!(!metadata.transfer.specified);
        assert!(!metadata.matrix.specified);
        assert_eq!(metadata.primaries.name, None);
        assert_eq!(metadata.transfer.name, None);
        assert_eq!(metadata.matrix.name, None);
    }

    #[test]
    fn video_color_diagnostic_summary_includes_source_detection_and_raw_tags() {
        let metadata = capture_color_metadata(
            Primaries::BT2020,
            TransferCharacteristic::SMPTE2084,
            Space::BT2020NCL,
        );
        let diagnostic = VideoColorDiagnostic {
            detected_color_space: Some(ColorSpace::Rec2100Pq),
            source: VideoColorSpaceSource::Metadata,
            method: VideoColorDetectionMethod::CicpTags,
            metadata: Some(metadata),
            metadata_hints: vec![VideoColorMetadataHint {
                scope: VideoColorMetadataHintScope::Stream,
                key: "camera_profile".to_string(),
                value: "S-Log3 / S-Gamut3.Cine".to_string(),
                detected_color_space: ColorSpace::SLog3,
            }],
            hdr_metadata: vec![VideoHdrMetadataSummary {
                kind: VideoHdrSideDataKind::MasteringDisplayMetadata,
                payload_size: 88,
            }],
        };

        let summary = diagnostic.summary();

        assert!(summary.contains("source=Metadata"));
        assert!(summary.contains("method=CicpTags"));
        assert!(summary.contains("detected=Rec2100Pq"));
        assert!(summary.contains("primaries=bt2020"));
        assert!(summary.contains("transfer=smpte2084"));
        assert!(summary.contains("matrix=bt2020nc"));
        assert!(summary.contains("camera_profile=S-Log3 / S-Gamut3.Cine"));
        assert!(summary.contains("MasteringDisplayMetadata(bytes=88)"));
    }
}
