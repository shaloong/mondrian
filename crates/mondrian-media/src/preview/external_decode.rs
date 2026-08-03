//! Cancellable lifecycle for the optional external FFmpeg still decoder.
//!
//! The process is an explicit CPU-RGBA fallback for exact still requests. This
//! module owns child creation, bounded cancellation polling, kill/wait, pipe
//! draining, and reader-thread joining so no caller can publish a canceled
//! result or leak a process/thread.

use super::*;
use crate::{
    run_supervised_command_while, SupervisedProcessError, SupervisedProcessPolicy,
    SupervisedStreamCapture,
};
use std::io;
use std::process::{Command, Output, Stdio};

const EXTERNAL_DECODE_STDERR_RETAIN_BYTES: usize = 64 * 1024;

pub(super) fn preview_external_ffmpeg_cpu_rgba_enabled(
    access_mode: PreviewDecodeAccessMode,
) -> bool {
    if !preview_external_ffmpeg_cpu_rgba_allowed_for_access_mode(access_mode) {
        return false;
    }
    match preview_decode_backend() {
        PreviewDecodeBackend::ExternalFfmpegCpuRgba => true,
        PreviewDecodeBackend::Software => false,
        PreviewDecodeBackend::Auto => {
            static ENABLED: OnceLock<bool> = OnceLock::new();
            *ENABLED.get_or_init(|| {
                std::env::var("MONDRIAN_PREVIEW_EXTERNAL_FFMPEG_CPU_RGBA")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false)
            })
        }
    }
}

pub(super) fn preview_external_ffmpeg_cpu_rgba_allowed_for_access_mode(
    access_mode: PreviewDecodeAccessMode,
) -> bool {
    access_mode == PreviewDecodeAccessMode::RandomAccessStillFrame
}

pub(super) fn ensure_ffmpeg_initialized(path: &Path) -> Result<()> {
    crate::ffmpeg_runtime::ensure_ffmpeg_initialized(path)
}

pub(super) fn try_decode_with_external_ffmpeg_cpu_rgba(
    path: &Path,
    video_stream_index: Option<u32>,
    source_time: TimelineTime,
    width: u32,
    height: u32,
    source_color: PreviewSourceColorContract,
    source_format: ffmpeg::util::format::pixel::Pixel,
    decoded_color_space: ffmpeg::util::color::Space,
    decoded_color_range: ffmpeg::util::color::Range,
    should_cancel: &(dyn Fn() -> bool + Send + Sync),
) -> Option<Result<Option<RgbaFrame>>> {
    if width == 0 || height == 0 {
        return None;
    }

    let hwaccel = if cfg!(target_os = "windows") {
        "d3d11va"
    } else if cfg!(target_os = "macos") {
        "videotoolbox"
    } else {
        "auto"
    };

    let color_contract = match resolve_cpu_rgba_contract_from_metadata(
        source_format,
        decoded_color_space,
        decoded_color_range,
        source_color,
        path,
    ) {
        Ok(contract) => contract,
        Err(error) => return Some(Err(error)),
    };
    let matrix_name = match color_contract.applied_matrix {
        DecodedVideoMatrix::Bt709 => "bt709",
        DecodedVideoMatrix::Bt2020NonConstant => "bt2020",
        DecodedVideoMatrix::Fcc => "fcc",
        DecodedVideoMatrix::Bt470Bg => "bt470bg",
        DecodedVideoMatrix::Smpte170M => "smpte170m",
        DecodedVideoMatrix::Smpte240M => "smpte240m",
        DecodedVideoMatrix::Rgb => return None,
        DecodedVideoMatrix::Unknown | DecodedVideoMatrix::Unsupported => return None,
    };
    let range_name = match color_contract.applied_range {
        DecodedVideoRange::Limited => "tv",
        DecodedVideoRange::Full => "pc",
        DecodedVideoRange::Unknown => return None,
    };
    let scale_filter = format!(
        "scale={width}:{height}:flags=fast_bilinear:in_color_matrix={matrix_name}:out_color_matrix={matrix_name}:in_range={range_name}:out_range=pc"
    );

    let source_time_arg = match ffmpeg_source_time_arg(source_time) {
        Ok(value) => value,
        Err(reason) => {
            return Some(Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason,
            }));
        }
    };
    let mut command = crate::ffmpeg_command();
    command
        .arg("-v")
        .arg("error")
        .arg("-hwaccel")
        .arg(hwaccel)
        .arg("-ss")
        .arg(source_time_arg)
        .arg("-i")
        .arg(path);
    if let Some(index) = video_stream_index {
        command.arg("-map").arg(format!("0:{index}"));
    }
    command
        .arg("-frames:v")
        .arg("1")
        .arg("-vf")
        .arg(scale_filter)
        .arg("-pix_fmt")
        .arg("rgba")
        .arg("-f")
        .arg("rawvideo")
        .arg("pipe:1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let Some(expected) = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
    else {
        return Some(Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: "external ffmpeg CPU RGBA output size overflows the platform".to_owned(),
        }));
    };
    let (output, stdout_exceeded_expected_size) =
        match run_external_decode_command_cancellable(&mut command, expected, should_cancel) {
            Ok(Some(output)) => output,
            Ok(None) => return Some(Ok(None)),
            Err(_) => return None,
        };

    if !output.status.success() {
        return Some(Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "external ffmpeg CPU RGBA decode failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        }));
    }

    if stdout_exceeded_expected_size {
        return Some(Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "external ffmpeg CPU RGBA decode returned more than the expected {expected} bytes"
            ),
        }));
    }

    if output.stdout.len() < expected {
        return Some(Err(MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!(
                "external ffmpeg CPU RGBA decode returned insufficient bytes: got {}, expect {}",
                output.stdout.len(),
                expected
            ),
        }));
    }

    Some(Ok(Some(RgbaFrame::new(
        width,
        height,
        output.stdout.into_iter().take(expected).collect(),
        color_contract,
        PreviewDecodePath::ExternalFfmpegCpuRgba,
    ))))
}

