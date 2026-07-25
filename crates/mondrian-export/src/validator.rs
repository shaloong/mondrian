use crate::preset::{
    AudioCodecConfig, Av1Profile, Container, H264Profile, HevcProfile, ProResProfile,
    VideoCodecConfig,
};
use mondrian_core::{
    AudioChannelLayout, VideoContentLightMetadata, VideoHdrChromaticity, VideoHdrRational,
    VideoMasteringDisplayLuminance, VideoMasteringDisplayMetadata, VideoMasteringDisplayPrimaries,
};
use mondrian_timeline::sequence::DeliveryBitDepth;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

/// Exact stream and duration contract that a finished export must prove.
#[derive(Debug, Clone)]
pub struct ExportValidationExpectations {
    /// Exact mux/container family expected from the admitted preset.
    pub container: Container,
    /// Exact video-stream presence and representation contract.
    pub video: ExpectedStream<ExpectedVideoConstraints>,
    /// Exact audio-stream presence and representation contract.
    pub audio: ExpectedStream<ExpectedAudioConstraints>,
    /// Expected container duration in seconds.
    pub expected_duration_secs: Option<f64>,
}

/// Closed presence contract for one finished-output stream kind.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ExpectedStream<T> {
    /// The finished output must not contain this stream kind.
    #[default]
    Forbidden,
    /// The finished output must contain exactly one stream matching the contract.
    Required(T),
}

/// Lightweight stream summary for media admission paths that do not need exact
/// delivery evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MediaStreamSummary {
    /// Whether at least one video stream exists.
    pub has_video: bool,
    /// Whether at least one audio stream exists.
    pub has_audio: bool,
    /// Best available container or stream duration.
    pub duration_secs: Option<f64>,
}

/// Exact video stream constraints derived from an admitted export preset.
#[derive(Debug, Clone, Default)]
pub struct ExpectedVideoConstraints {
    /// Exact encoded codec/profile identity.
    pub encoding: Option<ExpectedVideoEncoding>,
    /// Exact encoded sample depth.
    pub bit_depth: Option<u8>,
    /// Exact encoded width.
    pub width: Option<u32>,
    /// Exact encoded height.
    pub height: Option<u32>,
    /// Exact constant-frame-rate numerator.
    pub fps_num: Option<i64>,
    /// Exact constant-frame-rate denominator.
    pub fps_den: Option<i64>,
    /// Encoded signal fields that must match the finished video stream.
    pub signal: Option<ExpectedVideoSignalConstraints>,
}

/// Codec/profile identities supported by the production export Adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedVideoEncoding {
    /// H.264 High Profile.
    H264High,
    /// HEVC Main Profile.
    HevcMain,
    /// HEVC Main 10 Profile.
    HevcMain10,
    /// AV1 Main Profile.
    Av1Main,
    /// ProRes 422 Proxy.
    ProResProxy,
    /// ProRes 422 LT.
    ProResLt,
    /// ProRes 422.
    ProResStandard,
    /// ProRes 422 HQ.
    ProResHq,
    /// ProRes 4444.
    ProRes4444,
    /// ProRes 4444 XQ.
    ProRes4444Xq,
    /// Palette GIF.
    Gif,
}

impl ExpectedVideoEncoding {
    const fn codec_name(self) -> &'static str {
        match self {
            Self::H264High => "h264",
            Self::HevcMain | Self::HevcMain10 => "hevc",
            Self::Av1Main => "av1",
            Self::ProResProxy
            | Self::ProResLt
            | Self::ProResStandard
            | Self::ProResHq
            | Self::ProRes4444
            | Self::ProRes4444Xq => "prores",
            Self::Gif => "gif",
        }
    }

    const fn accepted_profiles(self) -> &'static [&'static str] {
        match self {
            Self::H264High => &["High"],
            Self::HevcMain => &["Main"],
            Self::HevcMain10 => &["Main 10", "Main10"],
            Self::Av1Main => &["Main"],
            Self::ProResProxy => &["Proxy"],
            Self::ProResLt => &["LT"],
            Self::ProResStandard => &["Standard"],
            Self::ProResHq => &["HQ"],
            Self::ProRes4444 => &["4444"],
            Self::ProRes4444Xq => &["XQ", "4444 XQ"],
            Self::Gif => &[],
        }
    }
}

/// Exact audio stream constraints derived from an admitted export preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedAudioConstraints {
    /// Exact encoded codec identity.
    pub encoding: ExpectedAudioEncoding,
    /// Exact output sample rate.
    pub sample_rate: u32,
    /// Exact semantic output layout.
    pub channel_layout: AudioChannelLayout,
}

/// Audio codec identities supported by the production export Adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedAudioEncoding {
    /// Advanced Audio Coding.
    Aac,
    /// MPEG Layer III audio.
    Mp3,
    /// 16-bit little-endian signed PCM.
    PcmS16Le,
    /// 24-bit little-endian signed PCM.
    PcmS24Le,
    /// 32-bit little-endian signed PCM.
    PcmS32Le,
}

impl ExpectedAudioEncoding {
    const fn codec_name(self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::Mp3 => "mp3",
            Self::PcmS16Le => "pcm_s16le",
            Self::PcmS24Le => "pcm_s24le",
            Self::PcmS32Le => "pcm_s32le",
        }
    }
}

/// Expected ffprobe-visible signal identity for a finished video stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExpectedVideoSignalConstraints {
    /// Exact encoded pixel format, such as `yuv420p10le`.
    pub pixel_format: Option<String>,
    /// Exact encoded range tag, such as `tv` or `pc`.
    pub color_range: Option<String>,
    /// Exact color-primaries tag.
    pub color_primaries: Option<String>,
    /// Exact transfer-characteristic tag.
    pub color_transfer: Option<String>,
    /// Exact matrix-coefficients tag.
    pub color_matrix: Option<String>,
    /// Require primaries, transfer, and matrix tags to be absent.
    pub require_color_tags_absent: bool,
    /// Finished-output static HDR metadata policy.
    pub static_hdr_metadata: ExpectedStaticHdrMetadata,
}

/// Static HDR evidence required from the finished output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ExpectedStaticHdrMetadata {
    /// Do not inspect static HDR metadata for this non-delivery use.
    #[default]
    Unspecified,
    /// Prove that neither ST 2086 nor CTA-861.3 static metadata is present.
    Absent,
    /// Prove that the exact authored metadata survived encoding and muxing.
    Exact(ExpectedStaticHdrMetadataConstraints),
}

/// Expected SMPTE ST 2086 and CTA-861.3 metadata on the finished HDR stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedStaticHdrMetadataConstraints {
    /// Mastering-display primaries, white point, and luminance bounds.
    pub mastering_display: VideoMasteringDisplayMetadata,
    /// MaxCLL and MaxFALL content-light levels.
    pub content_light: VideoContentLightMetadata,
}

/// Typed ffprobe evidence returned after finished-output validation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExportOutputProbe {
    /// ffprobe mux/container identity.
    pub container_format: Option<String>,
    /// Container major brand when the mux family exposes one.
    pub container_major_brand: Option<String>,
    /// Best available container or stream duration.
    pub duration_secs: Option<f64>,
    /// First encoded video stream, when present.
    pub video: Option<ProbedVideoStream>,
    /// First encoded audio stream, when present.
    pub audio: Option<ProbedAudioStream>,
}

