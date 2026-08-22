//! Product tracing initialization and bounded persistent log retention.
//!
//! The Window Adapter owns this Module's guard for the complete native process
//! lifetime. Callers choose a filter through `RUST_LOG`; log placement,
//! rotation, non-blocking I/O, and flush-on-drop remain behind this Interface.

use std::fs;
use std::path::PathBuf;

use mondrian_platform::UserStateDirectory;
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

pub(crate) const DEFAULT_APP_UI_LOG_FILTER: &str = "info,wgpu_core=warn,wgpu_hal=warn,naga=warn";
#[cfg(not(test))]
pub(crate) const FORCED_PROCESS_EXIT_CODE: u32 = 70;

const PRODUCT_LOG_DIRECTORY: &str = "logs";
const PRODUCT_LOG_FILE_PREFIX: &str = "mondrian";
const PRODUCT_LOG_FILE_SUFFIX: &str = "jsonl";
const PRODUCT_LOG_FILE_RETENTION: usize = 8;

/// Keeps the non-blocking writer alive and flushes queued events on ordinary
/// process return.
pub(crate) struct ProductTracingGuard {
    _file_guard: Option<WorkerGuard>,
}

/// Install the sole product tracing subscriber.
///
/// Persistent logging is best-effort during very early startup: failure to
/// resolve or create the user-state log directory falls back to stderr while
/// preserving the same filtering contract.
pub(crate) fn init_product_tracing(
    state_directory: &dyn UserStateDirectory,
) -> ProductTracingGuard {
    let file_output = match build_product_log_writer(state_directory) {
        Ok(output) => Some(output),
        Err(error) => {
            eprintln!("Mondrian persistent logging unavailable: {error}");
            None
        }
    };
    let log_directory = file_output.as_ref().map(|(_, _, directory)| directory.clone());
    let (file_writer, file_guard) = match file_output {
        Some((writer, guard, _)) => (Some(writer), Some(guard)),
        None => (None, None),
    };
    let file_layer = file_writer.map(|writer| {
        tracing_subscriber::fmt::layer()
            .json()
            .with_ansi(false)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_writer(writer)
    });
    let console_layer = tracing_subscriber::fmt::layer()
        .compact()
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_writer(std::io::stderr);

    if tracing_subscriber::registry()
        .with(app_ui_log_filter())
        .with(console_layer)
        .with(file_layer)
        .try_init()
        .is_err()
    {
        eprintln!("Mondrian tracing subscriber was already initialized; product logging skipped");
        return ProductTracingGuard { _file_guard: None };
    }

    if let Some(directory) = log_directory {
        tracing::info!(
            path = %directory.display(),
            retention_files = PRODUCT_LOG_FILE_RETENTION,
            "persistent product logging initialized"
        );
    } else {
        tracing::warn!("persistent product logging unavailable; stderr remains active");
    }

    ProductTracingGuard { _file_guard: file_guard }
}

fn app_ui_log_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_APP_UI_LOG_FILTER))
}

fn build_product_log_writer(
    state_directory: &dyn UserStateDirectory,
) -> Result<(NonBlocking, WorkerGuard, PathBuf), String> {
    let log_directory = product_log_directory(state_directory)?;
    fs::create_dir_all(&log_directory).map_err(|error| {
        format!(
            "failed to create product log directory '{}': {error}",
            log_directory.display()
        )
    })?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(PRODUCT_LOG_FILE_PREFIX)
        .filename_suffix(PRODUCT_LOG_FILE_SUFFIX)
        .max_log_files(PRODUCT_LOG_FILE_RETENTION)
        .build(&log_directory)
        .map_err(|error| {
            format!(
                "failed to open rolling product log under '{}': {error}",
                log_directory.display()
            )
        })?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    Ok((writer, guard, log_directory))
}

fn product_log_directory(state_directory: &dyn UserStateDirectory) -> Result<PathBuf, String> {
    let root = state_directory
        .user_state_directory()
        .map_err(|error| format!("failed to resolve stable per-user log root: {error}"))?;
    if !root.is_absolute() {
        return Err(format!(
            "stable per-user log root is not absolute: {}",
            root.display()
        ));
    }
    Ok(root.join("Mondrian").join(PRODUCT_LOG_DIRECTORY))
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    use mondrian_platform::UserStateDirectoryError;

    use super::*;

    struct FixedStateDirectory(PathBuf);

    impl UserStateDirectory for FixedStateDirectory {
        fn user_state_directory(&self) -> Result<PathBuf, UserStateDirectoryError> {
            Ok(self.0.clone())
        }
    }

    fn unique_test_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).expect("system time").as_nanos();
        std::env::temp_dir().join(format!(
            "mondrian-product-logging-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn product_log_directory_uses_the_platform_state_adapter() {
        let root = unique_test_root("directory");
        assert_eq!(
            product_log_directory(&FixedStateDirectory(root.clone())).expect("log directory"),
            root.join("Mondrian").join(PRODUCT_LOG_DIRECTORY)
        );
    }

    #[test]
    fn non_blocking_product_writer_flushes_and_bounds_matching_log_files() {
        let root = unique_test_root("writer");
        let expected_directory = root.join("Mondrian").join(PRODUCT_LOG_DIRECTORY);
        fs::create_dir_all(&expected_directory).expect("create test log directory");
        for day in 1..=12 {
            fs::write(
                expected_directory.join(format!("mondrian.2000-01-{day:02}.jsonl")),
                b"old\n",
            )
            .expect("write old log");
        }

        let (mut writer, guard, actual_directory) =
            build_product_log_writer(&FixedStateDirectory(root)).expect("product writer");
        writer.write_all(b"terminal product evidence\n").expect("queue log line");
        drop(writer);
        drop(guard);

        assert_eq!(actual_directory, expected_directory);
        let matching = fs::read_dir(&expected_directory)
            .expect("read log directory")
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with(PRODUCT_LOG_FILE_PREFIX) && name.ends_with(PRODUCT_LOG_FILE_SUFFIX)
            })
            .collect::<Vec<_>>();
        assert!(matching.len() <= PRODUCT_LOG_FILE_RETENTION);
        assert!(matching.iter().any(|entry| {
            fs::read(entry.path())
                .map(|bytes| {
                    bytes
                        .windows(b"terminal product evidence".len())
                        .any(|window| window == b"terminal product evidence")
                })
                .unwrap_or(false)
        }));
    }
}
