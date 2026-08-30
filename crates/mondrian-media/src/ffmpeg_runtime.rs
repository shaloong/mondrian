//! Shared linked-library and command-line FFmpeg runtime policy.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use ffmpeg_next as ffmpeg;
use mondrian_core::{MondrianError, Result};

use crate::ffmpeg_tools::{resolve_ffmpeg_tool, FfmpegTool, FfmpegToolSource};

const REQUIRED_LINKED_DECODERS: &[(&std::ffi::CStr, &str)] = &[
    (c"h264", "H.264"),
    (c"hevc", "HEVC"),
    (c"prores", "ProRes"),
    (c"dnxhd", "DNxHD/DNxHR"),
    (c"av1", "AV1"),
    (c"vp9", "VP9"),
    (c"aac", "AAC"),
    (c"mp3", "MP3"),
    (c"flac", "FLAC"),
    (c"pcm_s16le", "PCM S16LE"),
    (c"pcm_s24le", "PCM S24LE"),
    (c"pcm_s32le", "PCM S32LE"),
    (c"pcm_f32le", "PCM F32LE"),
    (c"png", "PNG"),
    (c"exr", "OpenEXR"),
    (c"dpx", "DPX"),
    (c"tiff", "TIFF"),
];

const REQUIRED_COMMAND_ENCODERS: &[&str] = &[
    "libx264",
    "libx265",
    "libaom-av1",
    "prores_ks",
    "dnxhd",
    "gif",
    "aac",
    "pcm_s16le",
    "pcm_s24le",
    "pcm_s32le",
    "pcm_f32le",
    "png",
    "exr",
    "dpx",
    "tiff",
];

const REQUIRED_COMMAND_FILTERS: &[&str] = &["scale", "setparams", "pan", "anullsrc"];
const REQUIRED_COMMAND_MUXERS: &[&str] =
    &["mp4", "mov", "matroska", "webm", "mxf", "gif", "image2"];

/// Initialize FFmpeg once with Mondrian's product log policy.
pub(crate) fn ensure_ffmpeg_initialized(path: &Path) -> Result<()> {
    static INIT_RESULT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    let init = INIT_RESULT.get_or_init(|| {
        let log_level =
            ffmpeg_log_level_from_env_value(std::env::var("MONDRIAN_FFMPEG_LOG_LEVEL").ok());

        unsafe {
            ffmpeg::ffi::av_log_set_level(log_level);
        }

        ffmpeg::init().map_err(|e| format!("{e}"))
    });
    match init {
        Ok(()) => Ok(()),
        Err(reason) => Err(MondrianError::MediaOpen {
            path: path.display().to_string(),
            reason: format!("ffmpeg init failed: {reason}"),
        }),
    }
}

/// Verify the complete private FFmpeg runtime shipped with a packaged app.
///
/// Unlike ordinary development resolution, this gate rejects command-line
/// tools found only through `PATH`. A release must carry both tools beside the
/// Mondrian executable and must expose every codec/filter/muxer used by
/// production proxy, audio-source, export, and post-encode validation paths.
pub fn verify_ffmpeg_runtime() -> Result<()> {
    let runtime_path = Path::new("<packaged-runtime>");
    ensure_ffmpeg_initialized(runtime_path)?;
    verify_required_decoders(runtime_path)?;
    verify_packaged_command_tools(runtime_path)
}

