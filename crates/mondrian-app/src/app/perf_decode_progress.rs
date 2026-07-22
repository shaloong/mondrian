//! Crash-surviving decode progress journal for ignored playback acceptance runs.
//!
//! The sampler owns only a cloneable read watch. It never touches Preview
//! Runtime state, media resources, cancellation, or recovery, so it can keep
//! recording when the gate's main thread is blocked in presentation code.

use super::preview_runtime::{
    PreviewDecodeWorkerExecutionDiagnostics, PreviewDecodeWorkerExecutionWatch,
};
use anyhow::Context;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(super) const PREVIEW_DECODE_EXECUTION_JOURNAL_ENV: &str =
    "MONDRIAN_PREVIEW_DECODE_EXECUTION_OUTPUT";

const JOURNAL_SCHEMA_VERSION: u32 = 3;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize)]
struct PreviewDecodeExecutionJournalRecord {
    schema_version: u32,
    scenario: &'static str,
    observed_at_us: u64,
    terminal: bool,
    workers: PreviewDecodeWorkerExecutionDiagnostics,
}

/// Bounded-latency owner of one independent progress sampler thread.
pub(super) struct PreviewDecodeExecutionJournal {
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl PreviewDecodeExecutionJournal {
    /// Start a journal only when the acceptance orchestrator supplied a path.
    pub(super) fn start_from_env(
        watch: PreviewDecodeWorkerExecutionWatch,
        scenario: &'static str,
    ) -> anyhow::Result<Option<Self>> {
        let Some(path) = std::env::var_os(PREVIEW_DECODE_EXECUTION_JOURNAL_ENV) else {
            return Ok(None);
        };
        Self::start(watch, scenario, PathBuf::from(path)).map(Some)
    }

    fn start(
        watch: PreviewDecodeWorkerExecutionWatch,
        scenario: &'static str,
        path: PathBuf,
    ) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "create Preview decode execution journal directory {}",
                    parent.display()
                )
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open Preview decode execution journal {}", path.display()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let worker_stop = Arc::clone(&stop);
        let worker_error = Arc::clone(&error);
        let handle = thread::Builder::new()
            .name("mondrian-preview-decode-progress".to_owned())
            .spawn(move || run_journal(file, watch, scenario, worker_stop, worker_error))
            .context("start Preview decode execution journal sampler")?;
        Ok(Self { stop, error, handle: Some(handle) })
    }

    /// Flush the terminal snapshot and surface journal I/O/thread failures.
    pub(super) fn finish(mut self) -> anyhow::Result<()> {
        self.stop.store(true, Ordering::Release);
        self.join()?;
        let error = lock_error(&self.error).take();
        if let Some(error) = error {
            anyhow::bail!("Preview decode execution journal failed: {error}");
        }
        Ok(())
    }

    fn join(&mut self) -> anyhow::Result<()> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        handle
            .join()
            .map_err(|_| anyhow::anyhow!("Preview decode execution journal panicked"))
    }
}

impl Drop for PreviewDecodeExecutionJournal {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.join();
    }
}

fn run_journal(
    file: File,
    watch: PreviewDecodeWorkerExecutionWatch,
    scenario: &'static str,
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
) {
    let mut writer = BufWriter::new(file);
    let started_at = Instant::now();
    let mut previous = None;
    let mut last_heartbeat = started_at;
    loop {
        let terminal = stop.load(Ordering::Acquire);
        let workers = watch.snapshot();
        let now = Instant::now();
        if terminal
            || previous != Some(workers)
            || now.saturating_duration_since(last_heartbeat) >= HEARTBEAT_INTERVAL
        {
            let record = PreviewDecodeExecutionJournalRecord {
                schema_version: JOURNAL_SCHEMA_VERSION,
                scenario,
                observed_at_us: duration_us(now.saturating_duration_since(started_at)),
                terminal,
                workers,
            };
            if let Err(write_error) = write_record(&mut writer, &record) {
                *lock_error(&error) = Some(write_error.to_string());
                return;
            }
            previous = Some(workers);
            last_heartbeat = now;
        }
        if terminal {
            return;
        }
        thread::sleep(SAMPLE_INTERVAL);
    }
}

fn write_record(
    writer: &mut BufWriter<File>,
    record: &PreviewDecodeExecutionJournalRecord,
) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *writer, record).context("serialize progress record")?;
    writer.write_all(b"\n").context("terminate progress record")?;
    // Each sparse transition/heartbeat must survive an externally terminated
    // gate process; buffering until normal test completion would lose the one
    // observation this journal exists to preserve.
    writer.flush().context("flush progress record")
}

fn lock_error(error: &Mutex<Option<String>>) -> std::sync::MutexGuard<'_, Option<String>> {
    match error.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn duration_us(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_flushes_a_terminal_jsonl_record_without_runtime_ownership() {
        let path = std::env::temp_dir().join(format!(
            "mondrian-preview-decode-progress-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let journal = PreviewDecodeExecutionJournal::start(
            PreviewDecodeWorkerExecutionWatch::default(),
            "journal-test",
            path.clone(),
        )
        .expect("journal starts");
        journal.finish().expect("journal finishes");

        let contents = std::fs::read_to_string(&path).expect("journal is readable");
        let last = contents.lines().last().expect("terminal record");
        let value: serde_json::Value = serde_json::from_str(last).expect("valid JSONL");
        assert_eq!(value["schema_version"], JOURNAL_SCHEMA_VERSION);
        assert_eq!(value["scenario"], "journal-test");
        assert_eq!(value["terminal"], true);
        let _ = std::fs::remove_file(path);
    }
}