pub(super) fn run_external_decode_command_cancellable(
    command: &mut Command,
    stdout_retain_bytes: usize,
    should_cancel: &(dyn Fn() -> bool + Send + Sync),
) -> io::Result<Option<(Output, bool)>> {
    let policy = SupervisedProcessPolicy {
        pipe_stdin: false,
        stdout: SupervisedStreamCapture::Head {
            limit_bytes: stdout_retain_bytes,
            reject_excess: false,
        },
        stderr: SupervisedStreamCapture::Tail { limit_bytes: EXTERNAL_DECODE_STDERR_RETAIN_BYTES },
        ..SupervisedProcessPolicy::default()
    };
    match run_supervised_command_while(command, None, policy, should_cancel) {
        Ok(output) => Ok(Some((
            Output {
                status: output.status,
                stdout: output.stdout,
                stderr: output.stderr,
            },
            output.stdout_truncated,
        ))),
        Err(SupervisedProcessError::Canceled { .. }) => Ok(None),
        Err(error) => Err(io::Error::other(error)),
    }
}

pub(super) fn fit_target_size(
    src_width: u32,
    src_height: u32,
    max_width: Option<u32>,
    max_height: Option<u32>,
) -> (u32, u32) {
    let Some(max_w) = max_width.filter(|v| *v > 0) else {
        return (src_width, src_height);
    };
    let Some(max_h) = max_height.filter(|v| *v > 0) else {
        return (src_width, src_height);
    };

    let src_w = src_width as f64;
    let src_h = src_height as f64;
    let scale = (max_w as f64 / src_w).min(max_h as f64 / src_h).min(1.0);

    let mut out_w = (src_w * scale).round().max(1.0) as u32;
    let mut out_h = (src_h * scale).round().max(1.0) as u32;

    if out_w % 2 == 1 {
        out_w = out_w.saturating_sub(1).max(1);
    }
    if out_h % 2 == 1 {
        out_h = out_h.saturating_sub(1).max(1);
    }

    (out_w, out_h)
}
