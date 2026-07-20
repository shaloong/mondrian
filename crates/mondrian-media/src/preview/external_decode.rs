//! Cancellable lifecycle for the optional external FFmpeg still decoder.
//!
//! The process is an explicit CPU-RGBA fallback for exact still requests. This
//! module owns child creation, bounded cancellation polling, kill/wait, pipe
//! draining, and reader-thread joining so no caller can publish a canceled
//! result or leak a process/thread.

use super::*;
use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::thread;

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
    let mut command = Command::new("ffmpeg");
    command
        .arg("-v")
        .arg("error")
        .arg("-hwaccel")
        .arg(hwaccel)
        .arg("-ss")
        .arg(source_time_arg)
        .arg("-i")
        .arg(path)
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
    let mut child = command.spawn()?;
    let Some(stdout) = child.stdout.take() else {
        terminate_external_decode_child(&mut child);
        return Err(io::Error::other("external decoder stdout was not piped"));
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_external_decode_child(&mut child);
        return Err(io::Error::other("external decoder stderr was not piped"));
    };
    let stdout_reader =
        thread::spawn(move || read_external_decode_pipe_bounded(stdout, stdout_retain_bytes));
    let stderr_reader = thread::spawn(move || {
        read_external_decode_pipe_bounded(stderr, EXTERNAL_DECODE_STDERR_RETAIN_BYTES)
    });

    let status = loop {
        if should_cancel() {
            terminate_external_decode_child(&mut child);
            let _ = join_external_decode_reader(stdout_reader);
            let _ = join_external_decode_reader(stderr_reader);
            return Ok(None);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                terminate_external_decode_child(&mut child);
                let _ = join_external_decode_reader(stdout_reader);
                let _ = join_external_decode_reader(stderr_reader);
                return Err(error);
            }
        }
        thread::sleep(Duration::from_millis(1));
    };

    let (stdout, stdout_exceeded_retain_limit) = join_external_decode_reader(stdout_reader)?;
    let (stderr, _) = join_external_decode_reader(stderr_reader)?;
    Ok(Some((
        Output { status, stdout, stderr },
        stdout_exceeded_retain_limit,
    )))
}

fn terminate_external_decode_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_external_decode_pipe_bounded(
    mut pipe: impl Read,
    retain_bytes: usize,
) -> io::Result<(Vec<u8>, bool)> {
    let mut retained = Vec::with_capacity(retain_bytes.min(64 * 1024));
    let mut exceeded_retain_limit = false;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = pipe.read(&mut buffer)?;
        if read == 0 {
            return Ok((retained, exceeded_retain_limit));
        }
        let remaining = retain_bytes.saturating_sub(retained.len());
        exceeded_retain_limit |= read > remaining;
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
    }
}

fn join_external_decode_reader(
    reader: thread::JoinHandle<io::Result<(Vec<u8>, bool)>>,
) -> io::Result<(Vec<u8>, bool)> {
    reader
        .join()
        .map_err(|_| io::Error::other("external decoder pipe reader panicked"))?
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

#[cfg(test)]
mod tests {
    use super::read_external_decode_pipe_bounded;
    use std::io::Cursor;

    #[test]
    fn pipe_reader_drains_input_while_bounding_retained_bytes() {
        let input = (0..=255).cycle().take(200_000).collect::<Vec<_>>();
        let (retained, exceeded_limit) =
            read_external_decode_pipe_bounded(Cursor::new(&input), 65_537)
                .expect("bounded pipe read");

        assert_eq!(retained.len(), 65_537);
        assert_eq!(retained.as_slice(), &input[..65_537]);
        assert!(exceeded_limit);
    }
}