/// Typed evidence for the first encoded video stream.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProbedVideoStream {
    /// ffprobe codec identity.
    pub codec_name: Option<String>,
    /// ffprobe profile identity.
    pub profile: Option<String>,
    /// Encoded width.
    pub width: Option<u32>,
    /// Encoded height.
    pub height: Option<u32>,
    /// Reduced frame-rate numerator.
    pub frame_rate_num: Option<i64>,
    /// Reduced frame-rate denominator.
    pub frame_rate_den: Option<i64>,
    /// Exact stream-local start/duration timing when ffprobe proves it.
    pub timing: ProbedStreamTiming,
    /// Exact pixel format.
    pub pixel_format: Option<String>,
    /// Sample depth derived from the exact pixel format.
    pub bit_depth: Option<u8>,
    /// Encoded range tag.
    pub color_range: Option<String>,
    /// Encoded primaries tag.
    pub color_primaries: Option<String>,
    /// Encoded transfer tag.
    pub color_transfer: Option<String>,
    /// Encoded matrix tag.
    pub color_matrix: Option<String>,
    /// Whether ST 2086 mastering-display metadata is present on the first frame.
    pub mastering_display_metadata_present: bool,
    /// Whether CTA-861.3 content-light metadata is present on the first frame.
    pub content_light_metadata_present: bool,
}

/// Typed evidence for the first encoded audio stream.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProbedAudioStream {
    /// ffprobe codec identity.
    pub codec_name: Option<String>,
    /// Output sample rate.
    pub sample_rate: Option<u32>,
    /// Output channel count.
    pub channels: Option<u32>,
    /// ffprobe channel-layout identity.
    pub channel_layout: Option<String>,
    /// Exact stream-local start/duration timing when ffprobe proves it.
    pub timing: ProbedStreamTiming,
}

