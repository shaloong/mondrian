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
}
