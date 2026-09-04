//! Unpublished Waveform ownership. Every returned resource is installed before
//! the next startup step; consuming failure cleanup uses the ordinary owner.

use super::*;

/// Actual inventory installed before Waveform startup failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioWaveformStartupStage {
    /// Only inert channels/state exist; no decoder or analysis worker started.
    Prepared,
    /// The complete private source cache exists, but no analysis worker started.
    SourceCache,
    /// The analysis handle and its private source cache are both installed.
    AnalysisWorker,
}

/// Safe original panic diagnostic, independent of live startup ownership.
#[derive(Debug, Clone, thiserror::Error)]
#[error("Waveform startup panicked: {detail}; opaque_payload_abandoned={opaque_payload_abandoned}")]
pub struct AudioWaveformStartupPanic {
    /// Exact canonical Rust panic message, or an explicit opaque description.
    pub detail: String,
    /// An unknown payload was retained without invoking its destructor.
    pub opaque_payload_abandoned: bool,
}

/// Actual partial inventory and its unmodified consuming shutdown receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct AudioWaveformStartupShutdownEvidence {
    /// Installed inventory, captured before any shutdown operation.
    pub stage: AudioWaveformStartupStage,
    /// Unknown startup unwind owner was not proved released.
    pub opaque_panic_payload_abandoned: bool,
    /// Raw common owner receipt; absent source inventory remains default, not clean.
    pub owner: AudioWaveformShutdownEvidence,
}

impl AudioWaveformStartupShutdownEvidence {
    /// Clean closure of only the created inventory, never successful startup.
    pub fn all_created_resources_released(self) -> bool {
        let expected_workers = u32::from(self.stage == AudioWaveformStartupStage::AnalysisWorker);
        !self.opaque_panic_payload_abandoned
            && self.owner.analysis_resources_released(expected_workers)
            && self.owner.pending_requests_before == 0
            && self.owner.deferred_requests_before == 0
            && self.owner.running_requests_before == 0
            && self.owner.awaiting_publication_before == 0
            && match self.stage {
                AudioWaveformStartupStage::Prepared => {
                    self.owner.source_cache == AudioSourceCacheShutdownEvidence::default()
                }
                AudioWaveformStartupStage::SourceCache
                | AudioWaveformStartupStage::AnalysisWorker => {
                    self.owner.source_cache.all_resources_released()
                }
            }
    }
}

/// Owning startup failure. Do not erase this into an error before consuming it.
#[must_use = "retain and consume the actual failed Waveform startup owner"]
pub struct AudioWaveformStartupFailure {
    service: Arc<AudioWaveformService>,
    stage: AudioWaveformStartupStage,
    diagnostic: AudioWaveformStartupPanic,
}

impl fmt::Debug for AudioWaveformStartupFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AudioWaveformStartupFailure")
            .field("stage", &self.stage)
            .field("diagnostic", &self.diagnostic)
            .finish_non_exhaustive()
    }
}

impl AudioWaveformStartupFailure {
    /// Borrow the owner-free original diagnostic without releasing resources.
    pub fn diagnostic(&self) -> &AudioWaveformStartupPanic {
        &self.diagnostic
    }

    /// Signal all actually created owners without joining.
    pub fn begin_shutdown(&self) {
        self.service.begin_shutdown();
    }

    /// Consume the retained partial inventory against the original deadline.
    pub fn shutdown_until(self, deadline: Instant) -> AudioWaveformStartupShutdownEvidence {
        AudioWaveformStartupShutdownEvidence {
            stage: self.stage,
            opaque_panic_payload_abandoned: self.diagnostic.opaque_payload_abandoned,
            owner: self.service.shutdown_until(deadline),
        }
    }
}

struct StartupTransports {
    jobs: mpsc::Receiver<WaveformJob>,
    results: mpsc::SyncSender<WaveformResult>,
}

impl AudioWaveformService {
    /// Start the ordinary product owner. OS thread-start failure retains the
    /// existing degraded-service policy; a panic propagates after best-effort
    /// ordinary Drop. Use [`Self::try_start`] for consuming startup evidence.
    pub fn new() -> Arc<Self> {
        let (service, transports) = Self::prepare_unpublished();
        service.start_in_place(transports, |_, _| {}, spawn_analysis);
        service
    }

    /// Retain every created resource if initialization unwinds. A caught panic
    /// is a failed start even if subsequent partial-owner closure is clean.
    /// OS thread-start failure retains the same degraded policy as [`Self::new`].
    pub fn try_start() -> Result<Arc<Self>, AudioWaveformStartupFailure> {
        Self::try_start_with(|_, _| {}, spawn_analysis)
    }