/// Exact ffprobe stream timing expressed in one declared integer time base.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ProbedStreamTiming {
    /// First stream timestamp in `time_base` units.
    pub start_pts: Option<i64>,
    /// Stream duration in `time_base` units.
    pub duration_ts: Option<i64>,
    /// Positive time-base numerator.
    pub time_base_num: Option<i64>,
    /// Positive time-base denominator.
    pub time_base_den: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FfprobeFrameSideData {
    side_data_type: Option<String>,
    red_x: Option<String>,
    red_y: Option<String>,
    green_x: Option<String>,
    green_y: Option<String>,
    blue_x: Option<String>,
    blue_y: Option<String>,
    white_point_x: Option<String>,
    white_point_y: Option<String>,
    min_luminance: Option<String>,
    max_luminance: Option<String>,
    max_content: Option<u32>,
    max_average: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
struct FfprobeReport {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    format: Option<FfprobeFormat>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FfprobeFrameReport {
    #[serde(default)]
    frames: Vec<FfprobeFrame>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FfprobeFrame {
    #[serde(default)]
    side_data_list: Vec<FfprobeFrameSideData>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    profile: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    duration: Option<String>,
    start_pts: Option<i64>,
    duration_ts: Option<i64>,
    time_base: Option<String>,
    pix_fmt: Option<String>,
    color_range: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
    channel_layout: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FfprobeFormat {
    format_name: Option<String>,
    duration: Option<String>,
    #[serde(default)]
    tags: FfprobeFormatTags,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FfprobeFormatTags {
    major_brand: Option<String>,
}

pub(crate) const fn expected_video_encoding(codec: &VideoCodecConfig) -> ExpectedVideoEncoding {
    match codec {
        VideoCodecConfig::H264 { profile: H264Profile::High, .. } => {
            ExpectedVideoEncoding::H264High
        }
        VideoCodecConfig::Hevc { profile: HevcProfile::Main, .. } => {
            ExpectedVideoEncoding::HevcMain
        }
        VideoCodecConfig::Hevc { profile: HevcProfile::Main10, .. } => {
            ExpectedVideoEncoding::HevcMain10
        }
        VideoCodecConfig::Av1 { profile: Av1Profile::Main, .. } => ExpectedVideoEncoding::Av1Main,
        VideoCodecConfig::ProRes { profile: ProResProfile::Proxy } => {
            ExpectedVideoEncoding::ProResProxy
        }
        VideoCodecConfig::ProRes { profile: ProResProfile::Lt } => ExpectedVideoEncoding::ProResLt,
        VideoCodecConfig::ProRes { profile: ProResProfile::Standard } => {
            ExpectedVideoEncoding::ProResStandard
        }
        VideoCodecConfig::ProRes { profile: ProResProfile::Hq } => ExpectedVideoEncoding::ProResHq,
        VideoCodecConfig::ProRes { profile: ProResProfile::FourFourFourFour } => {
            ExpectedVideoEncoding::ProRes4444
        }
        VideoCodecConfig::ProRes { profile: ProResProfile::FourFourFourFourXq } => {
            ExpectedVideoEncoding::ProRes4444Xq
        }
        VideoCodecConfig::Gif { .. } => ExpectedVideoEncoding::Gif,
    }
}

pub(crate) const fn delivery_bit_depth_value(bit_depth: DeliveryBitDepth) -> u8 {
    match bit_depth {
        DeliveryBitDepth::Eight => 8,
        DeliveryBitDepth::Ten => 10,
        DeliveryBitDepth::Twelve => 12,
    }
}

pub(crate) const fn expected_audio_constraints(
    codec: &AudioCodecConfig,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
) -> Option<ExpectedAudioConstraints> {
    let encoding = match codec {
        AudioCodecConfig::Disabled => return None,
        AudioCodecConfig::Aac { .. } => ExpectedAudioEncoding::Aac,
        AudioCodecConfig::Mp3 { .. } => ExpectedAudioEncoding::Mp3,
        AudioCodecConfig::Pcm { bit_depth: 16 } => ExpectedAudioEncoding::PcmS16Le,
        AudioCodecConfig::Pcm { bit_depth: 24 } => ExpectedAudioEncoding::PcmS24Le,
        AudioCodecConfig::Pcm { bit_depth: 32 } => ExpectedAudioEncoding::PcmS32Le,
        AudioCodecConfig::Pcm { .. } => return None,
    };
    Some(ExpectedAudioConstraints { encoding, sample_rate, channel_layout })
}

/// Validate a non-empty finished export and return the same typed evidence
/// used to decide whether the queue may publish `Completed`.
pub fn validate_export_output(
    output_path: &Path,
    expectations: &ExportValidationExpectations,
) -> Result<ExportOutputProbe, String> {
    let metadata = std::fs::metadata(output_path)
        .map_err(|err| format!("读取导出文件失败 {}: {}", output_path.display(), err))?;
    if metadata.len() == 0 {
        return Err(format!("导出文件大小为 0: {}", output_path.display()));
    }

    let report = ffprobe_report(output_path)?;
    validate_report(&report, expectations)?;

    let expected_static_hdr = match &expectations.video {
        ExpectedStream::Required(video) => video.signal.as_ref(),
        ExpectedStream::Forbidden => None,
    }
    .map(|signal| &signal.static_hdr_metadata);
    let side_data = if expected_static_hdr
        .is_none_or(|expected| matches!(expected, ExpectedStaticHdrMetadata::Unspecified))
    {
        None
    } else {
        Some(ffprobe_first_video_frame_side_data(output_path)?)
    };
    match (expected_static_hdr, side_data.as_deref()) {
        (None | Some(ExpectedStaticHdrMetadata::Unspecified), _) => {}
        (Some(ExpectedStaticHdrMetadata::Absent), Some(side_data)) => {
            validate_static_hdr_metadata_absent(side_data)?;
        }
        (Some(ExpectedStaticHdrMetadata::Exact(expected)), Some(side_data)) => {
            validate_static_hdr_metadata(side_data, expected)?;
        }
        (Some(ExpectedStaticHdrMetadata::Absent | ExpectedStaticHdrMetadata::Exact(_)), None) => {
            return Err("未取得导出成品首帧，无法证明静态 HDR metadata 合同".to_string());
        }
    }
    Ok(build_output_probe(&report, side_data.as_deref()))
}

/// Probe an export into stable typed evidence without applying a delivery
/// expectation. Video outputs must expose a decodable first frame so HDR
/// metadata presence cannot silently remain unknown.
pub fn probe_export_output(path: &Path) -> Result<ExportOutputProbe, String> {
    let report = ffprobe_report(path)?;
    let has_video = report
        .streams
        .iter()
        .any(|stream| stream.codec_type.as_deref() == Some("video"));
    let side_data = has_video.then(|| ffprobe_first_video_frame_side_data(path)).transpose()?;
    Ok(build_output_probe(&report, side_data.as_deref()))
}

/// Probe only stream presence and duration for lightweight media admission.
pub fn probe_media_summary(path: &Path) -> Result<MediaStreamSummary, String> {
    let report = ffprobe_report(path)?;
    Ok(summarize_report(&report))
}

fn ffprobe_report(path: &Path) -> Result<FfprobeReport, String> {
    let output = Command::new("ffprobe")
        .arg("-v")
        .arg("error")
        .arg("-show_streams")
        .arg("-show_format")
        .arg("-print_format")
        .arg("json")
        .arg(path)
        .output()
        .map_err(|err| format!("启动 ffprobe 失败: {}", err))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "ffprobe 失败（{}）: {}",
            output.status,
            stderr.trim()
        ));
    }

    serde_json::from_slice::<FfprobeReport>(&output.stdout)
        .map_err(|err| format!("解析 ffprobe 结果失败: {}", err))
}

fn ffprobe_first_video_frame_side_data(path: &Path) -> Result<Vec<FfprobeFrameSideData>, String> {
    let output = Command::new("ffprobe")
        .arg("-v")
        .arg("error")
        .arg("-select_streams")
        .arg("v:0")
        .arg("-read_intervals")
        .arg("%+#1")
        .arg("-show_frames")
        .arg("-show_entries")
        .arg("frame=side_data_list")
        .arg("-print_format")
        .arg("json")
        .arg(path)
        .output()
        .map_err(|err| format!("启动 ffprobe HDR metadata 校验失败: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "ffprobe HDR metadata 校验失败（{}）: {}",
            output.status,
            stderr.trim()
        ));
    }

    let report = serde_json::from_slice::<FfprobeFrameReport>(&output.stdout)
        .map_err(|err| format!("解析 ffprobe 首帧 HDR metadata 失败: {err}"))?;
    report
        .frames
        .into_iter()
        .next()
        .map(|frame| frame.side_data_list)
        .ok_or_else(|| "ffprobe 未能解码导出视频的首帧，无法校验静态 HDR metadata".to_string())
}

fn validate_report(
    report: &FfprobeReport,
    expectations: &ExportValidationExpectations,
) -> Result<(), String> {
    validate_container(report.format.as_ref(), &expectations.container)?;
    let video_streams = report
        .streams
        .iter()
        .filter(|stream| stream.codec_type.as_deref() == Some("video"))
        .collect::<Vec<_>>();
    let audio_streams = report
        .streams
        .iter()
        .filter(|stream| stream.codec_type.as_deref() == Some("audio"))
        .collect::<Vec<_>>();
    let video_stream = video_streams.first().copied();
    let audio_stream = audio_streams.first().copied();

    let expected_video = validate_stream_presence("视频", &video_streams, &expectations.video)?;
    let expected_audio = validate_stream_presence("音频", &audio_streams, &expectations.audio)?;

    if let (Some(stream), Some(expected)) = (video_stream, expected_video) {
        if let Some(encoding) = expected.encoding {
            validate_video_encoding(stream, encoding)?;
        }
        if let Some(expected_bit_depth) = expected.bit_depth {
            let actual_bit_depth = stream.pix_fmt.as_deref().and_then(pixel_format_bit_depth);
            if actual_bit_depth != Some(expected_bit_depth) {
                return Err(format!(
                    "导出视频位深不匹配：期望 {expected_bit_depth}-bit，实际 {}",
                    actual_bit_depth
                        .map(|value| format!("{value}-bit"))
                        .unwrap_or_else(|| "<unknown>".to_string())
                ));
            }
        }
        if let Some(expected_width) = expected.width {
            let actual_width = stream.width.unwrap_or(0);
            if actual_width != expected_width {
                return Err(format!(
                    "导出分辨率宽度不匹配：期望 {}，实际 {}",
                    expected_width, actual_width
                ));
            }
        }
        if let Some(expected_height) = expected.height {
            let actual_height = stream.height.unwrap_or(0);
            if actual_height != expected_height {
                return Err(format!(
                    "导出分辨率高度不匹配：期望 {}，实际 {}",
                    expected_height, actual_height
                ));
            }
        }

        if let (Some(fps_num), Some(fps_den)) = (expected.fps_num, expected.fps_den) {
            if fps_num <= 0 || fps_den <= 0 {
                return Err("导出校验合同包含无效帧率".to_string());
            }
            let actual_fps = stream
                .avg_frame_rate
                .as_deref()
                .and_then(parse_ratio_i64)
                .or_else(|| stream.r_frame_rate.as_deref().and_then(parse_ratio_i64));
            let exact_match = actual_fps.is_some_and(|(actual_num, actual_den)| {
                i128::from(actual_num) * i128::from(fps_den)
                    == i128::from(fps_num) * i128::from(actual_den)
            });
            if !exact_match {
                let actual = actual_fps
                    .map(|(num, den)| format!("{num}/{den}"))
                    .unwrap_or_else(|| "<missing>".to_string());
                return Err(format!(
                    "导出帧率不匹配：期望 {fps_num}/{fps_den}，实际 {actual}"
                ));
            }
        }
        if let Some(signal) = expected.signal.as_ref() {
            validate_video_signal(stream, signal)?;
        }
    }
    if let (Some(stream), Some(expected)) = (audio_stream, expected_audio) {
        validate_audio_stream(stream, expected)?;
    }

    if let Some(expected_duration_secs) = expectations.expected_duration_secs {
        let actual_duration_secs = report
            .format
            .as_ref()
            .and_then(|format| format.duration.as_deref())
            .and_then(parse_secs_f64)
            .or_else(|| {
                video_stream
                    .and_then(|stream| stream.duration.as_deref())
                    .and_then(parse_secs_f64)
            })
            .or_else(|| {
                audio_stream
                    .and_then(|stream| stream.duration.as_deref())
                    .and_then(parse_secs_f64)
            })
            .unwrap_or(0.0);
        if actual_duration_secs <= 0.0 {
            return Err("导出时长无效（<= 0）".to_string());
        }

        let tolerance = expected_duration_secs.max(1.0) * 0.03;
        if (actual_duration_secs - expected_duration_secs).abs() > tolerance {
            return Err(format!(
                "导出时长不匹配：期望 {:.3}s，实际 {:.3}s",
                expected_duration_secs, actual_duration_secs
            ));
        }
    }

    Ok(())
}

fn validate_container(actual: Option<&FfprobeFormat>, expected: &Container) -> Result<(), String> {
    let matches = actual.is_some_and(|actual| {
        let identities = actual
            .format_name
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .collect::<Vec<_>>();
        let major_brand = actual
            .tags
            .major_brand
            .as_deref()
            .map(str::trim)
            .filter(|brand| !brand.is_empty());
        match expected {
            Container::Mp4 => {
                identities.contains(&"mov")
                    && identities.contains(&"mp4")
                    && major_brand.is_some_and(|brand| !brand.eq_ignore_ascii_case("qt"))
            }
            Container::Mov => {
                identities.contains(&"mov")
                    && major_brand.is_some_and(|brand| brand.eq_ignore_ascii_case("qt"))
            }
            Container::Mkv | Container::Webm => {
                identities.contains(&"matroska") || identities.contains(&"webm")
            }
            Container::Gif => identities.contains(&"gif"),
            Container::Mxf => identities.contains(&"mxf"),
        }
    });
    if matches {
        return Ok(());
    }
    Err(format!(
        "导出容器不匹配：期望 {expected:?}，实际 format={} major_brand={}",
        actual.and_then(|format| format.format_name.as_deref()).unwrap_or("<missing>"),
        actual
            .and_then(|format| format.tags.major_brand.as_deref())
            .unwrap_or("<missing>")
    ))
}

fn validate_stream_presence<'a, T, S>(
    name: &str,
    streams: &[S],
    expectation: &'a ExpectedStream<T>,
) -> Result<Option<&'a T>, String> {
    match expectation {
        ExpectedStream::Forbidden if streams.is_empty() => Ok(None),
        ExpectedStream::Forbidden => Err(format!(
            "导出结果不应包含{name}流，实际 {} 个",
            streams.len()
        )),
        ExpectedStream::Required(_) if streams.is_empty() => Err(format!("导出结果缺少{name}流")),
        ExpectedStream::Required(expected) if streams.len() == 1 => Ok(Some(expected)),
        ExpectedStream::Required(_) => Err(format!(
            "导出结果必须恰好包含一个{name}流，实际 {} 个",
            streams.len()
        )),
    }
}

