//! Shared Headless adapters for independent Golden execution slices.

use crate::app::AppState;
use anyhow::{ensure, Context};
use mondrian_core::JobId;
use mondrian_export::queue::ExportJobSnapshot;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const IMPORT_TIMEOUT: Duration = Duration::from_secs(120);

/// Remove one project runtime directory only after its `AppState` has dropped.
#[derive(Debug, Default)]
pub(super) struct DirectoryCleanup {
    path: Option<PathBuf>,
}

impl DirectoryCleanup {
    pub(super) fn track(&mut self, path: Option<PathBuf>) {
        self.path = path;
    }
}

impl Drop for DirectoryCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

pub(super) fn rooted_env_path(
    repository_root: &Path,
    variable: &str,
    default: impl FnOnce() -> PathBuf,
) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                repository_root.join(path)
            }
        })
        .unwrap_or_else(default)
}

pub(super) fn fixture_root(repository_root: &Path) -> PathBuf {
    rooted_env_path(repository_root, "MONDRIAN_GOLDEN_FIXTURE_ROOT", || {
        repository_root.join("tests/fixtures")
    })
}

pub(super) fn new_run_directory(
    repository_root: &Path,
    root_env: &str,
    prefix: &str,
) -> anyhow::Result<PathBuf> {
    let run_root = rooted_env_path(repository_root, root_env, || {
        repository_root.join("target").join("validation").join("runs")
    });
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let directory = run_root.join(format!("{prefix}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("create Golden run directory {}", directory.display()))?;
    Ok(directory)
}

pub(super) fn wait_for_media_imports(state: &mut AppState) -> anyhow::Result<()> {
    let deadline = Instant::now() + IMPORT_TIMEOUT;
    while state.pending_media_import_batches() > 0 {
        state.poll_media_imports();
        if state.pending_media_import_batches() == 0 {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for media import"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

pub(super) fn wait_for_export_job(
    state: &mut AppState,
    job_id: JobId,
    timeout: Duration,
) -> anyhow::Result<ExportJobSnapshot> {
    let deadline = Instant::now() + timeout;
    loop {
        state.poll_export_queue();
        let snapshot = state
            .export_jobs_snapshot()
            .into_iter()
            .find(|snapshot| snapshot.id == job_id)
            .with_context(|| format!("export job disappeared: {job_id}"))?;
        if snapshot.status.is_terminal() {
            return Ok(snapshot);
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for export job {job_id}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(super) fn write_report<T: Serialize>(path: &Path, report: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(report)?;
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}
