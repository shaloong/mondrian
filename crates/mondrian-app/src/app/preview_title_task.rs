//! Bounded background generation for Preview Basic Title sources.
//!
//! The worker owns the font session and raster cache. Timeline execution sees
//! only exhaustive Ready/Pending/Unavailable outcomes, so font discovery,
//! shaping, and pixel allocation never run on the Window thread.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use mondrian_core::{EvaluatedBasicTitle, Resolution, WorkingColorSpace};
use mondrian_renderer::{
    basic_title_raster_request_key, BasicTitleRasterError, BasicTitleRasterFrame,
    BasicTitleRasterizer,
};

const TITLE_JOB_QUEUE_CAPACITY: usize = 2;
const TITLE_RESULT_CACHE_ENTRIES: usize = 64;
const TITLE_RESULT_CACHE_BYTES: usize = 128 * 1024 * 1024;
const TITLE_MAX_RESULTS_PER_POLL: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PreviewTitleRasterRequest {
    pub(crate) title: EvaluatedBasicTitle,
    pub(crate) author_resolution: Resolution,
    pub(crate) title_safe_margin: f32,
    pub(crate) sampled_resolution: Resolution,
    pub(crate) working_color_space: WorkingColorSpace,
}

impl PreviewTitleRasterRequest {
    pub(crate) fn key(&self) -> u64 {
        basic_title_raster_request_key(
            &self.title,
            self.author_resolution,
            self.title_safe_margin,
            self.sampled_resolution,
            self.working_color_space,
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) enum PreviewTitleRasterOutcome {
    Ready(BasicTitleRasterFrame),
    Pending,
    Unavailable(PreviewTitleRasterFailure),
}

#[derive(Debug, Clone)]
pub(crate) enum PreviewTitleRasterFailure {
    Raster(BasicTitleRasterError),
    Worker(String),
}

impl std::fmt::Display for PreviewTitleRasterFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Raster(error) => error.fmt(formatter),
            Self::Worker(detail) => formatter.write_str(detail),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreviewTitleTaskPoll {
    pub(crate) visible_change: bool,
    pub(crate) needs_follow_up_poll: bool,
}

enum PreviewTitleCacheState {
    Pending,
    Ready(BasicTitleRasterFrame),
    Failed(PreviewTitleRasterFailure),
}

struct PreviewTitleCacheEntry {
    request: PreviewTitleRasterRequest,
    state: PreviewTitleCacheState,
}

struct PreviewTitleRasterResult {
    key: u64,
    result: Result<BasicTitleRasterFrame, BasicTitleRasterError>,
}

pub(crate) struct PreviewTitleTask {
    jobs: Option<mpsc::SyncSender<PreviewTitleRasterRequest>>,
    worker_jobs: Option<mpsc::Receiver<PreviewTitleRasterRequest>>,
    results: mpsc::Receiver<PreviewTitleRasterResult>,
    result_sender: Option<mpsc::Sender<PreviewTitleRasterResult>>,
    worker: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    worker_start_failure: Option<String>,
    cache: HashMap<u64, PreviewTitleCacheEntry>,
    completed_order: VecDeque<u64>,
    completed_bytes: usize,
}

impl Default for PreviewTitleTask {
    fn default() -> Self {
        let (job_sender, job_receiver) = mpsc::sync_channel(TITLE_JOB_QUEUE_CAPACITY);
        let (result_sender, result_receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        Self {
            jobs: Some(job_sender),
            worker_jobs: Some(job_receiver),
            results: result_receiver,
            result_sender: Some(result_sender),
            worker: None,
            stop,
            worker_start_failure: None,
            cache: HashMap::new(),
            completed_order: VecDeque::new(),
            completed_bytes: 0,
        }
    }
}

impl PreviewTitleTask {
    pub(crate) fn resolve(
        &mut self,
        request: PreviewTitleRasterRequest,
    ) -> PreviewTitleRasterOutcome {
        self.poll_finished();
        let key = request.key();
        if let Some(entry) = self.cache.get(&key) {
            if entry.request != request {
                return PreviewTitleRasterOutcome::Unavailable(PreviewTitleRasterFailure::Worker(
                    "Basic Title request identity collision; generation refused".to_owned(),
                ));
            }
            return match &entry.state {
                PreviewTitleCacheState::Pending => PreviewTitleRasterOutcome::Pending,
                PreviewTitleCacheState::Ready(frame) => {
                    PreviewTitleRasterOutcome::Ready(frame.clone())
                }
                PreviewTitleCacheState::Failed(error) => {
                    PreviewTitleRasterOutcome::Unavailable(error.clone())
                }
            };
        }

        if let Err(error) = self.ensure_worker() {
            return PreviewTitleRasterOutcome::Unavailable(PreviewTitleRasterFailure::Worker(
                error,
            ));
        }
        let Some(jobs) = self.jobs.as_ref() else {
            return PreviewTitleRasterOutcome::Unavailable(PreviewTitleRasterFailure::Worker(
                "Basic Title worker is unavailable".to_owned(),
            ));
        };
        match jobs.try_send(request.clone()) {
            Ok(()) => {
                self.cache.insert(
                    key,
                    PreviewTitleCacheEntry { request, state: PreviewTitleCacheState::Pending },
                );
                PreviewTitleRasterOutcome::Pending
            }
            Err(mpsc::TrySendError::Full(_)) => PreviewTitleRasterOutcome::Pending,
            Err(mpsc::TrySendError::Disconnected(_)) => {
                PreviewTitleRasterOutcome::Unavailable(PreviewTitleRasterFailure::Worker(
                    "Basic Title worker queue disconnected".to_owned(),
                ))
            }
        }
    }

    pub(crate) fn poll_finished(&mut self) -> PreviewTitleTaskPoll {
        let mut poll = PreviewTitleTaskPoll::default();
        let mut worker_disconnected = false;
        for _ in 0..TITLE_MAX_RESULTS_PER_POLL {
            let result = match self.results.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    worker_disconnected = true;
                    break;
                }
            };
            let Some(entry) = self.cache.get_mut(&result.key) else {
                continue;
            };
            entry.state = match result.result {
                Ok(frame) => {
                    self.completed_bytes =
                        self.completed_bytes.saturating_add(frame.retained_bytes());
                    PreviewTitleCacheState::Ready(frame)
                }
                Err(error) => {
                    PreviewTitleCacheState::Failed(PreviewTitleRasterFailure::Raster(error))
                }
            };
            self.completed_order.retain(|key| *key != result.key);
            self.completed_order.push_back(result.key);
            poll.visible_change = true;
        }
        if worker_disconnected {
            let pending = self
                .cache
                .iter()
                .filter_map(|(key, entry)| {
                    matches!(entry.state, PreviewTitleCacheState::Pending).then_some(*key)
                })
                .collect::<Vec<_>>();
            for key in pending {
                let Some(entry) = self.cache.get_mut(&key) else {
                    continue;
                };
                entry.state = PreviewTitleCacheState::Failed(PreviewTitleRasterFailure::Worker(
                    "Basic Title worker terminated before publishing a result".to_owned(),
                ));
                self.completed_order.retain(|cached| *cached != key);
                self.completed_order.push_back(key);
                poll.visible_change = true;
            }
        }
        self.trim_completed_cache();
        poll.needs_follow_up_poll = self
            .cache
            .values()
            .any(|entry| matches!(entry.state, PreviewTitleCacheState::Pending));
        poll
    }

    fn ensure_worker(&mut self) -> Result<(), String> {
        if let Some(error) = &self.worker_start_failure {
            return Err(error.clone());
        }
        if self.worker.is_some() {
            return Ok(());
        }
        let jobs = self.worker_jobs.take().ok_or_else(|| {
            "Basic Title worker receiver is unavailable before startup".to_owned()
        })?;
        let results = self
            .result_sender
            .take()
            .ok_or_else(|| "Basic Title worker result channel is unavailable".to_owned())?;
        let stop = Arc::clone(&self.stop);
        match thread::Builder::new()
            .name("mondrian-preview-title".to_owned())
            .spawn(move || preview_title_worker(jobs, results, stop))
        {
            Ok(worker) => {
                self.worker = Some(worker);
                Ok(())
            }
            Err(error) => {
                let detail = format!("failed to start Basic Title worker: {error}");
                self.worker_start_failure = Some(detail.clone());
                Err(detail)
            }
        }
    }

    fn trim_completed_cache(&mut self) {
        while self.completed_order.len() > TITLE_RESULT_CACHE_ENTRIES
            || (self.completed_bytes > TITLE_RESULT_CACHE_BYTES && self.completed_order.len() > 1)
        {
            let Some(key) = self.completed_order.pop_front() else {
                break;
            };
            if self
                .cache
                .get(&key)
                .is_some_and(|entry| !matches!(entry.state, PreviewTitleCacheState::Pending))
            {
                if let Some(entry) = self.cache.remove(&key) {
                    if let PreviewTitleCacheState::Ready(frame) = entry.state {
                        self.completed_bytes =
                            self.completed_bytes.saturating_sub(frame.retained_bytes());
                    }
                }
            }
        }
    }
}

impl Drop for PreviewTitleTask {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.jobs.take();
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                tracing::warn!("Basic Title Preview worker panicked during shutdown");
            }
        }
    }
}

