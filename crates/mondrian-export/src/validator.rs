use mondrian_core::{
    VideoContentLightMetadata, VideoHdrChromaticity, VideoHdrRational,
    VideoMasteringDisplayLuminance, VideoMasteringDisplayMetadata, VideoMasteringDisplayPrimaries,
};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Default)]
pub struct ExportValidationExpectations {
    pub require_video_stream: bool,
    pub require_audio_stream: bool,
    pub expected_video: Option<ExpectedVideoConstraints>,
    pub expected_duration_secs: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MediaStreamSummary {
    pub has_video: bool,
    pub has_audio: bool,
    pub duration_secs: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct ExpectedVideoConstraints {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps_num: Option<i64>,
    pub fps_den: Option<i64>,
    /// Encoded signal fields that must match the finished video stream.
    pub signal: Option<ExpectedVideoSignalConstraints>,
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
    /// Exact authored static HDR metadata that must survive encoding and muxing.
    pub static_hdr_metadata: Option<ExpectedStaticHdrMetadataConstraints>,
}

/// Expected SMPTE ST 2086 and CTA-861.3 metadata on the finished HDR stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedStaticHdrMetadataConstraints {
    /// Mastering-display primaries, white point, and luminance bounds.
    pub mastering_display: VideoMasteringDisplayMetadata,
    /// MaxCLL and MaxFALL content-light levels.
    pub content_light: VideoContentLightMetadata,
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

#[derive(Debug, Clone, Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    duration: Option<String>,
    pix_fmt: Option<String>,
    color_range: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
}

pub fn validate_export_output(
    output_path: &Path,
    expectations: &ExportValidationExpectations,
) -> Result<(), String> {
    let metadata = std::fs::metadata(output_path)
        .map_err(|err| format!("读取导出文件失败 {}: {}", output_path.display(), err))?;
    if metadata.len() == 0 {
        return Err(format!("导出文件大小为 0: {}", output_path.display()));
    }

    let report = ffprobe_report(output_path)?;
    validate_report(&report, expectations)?;

    let expected_static_hdr = expectations
        .expected_video
        .as_ref()
        .and_then(|video| video.signal.as_ref())
        .and_then(|signal| signal.static_hdr_metadata.as_ref());
    if let Some(expected_static_hdr) = expected_static_hdr {
        let side_data = ffprobe_first_video_frame_side_data(output_path)?;
        validate_static_hdr_metadata(&side_data, expected_static_hdr)?;
    }
    Ok(())
}

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
    let video_stream = report
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("video"));
    let audio_stream = report
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("audio"));

    if expectations.require_video_stream && video_stream.is_none() {
        return Err("导出结果缺少视频流".to_string());
    }
    if expectations.require_audio_stream && audio_stream.is_none() {
        return Err("导出结果缺少音频流".to_string());
    }

    if let (Some(stream), Some(expected)) = (video_stream, expectations.expected_video.as_ref()) {
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
            let expected_fps = fps_num as f64 / fps_den.max(1) as f64;
            let actual_fps = stream
                .avg_frame_rate
                .as_deref()
                .and_then(parse_ratio_f64)
                .or_else(|| stream.r_frame_rate.as_deref().and_then(parse_ratio_f64))
                .unwrap_or(0.0);
            let tolerance = expected_fps.abs().max(1.0) * 0.01;
            if (actual_fps - expected_fps).abs() > tolerance {
                return Err(format!(
                    "导出帧率不匹配：期望 {:.4}，实际 {:.4}",
                    expected_fps, actual_fps
                ));
            }
        }
        if let Some(signal) = expected.signal.as_ref() {
            validate_video_signal(stream, signal)?;
        }
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