fn verify_required_decoders(path: &Path) -> Result<()> {
    let missing = REQUIRED_LINKED_DECODERS
        .iter()
        .copied()
        .filter_map(|(codec_name, display_name)| {
            let decoder = unsafe { ffmpeg::ffi::avcodec_find_decoder_by_name(codec_name.as_ptr()) };
            decoder.is_null().then_some(display_name)
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(runtime_error(
        path,
        format!(
            "linked FFmpeg runtime is missing required decoders: {}; install the documented Mondrian product runtime profile",
            missing.join(", ")
        ),
    ))
}

fn verify_packaged_command_tools(path: &Path) -> Result<()> {
    let ffmpeg = require_packaged_tool(path, FfmpegTool::Ffmpeg)?;
    let ffprobe = require_packaged_tool(path, FfmpegTool::Ffprobe)?;

    let version = command_output(
        path,
        Command::new(&ffmpeg).args(["-hide_banner", "-version"]),
    )?;
    if version.contains("--enable-nonfree") {
        return Err(runtime_error(
            path,
            "packaged FFmpeg was built with --enable-nonfree and is not redistributable",
        ));
    }
    command_output(
        path,
        Command::new(&ffprobe).args(["-hide_banner", "-version"]),
    )?;

    let encoders = command_output(
        path,
        Command::new(&ffmpeg).args(["-hide_banner", "-encoders"]),
    )?;
    verify_listing(path, "encoders", &encoders, REQUIRED_COMMAND_ENCODERS)?;
    let filters = command_output(
        path,
        Command::new(&ffmpeg).args(["-hide_banner", "-filters"]),
    )?;
    verify_listing(path, "filters", &filters, REQUIRED_COMMAND_FILTERS)?;
    let muxers = command_output(
        path,
        Command::new(&ffmpeg).args(["-hide_banner", "-muxers"]),
    )?;
    verify_listing(path, "muxers", &muxers, REQUIRED_COMMAND_MUXERS)
}

fn require_packaged_tool(path: &Path, tool: FfmpegTool) -> Result<std::path::PathBuf> {
    let resolved = resolve_ffmpeg_tool(tool);
    if resolved.source == FfmpegToolSource::Packaged {
        return Ok(resolved.path);
    }
    Err(runtime_error(
        path,
        format!(
            "packaged {} is missing beside the Mondrian executable; release verification may not fall back to PATH",
            tool.command_name()
        ),
    ))
}

fn command_output(path: &Path, command: &mut Command) -> Result<String> {
    command.stdin(Stdio::null()).stderr(Stdio::piped()).stdout(Stdio::piped());
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command.output().map_err(|error| {
        runtime_error(
            path,
            format!("failed to start packaged media tool {program}: {error}"),
        )
    })?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(text)
    } else {
        Err(runtime_error(
            path,
            format!(
                "packaged media tool {program} exited with {}: {text}",
                output.status
            ),
        ))
    }
}

fn verify_listing(path: &Path, kind: &str, listing: &str, required: &[&str]) -> Result<()> {
    let missing = required
        .iter()
        .copied()
        .filter(|name| !listing_contains_name(listing, name))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(runtime_error(
        path,
        format!(
            "packaged FFmpeg is missing required {kind}: {}",
            missing.join(", ")
        ),
    ))
}

fn listing_contains_name(listing: &str, expected: &str) -> bool {
    listing
        .split_whitespace()
        .any(|field| field.split(',').any(|name| name == expected))
}

fn runtime_error(path: &Path, reason: impl Into<String>) -> MondrianError {
    MondrianError::MediaOpen {
        path: path.display().to_string(),
        reason: reason.into(),
    }
}

pub(crate) fn ffmpeg_log_level_from_env_value(value: Option<String>) -> i32 {
    value
        .map(|value| value.to_ascii_lowercase())
        .and_then(|value| match value.as_str() {
            "quiet" => Some(ffmpeg::ffi::AV_LOG_QUIET),
            "panic" => Some(ffmpeg::ffi::AV_LOG_PANIC),
            "fatal" => Some(ffmpeg::ffi::AV_LOG_FATAL),
            "error" => Some(ffmpeg::ffi::AV_LOG_ERROR),
            "warning" => Some(ffmpeg::ffi::AV_LOG_WARNING),
            "info" => Some(ffmpeg::ffi::AV_LOG_INFO),
            "verbose" => Some(ffmpeg::ffi::AV_LOG_VERBOSE),
            "debug" => Some(ffmpeg::ffi::AV_LOG_DEBUG),
            "trace" => Some(ffmpeg::ffi::AV_LOG_TRACE),
            _ => None,
        })
        .unwrap_or(ffmpeg::ffi::AV_LOG_FATAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_log_level_defaults_to_fatal_for_product_preview() {
        assert_eq!(
            ffmpeg_log_level_from_env_value(None),
            ffmpeg::ffi::AV_LOG_FATAL
        );
        assert_eq!(
            ffmpeg_log_level_from_env_value(Some("error".to_owned())),
            ffmpeg::ffi::AV_LOG_ERROR
        );
        assert_eq!(
            ffmpeg_log_level_from_env_value(Some("debug".to_owned())),
            ffmpeg::ffi::AV_LOG_DEBUG
        );
        assert_eq!(
            ffmpeg_log_level_from_env_value(Some("unknown".to_owned())),
            ffmpeg::ffi::AV_LOG_FATAL
        );
    }

    #[test]
    fn linked_runtime_requires_the_product_decoder_baseline() {
        ensure_ffmpeg_initialized(Path::new("<test-runtime>")).expect("FFmpeg init");
        verify_required_decoders(Path::new("<test-runtime>"))
            .expect("linked FFmpeg decoder contract");
    }

    #[test]
    fn command_listing_matches_exact_component_names() {
        let listing =
            " V..... libx264             libx264 H.264\n E mov,mp4,m4a            QuickTime / MOV";

        assert!(listing_contains_name(listing, "libx264"));
        assert!(listing_contains_name(listing, "mp4"));
        assert!(!listing_contains_name(listing, "x264"));
        assert!(!listing_contains_name(listing, "scale"));
    }
}
