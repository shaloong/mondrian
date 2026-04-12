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

#[derive(Debug, Clone, Copy, Default)]
pub struct ExpectedVideoConstraints {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps_num: Option<i64>,
    pub fps_den: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct FfprobeReport {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    format: Option<FfprobeFormat>,
}

#[derive(Debug, Clone, Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    duration: Option<String>,
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
    validate_report(&report, expectations)
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

    if let (Some(stream), Some(expected)) = (video_stream, expectations.expected_video) {
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
                },
                FfprobeStream {
                    codec_type: Some("audio".to_string()),
                    width: None,
                    height: None,
                    r_frame_rate: None,
                    avg_frame_rate: None,
                    duration: Some("10.0".to_string()),
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
            }),
            expected_duration_secs: None,
        };
        let err = validate_report(&report, &expected).expect_err("should fail");
        assert!(err.contains("帧率不匹配"));
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
}
