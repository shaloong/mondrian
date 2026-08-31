use crate::store::{
    StoreLookupDisposition, TimelineRenderCacheStore, TimelineRenderCacheStoreError,
};
use crate::{TimelineRenderCacheFrame, TimelineRenderCacheIdentity};
use mondrian_core::WorkingColorSpace;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

/// Bounded physical policy for one Timeline render-cache service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineRenderCacheConfig {
    /// Absolute owner-selected cache root. The service adds a versioned child namespace.
    pub root: PathBuf,
    /// Maximum compressed bytes retained on disk.
    pub max_disk_bytes: u64,
    /// Maximum compressed or decoded bytes accepted for one artifact.
    pub max_artifact_bytes: u64,
    /// Maximum queued lookups/publications behind the active operation.
    pub queue_capacity: usize,
}

impl TimelineRenderCacheConfig {
    /// Validate and freeze one service policy.
    pub fn new(
        root: PathBuf,
        max_disk_bytes: u64,
        max_artifact_bytes: u64,
        queue_capacity: usize,
    ) -> Result<Self, TimelineRenderCacheConfigError> {
        if !root.is_absolute() {
            return Err(TimelineRenderCacheConfigError::RelativeRoot(root));
        }
        if max_disk_bytes == 0 {
            return Err(TimelineRenderCacheConfigError::ZeroDiskBudget);
        }
        if max_artifact_bytes == 0 || max_artifact_bytes > max_disk_bytes {
            return Err(TimelineRenderCacheConfigError::ArtifactBudget {
                artifact: max_artifact_bytes,
                disk: max_disk_bytes,
            });
        }
        if queue_capacity == 0 {
            return Err(TimelineRenderCacheConfigError::ZeroQueueCapacity);
        }
        Ok(Self {
            root,
            max_disk_bytes,
            max_artifact_bytes,
            queue_capacity,
        })
    }
}

/// Invalid physical cache policy.
#[derive(Debug, thiserror::Error)]
pub enum TimelineRenderCacheConfigError {
    /// Cache roots are frozen before worker admission.
    #[error("Timeline render-cache root must be absolute: {0}")]
    RelativeRoot(PathBuf),
    /// A zero-byte store cannot publish any artifact.
    #[error("Timeline render-cache disk budget must be non-zero")]
    ZeroDiskBudget,
    /// Per-artifact bound must be non-zero and no larger than the store.
    #[error("Timeline render-cache artifact budget {artifact} is invalid for disk budget {disk}")]
    ArtifactBudget { artifact: u64, disk: u64 },
    /// A zero-capacity command queue cannot admit work.
    #[error("Timeline render-cache queue capacity must be non-zero")]
    ZeroQueueCapacity,
}

/// Non-blocking command admission outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineRenderCacheSubmission {
    /// Work entered the independently bounded cache queue.
    Scheduled,
    /// Equivalent work is queued or executing already.
    AlreadyPending,
    /// The cache queue has no optional capacity; ordinary rendering should continue.
    Busy,
    /// Worker startup or connectivity was lost; ordinary rendering should continue.
    Disconnected,
}

/// Terminal lookup outcome for one exact content identity.
pub enum TimelineRenderCacheLookup {
    /// Verified working-linear artifact.
    Hit(TimelineRenderCacheFrame),
    /// No artifact exists.
    Miss,
    /// An invalid artifact was removed without invalidating unrelated identities.
    CorruptRemoved { detail: String },
}

/// Pollable terminal result from the background service.
pub enum TimelineRenderCacheResult {
    /// Lookup completed without blocking the caller thread.
    Lookup {
        /// Exact requested content identity.
        identity: TimelineRenderCacheIdentity,
        /// Verified/missing/corrupt disposition.
        result: TimelineRenderCacheLookup,
    },
    /// A complete artifact was durably published, then local pressure eviction ran.
    Published {
        /// Exact published content identity.
        identity: TimelineRenderCacheIdentity,
        /// Compressed artifact size.
        artifact_bytes: u64,
    },
    /// Cache work failed; the production renderer remains authoritative.
    Failed {
        /// Identity whose optional cache work failed.
        identity: TimelineRenderCacheIdentity,
        /// Stable diagnostic detail.
        detail: String,
    },
}