fn validate_video_encoding(
    stream: &FfprobeStream,
    expected: ExpectedVideoEncoding,
) -> Result<(), String> {
    validate_exact_codec_field(
        "视频编码",
        expected.codec_name(),
        stream.codec_name.as_deref(),
    )?;
    let accepted_profiles = expected.accepted_profiles();
    if accepted_profiles.is_empty() {
        return Ok(());
    }
    let Some(actual_profile) = stream.profile.as_deref() else {
        return Err(format!(
            "导出视频 profile 不匹配：期望 {}，实际 <missing>",
            accepted_profiles.join(" / ")
        ));
    };
    let actual_profile = normalized_identity(actual_profile);
    if accepted_profiles
        .iter()
        .any(|candidate| normalized_identity(candidate) == actual_profile)
    {
        return Ok(());
    }
    Err(format!(
        "导出视频 profile 不匹配：期望 {}，实际 {}",
        accepted_profiles.join(" / "),
        stream.profile.as_deref().unwrap_or("<missing>")
    ))
}

fn validate_audio_stream(
    stream: &FfprobeStream,
    expected: &ExpectedAudioConstraints,
) -> Result<(), String> {
    validate_exact_codec_field(
        "音频编码",
        expected.encoding.codec_name(),
        stream.codec_name.as_deref(),
    )?;
    let actual_sample_rate = stream.sample_rate.as_deref().and_then(|raw| raw.parse::<u32>().ok());
    if actual_sample_rate != Some(expected.sample_rate) {
        return Err(format!(
            "导出音频采样率不匹配：期望 {} Hz，实际 {}",
            expected.sample_rate,
            actual_sample_rate
                .map(|value| format!("{value} Hz"))
                .unwrap_or_else(|| "<missing>".to_string())
        ));
    }
    let expected_channels = expected.channel_layout.channel_count() as u32;
    if stream.channels != Some(expected_channels) {
        return Err(format!(
            "导出音频声道数不匹配：期望 {expected_channels}，实际 {}",
            stream
                .channels
                .map(|value| value.to_string())
                .unwrap_or_else(|| "<missing>".to_string())
        ));
    }
    let expected_layout = expected_ffprobe_channel_layout(expected.channel_layout)
        .ok_or_else(|| format!("导出校验尚未定义音频布局 {}", expected.channel_layout))?;
    validate_exact_codec_field(
        "音频声道布局",
        expected_layout,
        stream.channel_layout.as_deref(),
    )
}

fn validate_exact_codec_field(
    name: &str,
    expected: &str,
    actual: Option<&str>,
) -> Result<(), String> {
    if actual.is_some_and(|actual| actual.eq_ignore_ascii_case(expected)) {
        return Ok(());
    }
    Err(format!(
        "导出{name}不匹配：期望 {expected}，实际 {}",
        actual.unwrap_or("<missing>")
    ))
}

fn normalized_identity(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

const fn expected_ffprobe_channel_layout(layout: AudioChannelLayout) -> Option<&'static str> {
    match layout {
        AudioChannelLayout::Mono => Some("mono"),
        AudioChannelLayout::Stereo => Some("stereo"),
        AudioChannelLayout::Surround51Side => Some("5.1(side)"),
        AudioChannelLayout::Speakers(_) | AudioChannelLayout::Discrete(_) => None,
    }
}

fn pixel_format_bit_depth(pixel_format: &str) -> Option<u8> {
    let normalized = pixel_format.trim().to_ascii_lowercase();
    if matches!(
        normalized.as_str(),
        "yuv420p"
            | "yuv422p"
            | "yuv444p"
            | "yuva420p"
            | "yuva422p"
            | "yuva444p"
            | "gbrp"
            | "gbrap"
            | "gray"
            | "rgb24"
            | "rgba"
            | "bgra"
            | "pal8"
    ) {
        return Some(8);
    }
    for (marker, bit_depth) in [
        ("p16le", 16),
        ("p16be", 16),
        ("p14le", 14),
        ("p14be", 14),
        ("p12le", 12),
        ("p12be", 12),
        ("p10le", 10),
        ("p10be", 10),
        ("p9le", 9),
        ("p9be", 9),
    ] {
        if normalized.contains(marker) {
            return Some(bit_depth);
        }
    }
    for (prefix, bit_depth) in [
        ("p016", 16),
        ("p014", 14),
        ("p012", 12),
        ("p010", 10),
        ("p009", 9),
    ] {
        if normalized.starts_with(prefix) {
            return Some(bit_depth);
        }
    }
    None
}

fn validate_video_signal(
    stream: &FfprobeStream,
    expected: &ExpectedVideoSignalConstraints,
) -> Result<(), String> {
    validate_exact_video_field(
        "像素格式",
        expected.pixel_format.as_deref(),
        stream.pix_fmt.as_deref(),
    )?;
    validate_exact_video_field(
        "视频范围",
        expected.color_range.as_deref(),
        stream.color_range.as_deref(),
    )?;
    if expected.require_color_tags_absent {
        for (name, actual) in [
            ("色彩原色", stream.color_primaries.as_deref()),
            ("传递函数", stream.color_transfer.as_deref()),
            ("矩阵系数", stream.color_space.as_deref()),
        ] {
            if let Some(actual) = actual {
                return Err(format!("导出{name}标签应缺失，实际 {actual}"));
            }
        }
        return Ok(());
    }
    validate_exact_video_field(
        "色彩原色",
        expected.color_primaries.as_deref(),
        stream.color_primaries.as_deref(),
    )?;
    validate_exact_video_field(
        "传递函数",
        expected.color_transfer.as_deref(),
        stream.color_transfer.as_deref(),
    )?;
    validate_exact_video_field(
        "矩阵系数",
        expected.color_matrix.as_deref(),
        stream.color_space.as_deref(),
    )
}