    fn try_start_with(
        checkpoint: impl FnMut(AudioWaveformStartupStage, &Arc<Self>),
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<JoinHandle<()>>,
    ) -> Result<Arc<Self>, AudioWaveformStartupFailure> {
        // Allocation/state preparation precedes all worker creation. The actual
        // service remains outside the unwind region until activation returns it.
        let (service, transports) = Self::prepare_unpublished();
        match catch_unwind(AssertUnwindSafe(|| {
            service.start_in_place(transports, checkpoint, spawn);
        })) {
            Ok(()) => Ok(service),
            Err(payload) => {
                let stage = if service.shutdown.lock().workers_started != 0 {
                    AudioWaveformStartupStage::AnalysisWorker
                } else if service.source_cache.lock().is_some() {
                    AudioWaveformStartupStage::SourceCache
                } else {
                    AudioWaveformStartupStage::Prepared
                };
                let detail = payload
                    .downcast_ref::<&'static str>()
                    .map(|text| (*text).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "opaque startup panic payload".to_owned());
                let opaque_payload_abandoned =
                    dispose_canonical_or_abandon_opaque_panic_payload(payload);
                Err(AudioWaveformStartupFailure {
                    service,
                    stage,
                    diagnostic: AudioWaveformStartupPanic { detail, opaque_payload_abandoned },
                })
            }
        }
    }

    fn prepare_unpublished() -> (Arc<Self>, StartupTransports) {
        let (job_tx, job_rx) = mpsc::sync_channel(WAVEFORM_JOB_QUEUE_CAPACITY);
        let (result_tx, result_rx) = mpsc::sync_channel(WAVEFORM_JOB_QUEUE_CAPACITY + 1);
        let service = Arc::new(Self {
            resource_policy: Mutex::new(()),
            state: Mutex::new(WaveformState::default()),
            jobs: Mutex::new(Some(job_tx)),
            results: Mutex::new(result_rx),
            source_cache: Mutex::new(None),
            dispatch_gate: WaveformDispatchGate::new(),
            worker_activity: Arc::new(SingleWorkerActivity::default()),
            worker_terminal: Arc::new(AtomicU8::new(WAVEFORM_WORKER_TERMINAL_RUNNING)),
            shutdown: Mutex::new(WaveformShutdownControl {
                worker: None,
                boundary: None,
                receipt: None,
                workers_started: 0,
                worker_start_failures: 0,
                worker_owner_abandonments: 0,
            }),
        });
        (
            service,
            StartupTransports { jobs: job_rx, results: result_tx },
        )
    }

    fn start_in_place(
        self: &Arc<Self>,
        transports: StartupTransports,
        mut checkpoint: impl FnMut(AudioWaveformStartupStage, &Arc<Self>),
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<JoinHandle<()>>,
    ) {
        checkpoint(AudioWaveformStartupStage::Prepared, self);
        let (_, pcm_cache_byte_budget) =
            waveform_cache_partition(WAVEFORM_SOURCE_CACHE_BYTE_BUDGET);
        *self.source_cache.lock() = Some(Arc::new(AudioSourceCache::new_bounded_with_sessions(
            WAVEFORM_SAMPLE_RATE,
            WAVEFORM_DECODE_WINDOW_SECONDS,
            waveform_pcm_entry_capacity(pcm_cache_byte_budget),
            pcm_cache_byte_budget,
            1,
        )));
        checkpoint(AudioWaveformStartupStage::SourceCache, self);
        let worker_cache =
            self.source_cache.lock().as_ref().expect("installed source cache").clone();
        let dispatch = Arc::clone(&self.dispatch_gate);
        let activity = Arc::clone(&self.worker_activity);
        let terminal = Arc::clone(&self.worker_terminal);
        let worker = spawn(Box::new(move || {
            let result = catch_unwind(AssertUnwindSafe(|| {
                waveform_worker(
                    transports.jobs,
                    transports.results,
                    worker_cache,
                    dispatch,
                    activity,
                )
            }));
            let terminal_state = match result {
                Ok(WaveformWorkerExit::JobChannelClosed) => WAVEFORM_WORKER_TERMINAL_RETURNED,
                Ok(
                    WaveformWorkerExit::ResultTransportFull
                    | WaveformWorkerExit::ResultTransportDisconnected,
                ) => WAVEFORM_WORKER_TERMINAL_FAILED,
                Err(payload) => {
                    if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                        WAVEFORM_WORKER_TERMINAL_PANICKED_OWNER_ABANDONED
                    } else {
                        WAVEFORM_WORKER_TERMINAL_PANICKED
                    }
                }
            };
            terminal.store(terminal_state, Ordering::Release);
        }));
        match worker {
            Ok(worker) => {
                let mut shutdown = self.shutdown.lock();
                shutdown.worker = Some(worker);
                shutdown.workers_started = 1;
                drop(shutdown);
                checkpoint(AudioWaveformStartupStage::AnalysisWorker, self);
            }
            Err(error) => {
                let error_kind = error.kind();
                let owner_abandoned = u32::from(abandon_opaque_io_error(error));
                let mut shutdown = self.shutdown.lock();
                shutdown.worker_start_failures = 1;
                shutdown.worker_owner_abandonments = owner_abandoned;
                drop(shutdown);
                tracing::error!(?error_kind, "failed to start waveform analysis worker");
            }
        }
    }
}

fn spawn_analysis(work: Box<dyn FnOnce() + Send>) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("mondrian-waveform-analysis".to_owned())
        .spawn(work)
}

#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;