/// Point-in-time service and physical residency evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct TimelineRenderCacheDiagnostics {
    /// Lookup commands admitted.
    pub lookup_submissions: u64,
    /// Publication commands admitted.
    pub publication_submissions: u64,
    /// Commands rejected by queue pressure.
    pub busy_rejections: u64,
    /// Verified artifact hits.
    pub hits: u64,
    /// Artifact misses.
    pub misses: u64,
    /// Corrupt artifacts removed locally.
    pub corruptions: u64,
    /// Durable artifact publications.
    pub publications: u64,
    /// Optional cache operation failures.
    pub failures: u64,
    /// Entries evicted under disk pressure.
    pub evicted_entries: u64,
    /// Bytes evicted under disk pressure.
    pub evicted_bytes: u64,
    /// Current indexed artifact count.
    pub resident_entries: u64,
    /// Current indexed compressed bytes.
    pub resident_bytes: u64,
    /// Terminal results dropped because the result queue was not polled.
    pub dropped_results: u64,
}

#[derive(Default)]
struct SharedDiagnostics {
    lookup_submissions: AtomicU64,
    publication_submissions: AtomicU64,
    busy_rejections: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    corruptions: AtomicU64,
    publications: AtomicU64,
    failures: AtomicU64,
    evicted_entries: AtomicU64,
    evicted_bytes: AtomicU64,
    resident_entries: AtomicU64,
    resident_bytes: AtomicU64,
    dropped_results: AtomicU64,
}

impl SharedDiagnostics {
    fn snapshot(&self) -> TimelineRenderCacheDiagnostics {
        TimelineRenderCacheDiagnostics {
            lookup_submissions: self.lookup_submissions.load(Ordering::Relaxed),
            publication_submissions: self.publication_submissions.load(Ordering::Relaxed),
            busy_rejections: self.busy_rejections.load(Ordering::Relaxed),
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            corruptions: self.corruptions.load(Ordering::Relaxed),
            publications: self.publications.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            evicted_entries: self.evicted_entries.load(Ordering::Relaxed),
            evicted_bytes: self.evicted_bytes.load(Ordering::Relaxed),
            resident_entries: self.resident_entries.load(Ordering::Relaxed),
            resident_bytes: self.resident_bytes.load(Ordering::Relaxed),
            dropped_results: self.dropped_results.load(Ordering::Relaxed),
        }
    }
}

enum CacheCommand {
    Lookup {
        identity: TimelineRenderCacheIdentity,
        color_space: WorkingColorSpace,
    },
    Publish(TimelineRenderCacheFrame),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PendingKind {
    Lookup,
    Publish,
}

type PendingSet = Arc<Mutex<HashSet<(TimelineRenderCacheIdentity, PendingKind)>>>;

/// Independently bounded background owner for persistent Timeline render-cache I/O.
pub struct TimelineRenderCacheService {
    commands: Option<mpsc::SyncSender<CacheCommand>>,
    results: Option<mpsc::Receiver<TimelineRenderCacheResult>>,
    pending: PendingSet,
    diagnostics: Arc<SharedDiagnostics>,
    worker: Option<JoinHandle<()>>,
}

/// Synchronous terminal evidence for the cache service's sole worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineRenderCacheShutdownEvidence {
    /// Whether this service instance owned a worker when shutdown began.
    pub worker_started: bool,
    /// Whether the worker was synchronously joined by the caller.
    pub worker_terminated: bool,
    /// Whether joining observed a worker panic.
    pub worker_panicked: bool,
    /// Whether joining was impossible because shutdown ran on the worker itself.
    pub current_thread_skipped: bool,
}

impl TimelineRenderCacheShutdownEvidence {
    /// Whether every started worker returned without panic or detachment.
    pub const fn all_workers_terminated(self) -> bool {
        !self.worker_started
            || (self.worker_terminated && !self.worker_panicked && !self.current_thread_skipped)
    }
}

impl TimelineRenderCacheService {
    /// Start one cache worker. Filesystem scanning and all artifact work occur on it.
    pub fn start(config: TimelineRenderCacheConfig) -> Result<Self, std::io::Error> {
        Self::start_with_notifier(config, Arc::new(|| {}))
    }