fn validate_static_hdr_metadata(
    side_data: &[FfprobeFrameSideData],
    expected: &ExpectedStaticHdrMetadataConstraints,
) -> Result<(), String> {
    expected
        .mastering_display
        .validate()
        .map_err(|error| format!("期望的 SMPTE ST 2086 metadata 无效: {error}"))?;
    expected
        .content_light
        .validate()
        .map_err(|error| format!("期望的 CTA-861.3 metadata 无效: {error}"))?;

    let mastering_side_data = side_data
        .iter()
        .find(|data| {
            data.side_data_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("Mastering display metadata"))
        })
        .ok_or_else(|| "导出成品首帧缺少 Mastering display metadata (SMPTE ST 2086)".to_string())?;
    let actual_mastering = parse_mastering_display_metadata(mastering_side_data)?;
    actual_mastering
        .validate()
        .map_err(|error| format!("导出成品 SMPTE ST 2086 metadata 无效: {error}"))?;
    validate_mastering_display_matches(&actual_mastering, &expected.mastering_display)?;

    let content_light_side_data = side_data
        .iter()
        .find(|data| {
            data.side_data_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("Content light level metadata"))
        })
        .ok_or_else(|| {
            "导出成品首帧缺少 Content light level metadata (MaxCLL/MaxFALL)".to_string()
        })?;
    let actual_content_light = VideoContentLightMetadata {
        max_content_light_level: content_light_side_data
            .max_content
            .ok_or_else(|| "导出成品 Content light level metadata 缺少 max_content".to_string())?,
        max_frame_average_light_level: content_light_side_data
            .max_average
            .ok_or_else(|| "导出成品 Content light level metadata 缺少 max_average".to_string())?,
    };
    actual_content_light
        .validate()
        .map_err(|error| format!("导出成品 CTA-861.3 metadata 无效: {error}"))?;
    if actual_content_light != expected.content_light {
        return Err(format!(
            "导出静态 HDR Content light level metadata 不匹配：期望 MaxCLL/MaxFALL={}/{}, 实际 {}/{}",
            expected.content_light.max_content_light_level,
            expected.content_light.max_frame_average_light_level,
            actual_content_light.max_content_light_level,
            actual_content_light.max_frame_average_light_level
        ));
    }
    Ok(())
}

fn validate_static_hdr_metadata_absent(side_data: &[FfprobeFrameSideData]) -> Result<(), String> {
    let unexpected = side_data.iter().find_map(|data| {
        let kind = data.side_data_type.as_deref()?;
        is_static_hdr_side_data(kind).then_some(kind)
    });
    if let Some(unexpected) = unexpected {
        return Err(format!(
            "导出成品不应包含静态 HDR metadata，首帧实际包含 {unexpected}"
        ));
    }
    Ok(())
}

fn is_static_hdr_side_data(kind: &str) -> bool {
    kind.eq_ignore_ascii_case("Mastering display metadata")
        || kind.eq_ignore_ascii_case("Content light level metadata")
}

fn build_output_probe(
    report: &FfprobeReport,
    side_data: Option<&[FfprobeFrameSideData]>,
) -> ExportOutputProbe {
    let video = report
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("video"))
        .map(|stream| {
            let frame_rate = stream
                .avg_frame_rate
                .as_deref()
                .and_then(parse_ratio_i64)
                .or_else(|| stream.r_frame_rate.as_deref().and_then(parse_ratio_i64));
            ProbedVideoStream {
                codec_name: stream.codec_name.clone(),
                profile: stream.profile.clone(),
                width: stream.width,
                height: stream.height,
                frame_rate_num: frame_rate.map(|(num, _)| num),
                frame_rate_den: frame_rate.map(|(_, den)| den),
                timing: probed_stream_timing(stream),
                pixel_format: stream.pix_fmt.clone(),
                bit_depth: stream.pix_fmt.as_deref().and_then(pixel_format_bit_depth),
                color_range: stream.color_range.clone(),
                color_primaries: stream.color_primaries.clone(),
                color_transfer: stream.color_transfer.clone(),
                color_matrix: stream.color_space.clone(),
                mastering_display_metadata_present: side_data.is_some_and(|side_data| {
                    side_data.iter().any(|data| {
                        data.side_data_type.as_deref().is_some_and(|kind| {
                            kind.eq_ignore_ascii_case("Mastering display metadata")
                        })
                    })
                }),
                content_light_metadata_present: side_data.is_some_and(|side_data| {
                    side_data.iter().any(|data| {
                        data.side_data_type.as_deref().is_some_and(|kind| {
                            kind.eq_ignore_ascii_case("Content light level metadata")
                        })
                    })
                }),
            }
        });
    let audio = report
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("audio"))
        .map(|stream| ProbedAudioStream {
            codec_name: stream.codec_name.clone(),
            sample_rate: stream.sample_rate.as_deref().and_then(|raw| raw.parse::<u32>().ok()),
            channels: stream.channels,
            channel_layout: stream.channel_layout.clone(),
            timing: probed_stream_timing(stream),
        });
    ExportOutputProbe {
        container_format: report.format.as_ref().and_then(|format| format.format_name.clone()),
        container_major_brand: report
            .format
            .as_ref()
            .and_then(|format| format.tags.major_brand.clone()),
        duration_secs: summarize_report(report).duration_secs,
        video,
        audio,
    }
}

fn probed_stream_timing(stream: &FfprobeStream) -> ProbedStreamTiming {
    let time_base = stream.time_base.as_deref().and_then(parse_ratio_i64);
    ProbedStreamTiming {
        start_pts: stream.start_pts,
        duration_ts: stream.duration_ts,
        time_base_num: time_base.map(|(num, _)| num),
        time_base_den: time_base.map(|(_, den)| den),
    }
}

fn parse_mastering_display_metadata(
    data: &FfprobeFrameSideData,
) -> Result<VideoMasteringDisplayMetadata, String> {
    let rational = |field: &'static str, value: Option<&str>| {
        let raw =
            value.ok_or_else(|| format!("导出成品 Mastering display metadata 缺少 {field}"))?;
        parse_hdr_rational(raw).ok_or_else(|| {
            format!("导出成品 Mastering display metadata 的 {field} 不是有效有理数: {raw}")
        })
    };
    let chromaticity = |x_field: &'static str,
                        x: Option<&str>,
                        y_field: &'static str,
                        y: Option<&str>| {
        Ok::<_, String>(VideoHdrChromaticity { x: rational(x_field, x)?, y: rational(y_field, y)? })
    };

    Ok(VideoMasteringDisplayMetadata {
        primaries: Some(VideoMasteringDisplayPrimaries {
            red: chromaticity(
                "red_x",
                data.red_x.as_deref(),
                "red_y",
                data.red_y.as_deref(),
            )?,
            green: chromaticity(
                "green_x",
                data.green_x.as_deref(),
                "green_y",
                data.green_y.as_deref(),
            )?,
            blue: chromaticity(
                "blue_x",
                data.blue_x.as_deref(),
                "blue_y",
                data.blue_y.as_deref(),
            )?,
            white_point: chromaticity(
                "white_point_x",
                data.white_point_x.as_deref(),
                "white_point_y",
                data.white_point_y.as_deref(),
            )?,
        }),
        luminance: Some(VideoMasteringDisplayLuminance {
            min: rational("min_luminance", data.min_luminance.as_deref())?,
            max: rational("max_luminance", data.max_luminance.as_deref())?,
        }),
    })
}

fn parse_hdr_rational(raw: &str) -> Option<VideoHdrRational> {
    let trimmed = raw.trim();
    let (numerator, denominator) = trimmed.split_once('/').unwrap_or((trimmed, "1"));
    Some(VideoHdrRational::new(
        numerator.trim().parse::<i32>().ok()?,
        denominator.trim().parse::<i32>().ok()?,
    ))
}

