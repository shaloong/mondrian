//! Shared FFmpeg process runtime policy for media probing and preview decode.

use std::path::Path;
use std::sync::OnceLock;

use ffmpeg_next as ffmpeg;
use mondrian_core::{MondrianError, Result};

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

/// Verify initialization and required decoders in the packaged FFmpeg runtime.
pub fn verify_ffmpeg_runtime() -> Result<()> {
    let runtime_path = Path::new("<packaged-runtime>");
    ensure_ffmpeg_initialized(runtime_path)?;
    verify_required_decoders(runtime_path)
}

fn verify_required_decoders(path: &Path) -> Result<()> {
    let required = [(c"png", "PNG"), (c"exr", "OpenEXR")];
    let missing = required
        .into_iter()
        .filter_map(|(codec_name, display_name)| {
            let decoder = unsafe { ffmpeg::ffi::avcodec_find_decoder_by_name(codec_name.as_ptr()) };
            decoder.is_null().then_some(display_name)
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(MondrianError::MediaOpen {
        path: path.display().to_string(),
        reason: format!(
            "linked FFmpeg runtime is missing required decoders: {}; Windows builds must install vcpkg ffmpeg[zlib]",
            missing.join(", ")
        ),
    })
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
    fn packaged_runtime_requires_png_and_openexr_decoders() {
        verify_ffmpeg_runtime().expect("packaged FFmpeg decoder contract");
    }
}
