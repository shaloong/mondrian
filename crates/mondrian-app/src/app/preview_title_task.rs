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
    basic_title_raster_request_identity, BasicTitleRasterError, BasicTitleRasterFrame,
    BasicTitleRasterRequestIdentity, BasicTitleRasterizer,
};

use super::preview_work_notification::PreviewWorkNotifier;

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
    pub(crate) fn identity(&self) -> BasicTitleRasterRequestIdentity {
        basic_title_raster_request_identity(
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
    /// A retained title identity became Ready or Failed.
    ///
    /// The Runtime must combine this with current candidate-pending authority;
    /// a late cache fill is not by itself permission to repaint the Window.
    pub(crate) candidate_progress: bool,
    pub(crate) needs_follow_up_poll: bool,
}

impl PreviewTitleTaskPoll {
    /// Convert cache progress into a retry edge only while a Viewer candidate
    /// is actually waiting on asynchronous inputs.
    pub(crate) const fn candidate_retry_required(self, candidate_pending: bool) -> bool {
        self.candidate_progress && candidate_pending
    }
}

enum PreviewTitleCacheState {
    Pending,
    Ready(BasicTitleRasterFrame),
    Failed(PreviewTitleRasterFailure),
}

struct PreviewTitleCacheEntry {
    state: PreviewTitleCacheState,
}

struct PreviewTitleRasterResult {
    identity: BasicTitleRasterRequestIdentity,
    result: Result<BasicTitleRasterFrame, BasicTitleRasterError>,
}

pub(crate) struct PreviewTitleTask {
    jobs: Option<mpsc::SyncSender<PreviewTitleRasterRequest>>,
    worker_jobs: Option<mpsc::Receiver<PreviewTitleRasterRequest>>,
    results: mpsc::Receiver<PreviewTitleRasterResult>,
    result_sender: Option<mpsc::Sender<PreviewTitleRasterResult>>,
    work_notifier: PreviewWorkNotifier,
    worker: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    worker_start_failure: Option<String>,
    cache: HashMap<BasicTitleRasterRequestIdentity, PreviewTitleCacheEntry>,
    completed_order: VecDeque<BasicTitleRasterRequestIdentity>,
    completed_bytes: usize,
    cache_byte_budget: usize,
}

impl Default for PreviewTitleTask {
    fn default() -> Self {
        Self::with_notifier(PreviewWorkNotifier::default())
    }
}

impl PreviewTitleTask {
    /// Create a title task publishing into the Runtime's shared work watch.
    pub(crate) fn with_notifier(work_notifier: PreviewWorkNotifier) -> Self {
        let (job_sender, job_receiver) = mpsc::sync_channel(TITLE_JOB_QUEUE_CAPACITY);
        let (result_sender, result_receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        Self {
            jobs: Some(job_sender),
            worker_jobs: Some(job_receiver),
            results: result_receiver,
            result_sender: Some(result_sender),
            work_notifier,
            worker: None,
            stop,
            worker_start_failure: None,
            cache: HashMap::new(),
            completed_order: VecDeque::new(),
            completed_bytes: 0,
            cache_byte_budget: TITLE_RESULT_CACHE_BYTES,
        }
    }

    /// Apply the product-owned retained-result budget online.
    pub(crate) fn set_cache_byte_budget(&mut self, cache_byte_budget: usize) {
        let cache_byte_budget = cache_byte_budget.max(1);
        if self.cache_byte_budget == cache_byte_budget {
            return;
        }
        self.cache_byte_budget = cache_byte_budget;
        self.trim_completed_cache();
    }

    pub(crate) fn resolve(
        &mut self,
        request: PreviewTitleRasterRequest,
    ) -> PreviewTitleRasterOutcome {
        self.poll_finished();
        let identity = request.identity();
        if let Some(entry) = self.cache.get(&identity) {
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
        match jobs.try_send(request) {
            Ok(()) => {
                self.cache.insert(
                    identity,
                    PreviewTitleCacheEntry { state: PreviewTitleCacheState::Pending },
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
        let mut drained = 0usize;
        for _ in 0..TITLE_MAX_RESULTS_PER_POLL {
            let result = match self.results.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    worker_disconnected = true;
                    break;
                }
            };
            drained = drained.saturating_add(1);
            let Some(entry) = self.cache.get_mut(&result.identity) else {
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
            self.completed_order.retain(|identity| *identity != result.identity);
            self.completed_order.push_back(result.identity);
            poll.candidate_progress = true;
        }
        if worker_disconnected {
            let pending = self
                .cache
                .iter()
                .filter_map(|(identity, entry)| {
                    matches!(entry.state, PreviewTitleCacheState::Pending).then_some(*identity)
                })
                .collect::<Vec<_>>();
            for identity in pending {
                let Some(entry) = self.cache.get_mut(&identity) else {
                    continue;
                };
                entry.state = PreviewTitleCacheState::Failed(PreviewTitleRasterFailure::Worker(
                    "Basic Title worker terminated before publishing a result".to_owned(),
                ));
                self.completed_order.retain(|cached| *cached != identity);
                self.completed_order.push_back(identity);
                poll.candidate_progress = true;
            }
        }
        self.trim_completed_cache();
        // Pending means a worker owns asynchronous work and will publish a
        // work-watch edge. Only exhausting this bounded drain is evidence that
        // another result may already be immediately readable.
        poll.needs_follow_up_poll = drained == TITLE_MAX_RESULTS_PER_POLL;
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
        let work_notifier = self.work_notifier.clone();
        match thread::Builder::new()
            .name("mondrian-preview-title".to_owned())
            .spawn(move || preview_title_worker(jobs, results, work_notifier, stop))
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
            || (self.completed_bytes > self.cache_byte_budget && self.completed_order.len() > 1)
        {
            let Some(identity) = self.completed_order.pop_front() else {
                break;
            };
            if self
                .cache
                .get(&identity)
                .is_some_and(|entry| !matches!(entry.state, PreviewTitleCacheState::Pending))
                && let Some(entry) = self.cache.remove(&identity)
                && let PreviewTitleCacheState::Ready(frame) = entry.state
            {
                self.completed_bytes = self.completed_bytes.saturating_sub(frame.retained_bytes());
            }
        }
    }
}

impl Drop for PreviewTitleTask {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.jobs.take();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("Basic Title Preview worker panicked during shutdown");
        }
    }
}

fn preview_title_worker(
    jobs: mpsc::Receiver<PreviewTitleRasterRequest>,
    results: mpsc::Sender<PreviewTitleRasterResult>,
    work_notifier: PreviewWorkNotifier,
    stop: Arc<AtomicBool>,
) {
    let _exit_notification = work_notifier.worker_exit_notification();
    // The task's result cache already owns the exact same frames. A second
    // rasterizer cache would duplicate up to 128 MiB without adding reuse.
    let mut rasterizer = BasicTitleRasterizer::with_cache_budget(1, 0);
    while let Ok(request) = jobs.recv() {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let identity = request.identity();
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
        if !publish_title_result(
            &results,
            &work_notifier,
            PreviewTitleRasterResult { identity, result },
        ) {
            break;
        }
    }
}

fn publish_title_result(
    results: &mpsc::Sender<PreviewTitleRasterResult>,
    work_notifier: &PreviewWorkNotifier,
    result: PreviewTitleRasterResult,
) -> bool {
    if results.send(result).is_err() {
        return false;
    }
    work_notifier.result_became_pollable();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::preview_timeline_execution::PreviewTimelineTitleRequest;

    fn request() -> PreviewTitleRasterRequest {
        request_with_text("Mondrian")
    }

    fn request_with_text(text: &str) -> PreviewTitleRasterRequest {
        PreviewTitleRasterRequest {
            title: mondrian_core::BasicTitle::new(
                text,
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
    fn pending_title_without_a_result_does_not_request_busy_polling() {
        let mut task = PreviewTitleTask::default();
        let identity = request().identity();
        task.cache.insert(
            identity,
            PreviewTitleCacheEntry { state: PreviewTitleCacheState::Pending },
        );

        assert_eq!(task.poll_finished(), PreviewTitleTaskPoll::default());
    }

    #[test]
    fn bounded_title_result_burst_requests_follow_up_without_a_new_revision() {
        let work_notifier = PreviewWorkNotifier::default();
        let work_watch = work_notifier.watch();
        let mut task = PreviewTitleTask::with_notifier(work_notifier);
        for index in 0..=TITLE_MAX_RESULTS_PER_POLL {
            let request = request_with_text(&format!("Title {index}"));
            let identity = request.identity();
            task.cache.insert(
                identity,
                PreviewTitleCacheEntry { state: PreviewTitleCacheState::Pending },
            );
            task.result_sender
                .as_ref()
                .expect("test result sender")
                .send(PreviewTitleRasterResult {
                    identity,
                    result: Err(BasicTitleRasterError::InvalidCanvas {
                        author_width: 0,
                        author_height: 0,
                        sampled_width: 0,
                        sampled_height: 0,
                    }),
                })
                .expect("inject title result");
            task.work_notifier.result_became_pollable();
        }
        let published_revision = work_watch.revision();

        let first = task.poll_finished();
        assert!(first.candidate_progress);
        assert!(first.needs_follow_up_poll);
        assert_eq!(work_watch.revision(), published_revision);

        let second = task.poll_finished();
        assert!(second.candidate_progress);
        assert!(!second.needs_follow_up_poll);
        assert_eq!(work_watch.revision(), published_revision);
    }

    #[test]
    fn disconnected_worker_converts_pending_requests_to_terminal_failure() {
        let mut task = PreviewTitleTask::default();
        let request = request();
        let identity = request.identity();
        task.cache.insert(
            identity,
            PreviewTitleCacheEntry { state: PreviewTitleCacheState::Pending },
        );
        task.result_sender.take();

        let poll = task.poll_finished();

        assert!(poll.candidate_progress);
        assert!(!poll.needs_follow_up_poll);
        assert!(matches!(
            task.resolve(request),
            PreviewTitleRasterOutcome::Unavailable(PreviewTitleRasterFailure::Worker(_))
        ));
    }

    #[test]
    fn late_cache_fill_retries_only_a_candidate_that_is_still_pending() {
        let progress = PreviewTitleTaskPoll {
            candidate_progress: true,
            needs_follow_up_poll: false,
        };

        assert!(!progress.candidate_retry_required(false));
        assert!(progress.candidate_retry_required(true));
    }

    #[test]
    fn timeline_and_background_title_requests_share_one_complete_identity() {
        let request = request();
        let timeline_request = PreviewTimelineTitleRequest {
            title: request.title.clone(),
            author_resolution: request.author_resolution,
            title_safe_margin: request.title_safe_margin,
            target_resolution: request.sampled_resolution,
            working_color_space: request.working_color_space,
        };

        assert_eq!(timeline_request.identity(), request.identity());
    }

    #[test]
    fn title_result_advances_shared_work_watch_after_publication() {
        let work_notifier = PreviewWorkNotifier::default();
        let work_watch = work_notifier.watch();
        let before = work_watch.revision();
        let (result_sender, results) = mpsc::channel();
        let identity = request().identity();
        assert!(publish_title_result(
            &result_sender,
            &work_notifier,
            PreviewTitleRasterResult {
                identity,
                result: Err(BasicTitleRasterError::InvalidCanvas {
                    author_width: 0,
                    author_height: 0,
                    sampled_width: 0,
                    sampled_height: 0,
                }),
            },
        ));
        assert_eq!(
            results.try_recv().expect("published title result").identity,
            identity
        );
        assert_ne!(work_watch.revision(), before);
    }

    #[test]
    fn product_cache_budget_is_applied_online_and_idempotently() {
        let mut task = PreviewTitleTask::default();
        task.set_cache_byte_budget(8 * 1024 * 1024);
        assert_eq!(task.cache_byte_budget, 8 * 1024 * 1024);

        task.set_cache_byte_budget(8 * 1024 * 1024);
        assert_eq!(task.cache_byte_budget, 8 * 1024 * 1024);
    }
}