fn validate_mastering_display_matches(
    actual: &VideoMasteringDisplayMetadata,
    expected: &VideoMasteringDisplayMetadata,
) -> Result<(), String> {
    let actual_primaries = actual
        .primaries
        .as_ref()
        .ok_or_else(|| "导出成品 SMPTE ST 2086 metadata 缺少 primaries".to_string())?;
    let expected_primaries = expected
        .primaries
        .as_ref()
        .ok_or_else(|| "期望的 SMPTE ST 2086 metadata 缺少 primaries".to_string())?;
    for (field, actual, expected) in [
        ("red_x", actual_primaries.red.x, expected_primaries.red.x),
        ("red_y", actual_primaries.red.y, expected_primaries.red.y),
        (
            "green_x",
            actual_primaries.green.x,
            expected_primaries.green.x,
        ),
        (
            "green_y",
            actual_primaries.green.y,
            expected_primaries.green.y,
        ),
        ("blue_x", actual_primaries.blue.x, expected_primaries.blue.x),
        ("blue_y", actual_primaries.blue.y, expected_primaries.blue.y),
        (
            "white_point_x",
            actual_primaries.white_point.x,
            expected_primaries.white_point.x,
        ),
        (
            "white_point_y",
            actual_primaries.white_point.y,
            expected_primaries.white_point.y,
        ),
    ] {
        validate_quantized_hdr_rational(field, actual, expected, 50_000)?;
    }

    let actual_luminance = actual
        .luminance
        .ok_or_else(|| "导出成品 SMPTE ST 2086 metadata 缺少 luminance".to_string())?;
    let expected_luminance = expected
        .luminance
        .ok_or_else(|| "期望的 SMPTE ST 2086 metadata 缺少 luminance".to_string())?;
    validate_quantized_hdr_rational(
        "min_luminance",
        actual_luminance.min,
        expected_luminance.min,
        10_000,
    )?;
    validate_quantized_hdr_rational(
        "max_luminance",
        actual_luminance.max,
        expected_luminance.max,
        10_000,
    )
}

fn validate_quantized_hdr_rational(
    field: &str,
    actual: VideoHdrRational,
    expected: VideoHdrRational,
    encoder_scale: i64,
) -> Result<(), String> {
    let expected_numerator = expected
        .scaled_i64(encoder_scale)
        .ok_or_else(|| format!("期望的静态 HDR metadata 字段 {field} 不能量化到编码器尺度"))?;
    let matches = actual.denominator > 0
        && i64::from(actual.numerator) * encoder_scale
            == expected_numerator * i64::from(actual.denominator);
    if matches {
        return Ok(());
    }
    Err(format!(
        "导出静态 HDR metadata 字段 {field} 不匹配：期望编码值 {expected_numerator}/{encoder_scale}，实际 {}/{}",
        actual.numerator, actual.denominator
    ))
}

fn validate_exact_video_field(
    name: &str,
    expected: Option<&str>,
    actual: Option<&str>,
) -> Result<(), String> {
    let Some(expected) = expected else {
        return Ok(());
    };
    if actual == Some(expected) {
        return Ok(());
    }
    Err(format!(
        "导出{name}不匹配：期望 {expected}，实际 {}",
        actual.unwrap_or("<missing>")
    ))
}

fn summarize_report(report: &FfprobeReport) -> MediaStreamSummary {
    let has_video = report
        .streams
        .iter()
        .any(|stream| stream.codec_type.as_deref() == Some("video"));
    let has_audio = report
        .streams
        .iter()
        .any(|stream| stream.codec_type.as_deref() == Some("audio"));
    let duration_secs = report
        .format
        .as_ref()
        .and_then(|format| format.duration.as_deref())
        .and_then(parse_secs_f64)
        .or_else(|| {
            report
                .streams
                .iter()
                .find_map(|stream| stream.duration.as_deref().and_then(parse_secs_f64))
        });

    MediaStreamSummary { has_video, has_audio, duration_secs }
}

fn parse_ratio_i64(raw: &str) -> Option<(i64, i64)> {
    let trimmed = raw.trim();
    let (numerator, denominator) = trimmed.split_once('/').unwrap_or((trimmed, "1"));
    let mut numerator = numerator.trim().parse::<i64>().ok()?;
    let mut denominator = denominator.trim().parse::<i64>().ok()?;
    if numerator <= 0 || denominator <= 0 {
        return None;
    }
    let divisor = greatest_common_divisor(numerator, denominator);
    numerator /= divisor;
    denominator /= divisor;
    Some((numerator, denominator))
}

const fn greatest_common_divisor(mut left: i64, mut right: i64) -> i64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