fn preview_title_worker(
    jobs: mpsc::Receiver<PreviewTitleRasterRequest>,
    results: mpsc::Sender<PreviewTitleRasterResult>,
    stop: Arc<AtomicBool>,
) {
    let mut rasterizer = BasicTitleRasterizer::new();
    while let Ok(request) = jobs.recv() {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let key = request.key();
        let result = rasterizer.rasterize(
            &request.title,
            request.author_resolution,
            request.title_safe_margin,
            request.sampled_resolution,
            request.working_color_space,
        );
        if stop.load(Ordering::Acquire) {
            break;
        }
        if results.send(PreviewTitleRasterResult { key, result }).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> PreviewTitleRasterRequest {
        PreviewTitleRasterRequest {
            title: mondrian_core::BasicTitle::new(
                "Mondrian",
                mondrian_core::default_basic_title_font_family(),
            )
            .expect("title")
            .evaluate(mondrian_core::TimelineTime::ZERO)
            .expect("evaluated title"),
            author_resolution: Resolution::FHD,
            title_safe_margin: 0.20,
            sampled_resolution: Resolution::HD,
            working_color_space: WorkingColorSpace::LinearRec709,
        }
    }

    #[test]
    fn disconnected_worker_converts_pending_requests_to_terminal_failure() {
        let mut task = PreviewTitleTask::default();
        let request = request();
        let key = request.key();
        task.cache.insert(
            key,
            PreviewTitleCacheEntry {
                request: request.clone(),
                state: PreviewTitleCacheState::Pending,
            },
        );
        task.result_sender.take();

        let poll = task.poll_finished();

        assert!(poll.visible_change);
        assert!(!poll.needs_follow_up_poll);
        assert!(matches!(
            task.resolve(request),
            PreviewTitleRasterOutcome::Unavailable(PreviewTitleRasterFailure::Worker(_))
        ));
    }
}