fn parse_ratio_f64(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(v) = trimmed.parse::<f64>() {
        if v.is_finite() && v > 0.0 {
            return Some(v);
        }
        return None;
    }

    let (num_raw, den_raw) = trimmed.split_once('/')?;
    let num = num_raw.trim().parse::<f64>().ok()?;
    let den = den_raw.trim().parse::<f64>().ok()?;
    if !num.is_finite() || !den.is_finite() || den.abs() <= f64::EPSILON {
        return None;
    }
    let value = num / den;
    if value.is_finite() && value > 0.0 {
        Some(value)
    } else {
        None
    }
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
                    width: Some(1920),
                    height: Some(1080),
                    r_frame_rate: Some("25/1".to_string()),
                    avg_frame_rate: Some("25/1".to_string()),
                    duration: Some("10.0".to_string()),
                    pix_fmt: Some("yuv420p10le".to_string()),
                    color_range: Some("tv".to_string()),
                    color_space: Some("bt2020nc".to_string()),
                    color_transfer: Some("smpte2084".to_string()),
                    color_primaries: Some("bt2020".to_string()),
                },
                FfprobeStream {
                    codec_type: Some("audio".to_string()),
                    width: None,
                    height: None,
                    r_frame_rate: None,
                    avg_frame_rate: None,
                    duration: Some("10.0".to_string()),
                    pix_fmt: None,
                    color_range: None,
                    color_space: None,
                    color_transfer: None,
                    color_primaries: None,
                },
            ],
            format: Some(FfprobeFormat { duration: Some("10.0".to_string()) }),
        }
    }

    #[test]
    fn validate_report_passes_with_matching_constraints() {
        let report = base_report();
        let expected = ExportValidationExpectations {
            require_video_stream: true,
            require_audio_stream: true,
            expected_video: Some(ExpectedVideoConstraints {
                width: Some(1920),
                height: Some(1080),
                fps_num: Some(25),
                fps_den: Some(1),
                signal: None,
            }),
            expected_duration_secs: Some(10.0),
        };

        assert!(validate_report(&report, &expected).is_ok());
    }

    #[test]
    fn validate_report_fails_when_audio_required_but_missing() {
        let mut report = base_report();
        report.streams.retain(|stream| stream.codec_type.as_deref() != Some("audio"));

        let expected = ExportValidationExpectations {
            require_video_stream: true,
            require_audio_stream: true,
            expected_video: None,
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
            require_video_stream: true,
            require_audio_stream: false,
            expected_video: Some(ExpectedVideoConstraints {
                width: Some(1920),
                height: Some(1080),
                fps_num: Some(25),
                fps_den: Some(1),
                signal: None,
            }),
            expected_duration_secs: None,
        };
        let err = validate_report(&report, &expected).expect_err("should fail");
        assert!(err.contains("帧率不匹配"));
    }

    #[test]
    fn validate_report_checks_encoded_video_signal_fields() {
        let report = base_report();
        let expected = ExportValidationExpectations {
            require_video_stream: true,
            require_audio_stream: false,
            expected_video: Some(ExpectedVideoConstraints {
                signal: Some(ExpectedVideoSignalConstraints {
                    pixel_format: Some("yuv420p10le".to_owned()),
                    color_range: Some("tv".to_owned()),
                    color_primaries: Some("bt2020".to_owned()),
                    color_transfer: Some("smpte2084".to_owned()),
                    color_matrix: Some("bt2020nc".to_owned()),
                    require_color_tags_absent: false,
                    static_hdr_metadata: None,
                }),
                ..ExpectedVideoConstraints::default()
            }),
            expected_duration_secs: None,
        };

        assert!(validate_report(&report, &expected).is_ok());
    }

    #[test]
    fn validate_report_fails_on_encoded_matrix_mismatch() {
        let report = base_report();
        let expected = ExportValidationExpectations {
            require_video_stream: true,
            require_audio_stream: false,
            expected_video: Some(ExpectedVideoConstraints {
                signal: Some(ExpectedVideoSignalConstraints {
                    color_matrix: Some("bt709".to_owned()),
                    ..ExpectedVideoSignalConstraints::default()
                }),
                ..ExpectedVideoConstraints::default()
            }),
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
            require_video_stream: true,
            require_audio_stream: false,
            expected_video: Some(ExpectedVideoConstraints {
                signal: Some(ExpectedVideoSignalConstraints {
                    require_color_tags_absent: true,
                    ..ExpectedVideoSignalConstraints::default()
                }),
                ..ExpectedVideoConstraints::default()
            }),
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
    fn parse_ratio_f64_handles_fraction_and_number() {
        assert_eq!(parse_ratio_f64("25/1"), Some(25.0));
        assert_eq!(
            parse_ratio_f64("30000/1001").map(|v| (v * 1000.0).round() as i64),
            Some(29970)
        );
        assert_eq!(parse_ratio_f64("24"), Some(24.0));
        assert_eq!(parse_ratio_f64("0/0"), None);
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
            require_video_stream: true,
            require_audio_stream: false,
            expected_video: Some(ExpectedVideoConstraints {
                width: Some(16),
                height: Some(16),
                signal: Some(ExpectedVideoSignalConstraints {
                    pixel_format: Some("yuv420p".to_owned()),
                    color_range: Some("tv".to_owned()),
                    color_primaries: Some("bt709".to_owned()),
                    color_transfer: Some("iec61966-2-1".to_owned()),
                    color_matrix: Some("bt709".to_owned()),
                    require_color_tags_absent: false,
                    static_hdr_metadata: None,
                }),
                ..ExpectedVideoConstraints::default()
            }),
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
            require_video_stream: true,
            require_audio_stream: false,
            expected_video: Some(ExpectedVideoConstraints {
                width: Some(16),
                height: Some(16),
                signal: Some(ExpectedVideoSignalConstraints {
                    pixel_format: Some("yuv420p10le".to_owned()),
                    color_range: Some("tv".to_owned()),
                    color_primaries: Some("bt2020".to_owned()),
                    color_transfer: Some("smpte2084".to_owned()),
                    color_matrix: Some("bt2020nc".to_owned()),
                    require_color_tags_absent: false,
                    static_hdr_metadata: Some(ExpectedStaticHdrMetadataConstraints {
                        mastering_display:
                            VideoMasteringDisplayMetadata::rec2100_1000_nit_reference(),
                        content_light: VideoContentLightMetadata::rec2100_1000_nit_reference(),
                    }),
                }),
                ..ExpectedVideoConstraints::default()
            }),
            expected_duration_secs: None,
        };

        validate_export_output(&path, &expectations)
            .expect("real encoded HDR10 metadata should satisfy its contract");
        expectations
            .expected_video
            .as_mut()
            .and_then(|video| video.signal.as_mut())
            .and_then(|signal| signal.static_hdr_metadata.as_mut())
            .expect("HDR10 expectation")
            .content_light
            .max_content_light_level = 900;
        let mismatch = validate_export_output(&path, &expectations)
            .expect_err("a mismatched post-encode MaxCLL contract must fail");
        let _ = std::fs::remove_file(path);
        assert!(mismatch.contains("实际 1000/400"));
    }
}