    /// Start one cache worker with a lightweight result-availability callback.
    ///
    /// The callback carries no payload or authority; it may only wake the
    /// consumer that subsequently polls [`Self::try_poll`].
    pub fn start_with_notifier(
        config: TimelineRenderCacheConfig,
        notifier: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, std::io::Error> {
        let capacity = config.queue_capacity;
        let (command_tx, command_rx) = mpsc::sync_channel(capacity);
        let (result_tx, result_rx) = mpsc::sync_channel(capacity);
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let diagnostics = Arc::new(SharedDiagnostics::default());
        let worker_pending = Arc::clone(&pending);
        let worker_diagnostics = Arc::clone(&diagnostics);
        let worker = thread::Builder::new()
            .name("mondrian-timeline-render-cache".to_owned())
            .spawn(move || {
                cache_worker(
                    config,
                    command_rx,
                    result_tx,
                    worker_pending,
                    worker_diagnostics,
                    notifier,
                );
            })?;
        Ok(Self {
            commands: Some(command_tx),
            results: Some(result_rx),
            pending,
            diagnostics,
            worker: Some(worker),
        })
    }

    /// Request an asynchronous verified lookup.
    pub fn lookup(
        &self,
        identity: TimelineRenderCacheIdentity,
        color_space: WorkingColorSpace,
    ) -> TimelineRenderCacheSubmission {
        self.submit(
            identity,
            PendingKind::Lookup,
            CacheCommand::Lookup { identity, color_space },
        )
    }

    /// Request asynchronous compression and durable publication.
    pub fn publish(&self, frame: TimelineRenderCacheFrame) -> TimelineRenderCacheSubmission {
        let identity = frame.identity();
        self.submit(identity, PendingKind::Publish, CacheCommand::Publish(frame))
    }

    /// Poll one terminal without waiting.
    pub fn try_poll(&self) -> Option<TimelineRenderCacheResult> {
        self.results.as_ref()?.try_recv().ok()
    }

    /// Snapshot bounded cache health and residency.
    pub fn diagnostics(&self) -> TimelineRenderCacheDiagnostics {
        self.diagnostics.snapshot()
    }

    /// Close admission and synchronously reclaim the cache worker.
    pub fn shutdown_and_wait(mut self) -> TimelineRenderCacheShutdownEvidence {
        self.stop_worker()
    }

    fn stop_worker(&mut self) -> TimelineRenderCacheShutdownEvidence {
        self.commands.take();
        self.results.take();
        let Some(worker) = self.worker.take() else {
            return TimelineRenderCacheShutdownEvidence {
                worker_started: false,
                worker_terminated: true,
                worker_panicked: false,
                current_thread_skipped: false,
            };
        };
        if worker.thread().id() == thread::current().id() {
            drop(worker);
            return TimelineRenderCacheShutdownEvidence {
                worker_started: true,
                worker_terminated: false,
                worker_panicked: false,
                current_thread_skipped: true,
            };
        }
        let worker_panicked = worker.join().is_err();
        TimelineRenderCacheShutdownEvidence {
            worker_started: true,
            worker_terminated: true,
            worker_panicked,
            current_thread_skipped: false,
        }
    }

    fn submit(
        &self,
        identity: TimelineRenderCacheIdentity,
        kind: PendingKind,
        command: CacheCommand,
    ) -> TimelineRenderCacheSubmission {
        let mut pending = lock_pending(&self.pending);
        if !pending.insert((identity, kind)) {
            return TimelineRenderCacheSubmission::AlreadyPending;
        }
        let Some(commands) = self.commands.as_ref() else {
            pending.remove(&(identity, kind));
            return TimelineRenderCacheSubmission::Disconnected;
        };
        match commands.try_send(command) {
            Ok(()) => {
                match kind {
                    PendingKind::Lookup => {
                        self.diagnostics.lookup_submissions.fetch_add(1, Ordering::Relaxed);
                    }
                    PendingKind::Publish => {
                        self.diagnostics.publication_submissions.fetch_add(1, Ordering::Relaxed);
                    }
                }
                TimelineRenderCacheSubmission::Scheduled
            }
            Err(mpsc::TrySendError::Full(_)) => {
                pending.remove(&(identity, kind));
                self.diagnostics.busy_rejections.fetch_add(1, Ordering::Relaxed);
                TimelineRenderCacheSubmission::Busy
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                pending.remove(&(identity, kind));
                TimelineRenderCacheSubmission::Disconnected
            }
        }
    }
}

impl Drop for TimelineRenderCacheService {
    fn drop(&mut self) {
        let evidence = self.stop_worker();
        if evidence.worker_panicked {
            tracing::warn!("Timeline render-cache worker panicked during shutdown");
        }
        if evidence.current_thread_skipped {
            tracing::warn!("Timeline render-cache shutdown detached its current worker thread");
        }
    }
}

fn cache_worker(
    config: TimelineRenderCacheConfig,
    commands: mpsc::Receiver<CacheCommand>,
    results: mpsc::SyncSender<TimelineRenderCacheResult>,
    pending: PendingSet,
    diagnostics: Arc<SharedDiagnostics>,
    notifier: Arc<dyn Fn() + Send + Sync>,
) {
    let mut store = TimelineRenderCacheStore::open(
        &config.root,
        config.max_disk_bytes,
        config.max_artifact_bytes,
    );
    if let Ok(store) = &store {
        update_residency(&diagnostics, store);
    }
    while let Ok(command) = commands.recv() {
        let (identity, kind, result) = match command {
            CacheCommand::Lookup { identity, color_space } => {
                let result = execute_lookup(&mut store, identity, color_space, &diagnostics);
                (identity, PendingKind::Lookup, result)
            }
            CacheCommand::Publish(frame) => {
                let identity = frame.identity();
                let result = execute_publication(&mut store, &frame, &diagnostics);
                (identity, PendingKind::Publish, result)
            }
        };
        lock_pending(&pending).remove(&(identity, kind));
        if results.try_send(result).is_err() {
            diagnostics.dropped_results.fetch_add(1, Ordering::Relaxed);
        } else {
            notifier();
        }
    }
}

fn execute_lookup(
    store: &mut Result<TimelineRenderCacheStore, TimelineRenderCacheStoreError>,
    identity: TimelineRenderCacheIdentity,
    color_space: WorkingColorSpace,
    diagnostics: &SharedDiagnostics,
) -> TimelineRenderCacheResult {
    let Some(store) = store.as_mut().ok() else {
        diagnostics.failures.fetch_add(1, Ordering::Relaxed);
        return TimelineRenderCacheResult::Failed { identity, detail: store_error_detail(store) };
    };
    match store.lookup(identity, color_space) {
        Ok(lookup) => {
            update_residency(diagnostics, store);
            let result = match lookup.disposition {
                StoreLookupDisposition::Hit => {
                    diagnostics.hits.fetch_add(1, Ordering::Relaxed);
                    match lookup.frame {
                        Some(frame) => TimelineRenderCacheLookup::Hit(frame),
                        None => {
                            diagnostics.failures.fetch_add(1, Ordering::Relaxed);
                            return TimelineRenderCacheResult::Failed {
                                identity,
                                detail: "cache Store reported Hit without a frame".to_owned(),
                            };
                        }
                    }
                }
                StoreLookupDisposition::Miss => {
                    diagnostics.misses.fetch_add(1, Ordering::Relaxed);
                    TimelineRenderCacheLookup::Miss
                }
                StoreLookupDisposition::CorruptRemoved => {
                    diagnostics.corruptions.fetch_add(1, Ordering::Relaxed);
                    TimelineRenderCacheLookup::CorruptRemoved {
                        detail: lookup
                            .detail
                            .unwrap_or_else(|| "invalid cache artifact".to_owned()),
                    }
                }
            };
            TimelineRenderCacheResult::Lookup { identity, result }
        }
        Err(error) => {
            diagnostics.failures.fetch_add(1, Ordering::Relaxed);
            TimelineRenderCacheResult::Failed { identity, detail: error.to_string() }
        }
    }
}

fn execute_publication(
    store: &mut Result<TimelineRenderCacheStore, TimelineRenderCacheStoreError>,
    frame: &TimelineRenderCacheFrame,
    diagnostics: &SharedDiagnostics,
) -> TimelineRenderCacheResult {
    let identity = frame.identity();
    let Some(store) = store.as_mut().ok() else {
        diagnostics.failures.fetch_add(1, Ordering::Relaxed);
        return TimelineRenderCacheResult::Failed { identity, detail: store_error_detail(store) };
    };
    match store.publish(frame) {
        Ok(publication) => {
            diagnostics.publications.fetch_add(1, Ordering::Relaxed);
            diagnostics
                .evicted_entries
                .fetch_add(publication.evicted_entries, Ordering::Relaxed);
            diagnostics
                .evicted_bytes
                .fetch_add(publication.evicted_bytes, Ordering::Relaxed);
            update_residency(diagnostics, store);
            TimelineRenderCacheResult::Published {
                identity,
                artifact_bytes: publication.artifact_bytes,
            }
        }
        Err(error) => {
            diagnostics.failures.fetch_add(1, Ordering::Relaxed);
            TimelineRenderCacheResult::Failed { identity, detail: error.to_string() }
        }
    }
}

fn store_error_detail(
    store: &Result<TimelineRenderCacheStore, TimelineRenderCacheStoreError>,
) -> String {
    store
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_else(|| "Timeline render-cache Store is unavailable".to_owned())
}

fn update_residency(diagnostics: &SharedDiagnostics, store: &TimelineRenderCacheStore) {
    diagnostics.resident_entries.store(store.resident_entries(), Ordering::Relaxed);
    diagnostics.resident_bytes.store(store.resident_bytes(), Ordering::Relaxed);
}

fn lock_pending(
    pending: &PendingSet,
) -> MutexGuard<'_, HashSet<(TimelineRenderCacheIdentity, PendingKind)>> {
    pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::WorkingRgbaF32Frame;
    use std::time::{Duration, Instant};