fn parse_secs_f64(raw: &str) -> Option<f64> {
    let v = raw.trim().parse::<f64>().ok()?;
    if v.is_finite() && v >= 0.0 {
        Some(v)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_report() -> FfprobeReport {
        FfprobeReport {
            streams: vec![
                FfprobeStream {
                    codec_type: Some("video".to_string()),
                    codec_name: Some("hevc".to_string()),
                    profile: Some("Main 10".to_string()),
                    width: Some(1920),
                    height: Some(1080),
                    r_frame_rate: Some("25/1".to_string()),
                    avg_frame_rate: Some("25/1".to_string()),
                    duration: Some("10.0".to_string()),
                    start_pts: Some(0),
                    duration_ts: Some(256_000),
                    time_base: Some("1/25600".to_string()),
                    pix_fmt: Some("yuv420p10le".to_string()),
                    color_range: Some("tv".to_string()),
                    color_space: Some("bt2020nc".to_string()),
                    color_transfer: Some("smpte2084".to_string()),
                    color_primaries: Some("bt2020".to_string()),
                    ..FfprobeStream::default()
                },
                FfprobeStream {
                    codec_type: Some("audio".to_string()),
                    codec_name: Some("aac".to_string()),
                    duration: Some("10.0".to_string()),
                    start_pts: Some(0),
                    duration_ts: Some(480_000),
                    time_base: Some("1/48000".to_string()),
                    sample_rate: Some("48000".to_string()),
                    channels: Some(2),
                    channel_layout: Some("stereo".to_string()),
                    ..FfprobeStream::default()
                },
            ],
            format: Some(FfprobeFormat {
                format_name: Some("mov,mp4,m4a,3gp,3g2,mj2".to_string()),
                duration: Some("10.0".to_string()),
                tags: FfprobeFormatTags { major_brand: Some("isom".to_string()) },
            }),
        }
    }

    fn base_audio_expectation() -> ExpectedStream<ExpectedAudioConstraints> {
        ExpectedStream::Required(ExpectedAudioConstraints {
            encoding: ExpectedAudioEncoding::Aac,
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
        })
    }

    fn exact_base_expectations() -> ExportValidationExpectations {
        ExportValidationExpectations {
            container: Container::Mp4,
            video: ExpectedStream::Required(ExpectedVideoConstraints {
                encoding: Some(ExpectedVideoEncoding::HevcMain10),
                bit_depth: Some(10),
                width: Some(1920),
                height: Some(1080),
                fps_num: Some(25),
                fps_den: Some(1),
                signal: None,
            }),
            audio: base_audio_expectation(),
            expected_duration_secs: Some(10.0),
        }
    }

    #[test]
    fn validate_report_passes_with_matching_constraints() {
        let report = base_report();
        let expected = ExportValidationExpectations {
            container: Container::Mp4,
            video: ExpectedStream::Required(ExpectedVideoConstraints {
                width: Some(1920),
                height: Some(1080),
                fps_num: Some(25),
                fps_den: Some(1),
                signal: None,
                ..ExpectedVideoConstraints::default()
            }),
            audio: base_audio_expectation(),
            expected_duration_secs: Some(10.0),
        };

        assert!(validate_report(&report, &expected).is_ok());
    }

    #[test]
    fn validate_report_proves_exact_codec_profile_depth_and_audio_contract() {
        let report = base_report();
        let expected = exact_base_expectations();

        validate_report(&report, &expected).expect("the exact encoded contract should match");
        let probe = build_output_probe(&report, Some(&[]));
        assert_eq!(
            probe.container_format.as_deref(),
            Some("mov,mp4,m4a,3gp,3g2,mj2")
        );
        assert_eq!(probe.container_major_brand.as_deref(), Some("isom"));
        let video = probe.video.expect("video evidence");
        assert_eq!(video.codec_name.as_deref(), Some("hevc"));
        assert_eq!(video.profile.as_deref(), Some("Main 10"));
        assert_eq!(video.bit_depth, Some(10));
        assert_eq!(video.frame_rate_num, Some(25));
        assert_eq!(video.frame_rate_den, Some(1));
        assert_eq!(
            video.timing,
            ProbedStreamTiming {
                start_pts: Some(0),
                duration_ts: Some(256_000),
                time_base_num: Some(1),
                time_base_den: Some(25_600),
            }
        );
        let audio = probe.audio.expect("audio evidence");
        assert_eq!(audio.codec_name.as_deref(), Some("aac"));
        assert_eq!(audio.sample_rate, Some(48_000));
        assert_eq!(audio.channels, Some(2));
        assert_eq!(audio.channel_layout.as_deref(), Some("stereo"));
        assert_eq!(
            audio.timing,
            ProbedStreamTiming {
                start_pts: Some(0),
                duration_ts: Some(480_000),
                time_base_num: Some(1),
                time_base_den: Some(48_000),
            }
        );
    }

    #[test]
    fn validate_report_distinguishes_mp4_and_quicktime_major_brands() {
        let mut report = base_report();
        report.format.as_mut().expect("format").tags.major_brand = Some("qt".to_string());
        let expected = exact_base_expectations();

        let error =
            validate_report(&report, &expected).expect_err("QuickTime must not satisfy MP4");
        assert!(error.contains("容器不匹配"));
        assert!(error.contains("major_brand=qt"));

        let expected = ExportValidationExpectations {
            container: Container::Mov,
            ..exact_base_expectations()
        };
        validate_report(&report, &expected).expect("QuickTime major brand should satisfy MOV");
    }

    #[test]
    fn validate_report_rejects_wrong_profile_depth_and_audio_rate() {
        let expected = exact_base_expectations();

        let mut wrong_profile = base_report();
        wrong_profile.streams[0].profile = Some("Main".to_string());
        let error = validate_report(&wrong_profile, &expected)
            .expect_err("a profile downgrade must fail closed");
        assert!(error.contains("profile"));
        assert!(error.contains("Main 10"));

        let mut wrong_depth = base_report();
        wrong_depth.streams[0].pix_fmt = Some("yuv420p".to_string());
        let error = validate_report(&wrong_depth, &expected)
            .expect_err("an encoded depth downgrade must fail closed");
        assert!(error.contains("位深"));
        assert!(error.contains("10-bit"));
        assert!(error.contains("8-bit"));

        let mut wrong_audio_rate = base_report();
        wrong_audio_rate.streams[1].sample_rate = Some("44100".to_string());
        let error = validate_report(&wrong_audio_rate, &expected)
            .expect_err("an audio sample-rate mismatch must fail closed");
        assert!(error.contains("音频采样率"));
        assert!(error.contains("48000"));
        assert!(error.contains("44100"));
    }

    #[test]
    fn stream_contract_rejects_unexpected_audio_and_static_hdr_metadata() {
        let report = base_report();
        let expected = ExportValidationExpectations {
            container: Container::Mp4,
            video: ExpectedStream::Required(ExpectedVideoConstraints::default()),
            audio: ExpectedStream::Forbidden,
            expected_duration_secs: None,
        };
        let error =
            validate_report(&report, &expected).expect_err("unexpected audio must fail closed");
        assert!(error.contains("不应包含音频流"));

        let error = validate_static_hdr_metadata_absent(&reference_static_hdr_side_data())
            .expect_err("SDR/Omit delivery must reject invented static HDR metadata");
        assert!(error.contains("不应包含静态 HDR metadata"));
        assert!(error.contains("Mastering display metadata"));
    }

    #[test]
    fn validate_report_fails_when_audio_required_but_missing() {
        let mut report = base_report();
        report.streams.retain(|stream| stream.codec_type.as_deref() != Some("audio"));

        let expected = ExportValidationExpectations {
            container: Container::Mp4,
            video: ExpectedStream::Required(ExpectedVideoConstraints::default()),
            audio: ExpectedStream::Required(ExpectedAudioConstraints {
                encoding: ExpectedAudioEncoding::Aac,
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
            }),
            expected_duration_secs: None,
        };
        let err = validate_report(&report, &expected).expect_err("should fail");
        assert!(err.contains("缺少音频流"));
    }

    #[test]
    fn validate_report_fails_on_fps_mismatch() {
        let mut report = base_report();
        if let Some(video) = report
            .streams
            .iter_mut()
            .find(|stream| stream.codec_type.as_deref() == Some("video"))
        {
            video.avg_frame_rate = Some("30/1".to_string());
        }

        let expected = ExportValidationExpectations {
            container: Container::Mp4,
            video: ExpectedStream::Required(ExpectedVideoConstraints {
                width: Some(1920),
                height: Some(1080),
                fps_num: Some(25),
                fps_den: Some(1),
                signal: None,
                ..ExpectedVideoConstraints::default()
            }),
            audio: base_audio_expectation(),
            expected_duration_secs: None,
        };
        let err = validate_report(&report, &expected).expect_err("should fail");
        assert!(err.contains("帧率不匹配"));
    }

    #[test]
    fn validate_report_checks_encoded_video_signal_fields() {
        let report = base_report();
        let expected = ExportValidationExpectations {
            container: Container::Mp4,
            video: ExpectedStream::Required(ExpectedVideoConstraints {
                signal: Some(ExpectedVideoSignalConstraints {
                    pixel_format: Some("yuv420p10le".to_owned()),
                    color_range: Some("tv".to_owned()),
                    color_primaries: Some("bt2020".to_owned()),
                    color_transfer: Some("smpte2084".to_owned()),
                    color_matrix: Some("bt2020nc".to_owned()),
                    require_color_tags_absent: false,
                    static_hdr_metadata: ExpectedStaticHdrMetadata::Unspecified,
                }),
                ..ExpectedVideoConstraints::default()
            }),
            audio: base_audio_expectation(),
            expected_duration_secs: None,
        };

        assert!(validate_report(&report, &expected).is_ok());
    }

    #[test]
    fn validate_report_fails_on_encoded_matrix_mismatch() {
        let report = base_report();
        let expected = ExportValidationExpectations {
            container: Container::Mp4,
            video: ExpectedStream::Required(ExpectedVideoConstraints {
                signal: Some(ExpectedVideoSignalConstraints {
                    color_matrix: Some("bt709".to_owned()),
                    ..ExpectedVideoSignalConstraints::default()
                }),
                ..ExpectedVideoConstraints::default()
            }),
            audio: base_audio_expectation(),
            expected_duration_secs: None,
        };

        let err = validate_report(&report, &expected).expect_err("matrix mismatch must fail");
        assert!(err.contains("矩阵系数"));
        assert!(err.contains("bt709"));
        assert!(err.contains("bt2020nc"));
    }

    #[test]
    fn validate_report_can_require_color_tags_absent() {
        let mut report = base_report();
        {
            let video = report
                .streams
                .iter_mut()
                .find(|stream| stream.codec_type.as_deref() == Some("video"))
                .expect("video stream");
            video.color_primaries = None;
            video.color_transfer = None;
            video.color_space = None;
        }
        let expected = ExportValidationExpectations {
            container: Container::Mp4,
            video: ExpectedStream::Required(ExpectedVideoConstraints {
                signal: Some(ExpectedVideoSignalConstraints {
                    require_color_tags_absent: true,
                    ..ExpectedVideoSignalConstraints::default()
                }),
                ..ExpectedVideoConstraints::default()
            }),
            audio: base_audio_expectation(),
            expected_duration_secs: None,
        };

        assert!(validate_report(&report, &expected).is_ok());
        report
            .streams
            .iter_mut()
            .find(|stream| stream.codec_type.as_deref() == Some("video"))
            .expect("video stream")
            .color_transfer = Some("bt709".to_owned());
        let err = validate_report(&report, &expected).expect_err("invented tag must fail");
        assert!(err.contains("传递函数标签应缺失"));
    }

    #[test]
    fn parse_ratio_i64_reduces_fraction_and_number() {
        assert_eq!(parse_ratio_i64("25/1"), Some((25, 1)));
        assert_eq!(parse_ratio_i64("60000/2002"), Some((30_000, 1_001)));
        assert_eq!(parse_ratio_i64("24"), Some((24, 1)));
        assert_eq!(parse_ratio_i64("0/0"), None);
    }

    #[test]
    fn validate_static_hdr_metadata_rejects_missing_side_data() {
        let expected = ExpectedStaticHdrMetadataConstraints {
            mastering_display: VideoMasteringDisplayMetadata::rec2100_1000_nit_reference(),
            content_light: VideoContentLightMetadata::rec2100_1000_nit_reference(),
        };

        let error = validate_static_hdr_metadata(&[], &expected)
            .expect_err("missing encoded static HDR metadata must fail closed");
        assert!(error.contains("Mastering display metadata"));
    }

    #[test]
    fn validate_static_hdr_metadata_accepts_ffprobe_frame_payload() {
        let expected = ExpectedStaticHdrMetadataConstraints {
            mastering_display: VideoMasteringDisplayMetadata::rec2100_1000_nit_reference(),
            content_light: VideoContentLightMetadata::rec2100_1000_nit_reference(),
        };

        validate_static_hdr_metadata(&reference_static_hdr_side_data(), &expected)
            .expect("encoded reference metadata should match its delivery contract");
    }

    #[test]
    fn validate_static_hdr_metadata_rejects_content_light_mismatch() {
        let expected = ExpectedStaticHdrMetadataConstraints {
            mastering_display: VideoMasteringDisplayMetadata::rec2100_1000_nit_reference(),
            content_light: VideoContentLightMetadata::rec2100_1000_nit_reference(),
        };
        let mut side_data = reference_static_hdr_side_data();
        side_data
            .iter_mut()
            .find(|data| data.side_data_type.as_deref() == Some("Content light level metadata"))
            .expect("content-light fixture")
            .max_content = Some(900);

        let error = validate_static_hdr_metadata(&side_data, &expected)
            .expect_err("changed encoded MaxCLL must fail");
        assert!(error.contains("期望 MaxCLL/MaxFALL=1000/400"));
        assert!(error.contains("实际 900/400"));
    }

    fn reference_static_hdr_side_data() -> Vec<FfprobeFrameSideData> {
        vec![
            FfprobeFrameSideData {
                side_data_type: Some("Mastering display metadata".to_string()),
                red_x: Some("34000/50000".to_string()),
                red_y: Some("16000/50000".to_string()),
                green_x: Some("13250/50000".to_string()),
                green_y: Some("34500/50000".to_string()),
                blue_x: Some("7500/50000".to_string()),
                blue_y: Some("3000/50000".to_string()),
                white_point_x: Some("15635/50000".to_string()),
                white_point_y: Some("16450/50000".to_string()),
                min_luminance: Some("1/10000".to_string()),
                max_luminance: Some("10000000/10000".to_string()),
                ..FfprobeFrameSideData::default()
            },
            FfprobeFrameSideData {
                side_data_type: Some("Content light level metadata".to_string()),
                max_content: Some(1000),
                max_average: Some(400),
                ..FfprobeFrameSideData::default()
            },
        ]
    }

    #[test]
    fn summarize_report_detects_streams_and_duration() {
        let report = base_report();
        let summary = summarize_report(&report);
        assert_eq!(
            summary,
            MediaStreamSummary {
                has_video: true,
                has_audio: true,
                duration_secs: Some(10.0),
            }
        );
    }

    #[test]
    fn validate_export_output_reads_real_signal_metadata() {
        let path = std::env::temp_dir().join(format!(
            "mondrian-export-signal-validation-{}.mp4",
            std::process::id()
        ));
        let status = Command::new("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=red:s=16x16:d=0.1",
                "-vf",
                "format=rgba,scale=iw:ih:in_range=full:out_range=limited:out_color_matrix=bt709",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-color_range",
                "tv",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "iec61966-2-1",
                "-colorspace",
                "bt709",
            ])
            .arg(&path)
            .status()
            .expect("launch ffmpeg signal fixture");
        assert!(status.success());
        let expectations = ExportValidationExpectations {
            container: Container::Mp4,
            video: ExpectedStream::Required(ExpectedVideoConstraints {
                width: Some(16),
                height: Some(16),
                signal: Some(ExpectedVideoSignalConstraints {
                    pixel_format: Some("yuv420p".to_owned()),
                    color_range: Some("tv".to_owned()),
                    color_primaries: Some("bt709".to_owned()),
                    color_transfer: Some("iec61966-2-1".to_owned()),
                    color_matrix: Some("bt709".to_owned()),
                    require_color_tags_absent: false,
                    static_hdr_metadata: ExpectedStaticHdrMetadata::Absent,
                }),
                ..ExpectedVideoConstraints::default()
            }),
            audio: ExpectedStream::Forbidden,
            expected_duration_secs: None,
        };

        let result = validate_export_output(&path, &expectations);
        let _ = std::fs::remove_file(path);
        result.expect("real encoded signal should satisfy its contract");
    }

    #[test]
    fn validate_export_output_reads_real_hdr10_static_metadata() {
        let path = std::env::temp_dir().join(format!(
            "mondrian-export-hdr10-validation-{}.mp4",
            std::process::id()
        ));
        let output = Command::new("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=red:s=16x16:r=1:d=1",
                "-frames:v",
                "1",
                "-c:v",
                "libx265",
                "-pix_fmt",
                "yuv420p10le",
                "-color_range",
                "tv",
                "-color_primaries",
                "bt2020",
                "-color_trc",
                "smpte2084",
                "-colorspace",
                "bt2020nc",
                "-x265-params",
                "master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400",
            ])
            .arg(&path)
            .output()
            .expect("launch ffmpeg HDR10 fixture");
        assert!(
            output.status.success(),
            "ffmpeg HDR10 fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut expectations = ExportValidationExpectations {
            container: Container::Mp4,
            video: ExpectedStream::Required(ExpectedVideoConstraints {
                width: Some(16),
                height: Some(16),
                signal: Some(ExpectedVideoSignalConstraints {
                    pixel_format: Some("yuv420p10le".to_owned()),
                    color_range: Some("tv".to_owned()),
                    color_primaries: Some("bt2020".to_owned()),
                    color_transfer: Some("smpte2084".to_owned()),
                    color_matrix: Some("bt2020nc".to_owned()),
                    require_color_tags_absent: false,
                    static_hdr_metadata: ExpectedStaticHdrMetadata::Exact(
                        ExpectedStaticHdrMetadataConstraints {
                            mastering_display:
                                VideoMasteringDisplayMetadata::rec2100_1000_nit_reference(),
                            content_light: VideoContentLightMetadata::rec2100_1000_nit_reference(),
                        },
                    ),
                }),
                ..ExpectedVideoConstraints::default()
            }),
            audio: ExpectedStream::Forbidden,
            expected_duration_secs: None,
        };

        validate_export_output(&path, &expectations)
            .expect("real encoded HDR10 metadata should satisfy its contract");
        let ExpectedStream::Required(expected_video) = &mut expectations.video else {
            panic!("required HDR10 video expectation");
        };
        let signal = expected_video.signal.as_mut().expect("HDR10 signal expectation");
        let ExpectedStaticHdrMetadata::Exact(expected_static_hdr) = &mut signal.static_hdr_metadata
        else {
            panic!("HDR10 exact metadata expectation");
        };
        expected_static_hdr.content_light.max_content_light_level = 900;
        let mismatch = validate_export_output(&path, &expectations)
            .expect_err("a mismatched post-encode MaxCLL contract must fail");
        let _ = std::fs::remove_file(path);
        assert!(mismatch.contains("实际 1000/400"));
    }
}