    fn service(root: PathBuf) -> TimelineRenderCacheService {
        TimelineRenderCacheService::start(
            TimelineRenderCacheConfig::new(root, 1_048_576, 1_048_576, 2).expect("config"),
        )
        .expect("service")
    }

    fn frame() -> TimelineRenderCacheFrame {
        TimelineRenderCacheFrame::new(
            TimelineRenderCacheIdentity::from_digest([9; 32]),
            WorkingRgbaF32Frame {
                width: 16,
                height: 16,
                data: vec![[0.25, 0.5, 1.0, 1.0]; 256],
                color_space: WorkingColorSpace::LinearRec709,
            },
        )
        .expect("frame")
    }

    fn wait_result(service: &TimelineRenderCacheService) -> TimelineRenderCacheResult {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = service.try_poll() {
                return result;
            }
            assert!(Instant::now() < deadline, "cache result timed out");
            std::thread::yield_now();
        }
    }

    #[test]
    fn worker_publishes_then_returns_verified_hit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = service(temp.path().to_path_buf());
        let frame = frame();
        assert_eq!(
            service.publish(frame.clone()),
            TimelineRenderCacheSubmission::Scheduled
        );
        assert!(matches!(
            wait_result(&service),
            TimelineRenderCacheResult::Published { .. }
        ));
        assert_eq!(
            service.lookup(frame.identity(), WorkingColorSpace::LinearRec709),
            TimelineRenderCacheSubmission::Scheduled
        );
        let result = wait_result(&service);
        assert!(matches!(
            result,
            TimelineRenderCacheResult::Lookup { result: TimelineRenderCacheLookup::Hit(_), .. }
        ));
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.publications, 1);
        assert_eq!(diagnostics.hits, 1);
        assert_eq!(diagnostics.resident_entries, 1);
    }

    #[test]
    fn duplicate_identity_is_deduplicated_while_pending() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = service(temp.path().to_path_buf());
        let frame = frame();
        assert_eq!(
            service.publish(frame.clone()),
            TimelineRenderCacheSubmission::Scheduled
        );
        assert_eq!(
            service.publish(frame),
            TimelineRenderCacheSubmission::AlreadyPending
        );
        let _ = wait_result(&service);
    }

    #[test]
    fn synchronous_shutdown_returns_worker_terminal_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let evidence = service(temp.path().to_path_buf()).shutdown_and_wait();

        assert!(evidence.worker_started);
        assert!(evidence.worker_terminated);
        assert!(!evidence.worker_panicked);
        assert!(!evidence.current_thread_skipped);
        assert!(evidence.all_workers_terminated());
    }
}
