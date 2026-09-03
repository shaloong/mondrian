//! Preview Adapter over the persistent Timeline render-cache service.
//!
//! The domain service owns filesystem, compression, verification, publication,
//! queueing, and disk pressure. This shallow Adapter retains only one ready
//! working frame for immediate GPU/CPU promotion plus a bounded negative set
//! that prevents a UI poll loop from resubmitting known misses.

use std::collections::VecDeque;
#[cfg(not(test))]
use std::path::PathBuf;
#[cfg(not(test))]
use std::sync::Arc;
use std::time::Instant;

use mondrian_core::WorkingColorSpace;
use mondrian_render_cache::{
    TimelineRenderCacheConfig, TimelineRenderCacheDiagnostics, TimelineRenderCacheFrame,
    TimelineRenderCacheIdentity, TimelineRenderCacheLookup, TimelineRenderCacheResult,
    TimelineRenderCacheService, TimelineRenderCacheShutdownEvidence, TimelineRenderCacheSubmission,
};
use mondrian_renderer::CpuColorFrame;

use super::preview_runtime::PreviewOwnedWorkerShutdown;
use super::preview_work_notification::PreviewWorkNotifier;

const NEGATIVE_IDENTITY_CAPACITY: usize = 256;
#[cfg(not(test))]
const DEFAULT_DISK_BUDGET_BYTES: u64 = 20 * 1024 * 1024 * 1024;
#[cfg(not(test))]
const DEFAULT_ARTIFACT_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_QUEUE_CAPACITY: usize = 4;

pub(crate) struct PreviewTimelineRenderCache {
    service: Option<TimelineRenderCacheService>,
    required: bool,
    start_failure: Option<String>,
    ready: Option<(TimelineRenderCacheIdentity, CpuColorFrame)>,
    negative: VecDeque<TimelineRenderCacheIdentity>,
}

impl PreviewTimelineRenderCache {
    pub(crate) fn start(work_notifier: PreviewWorkNotifier) -> Self {
        #[cfg(test)]
        {
            let _ = work_notifier;
            Self {
                service: None,
                required: false,
                start_failure: None,
                ready: None,
                negative: VecDeque::new(),
            }
        }
        #[cfg(not(test))]
        {
            let config = default_config();
            let notifier = Arc::new(move || {
                work_notifier.result_became_pollable();
            });
            match config.and_then(|config| {
                TimelineRenderCacheService::start_with_notifier(config, notifier)
                    .map_err(|error| error.to_string())
            }) {
                Ok(service) => Self {
                    service: Some(service),
                    required: true,
                    start_failure: None,
                    ready: None,
                    negative: VecDeque::new(),
                },
                Err(error) => {
                    tracing::warn!(%error, "Timeline render cache is unavailable");
                    Self {
                        service: None,
                        required: true,
                        start_failure: Some(error),
                        ready: None,
                        negative: VecDeque::new(),
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn with_service(service: TimelineRenderCacheService) -> Self {
        Self {
            service: Some(service),
            required: true,
            start_failure: None,
            ready: None,
            negative: VecDeque::new(),
        }
    }

    pub(crate) fn request_lookup(
        &self,
        identity: TimelineRenderCacheIdentity,
        color_space: WorkingColorSpace,
    ) -> TimelineRenderCacheSubmission {
        if self.ready.as_ref().is_some_and(|(ready, _)| *ready == identity)
            || self.negative.contains(&identity)
        {
            return TimelineRenderCacheSubmission::AlreadyPending;
        }
        self.service
            .as_ref()
            .map_or(TimelineRenderCacheSubmission::Disconnected, |service| {
                service.lookup(identity, color_space)
            })
    }

    pub(crate) fn ready_frame(
        &self,
        identity: TimelineRenderCacheIdentity,
    ) -> Option<CpuColorFrame> {
        self.ready
            .as_ref()
            .filter(|(ready, _)| *ready == identity)
            .map(|(_, frame)| frame.clone())
    }

    pub(crate) fn publish(&mut self, frame: TimelineRenderCacheFrame) {
        let identity = frame.identity();
        self.remove_negative(identity);
        if let Some(service) = &self.service {
            let _ = service.publish(frame);
        }
    }

    /// Pump a bounded number of terminals. Returns whether a verified hit
    /// became available and presentation should retry.
    pub(crate) fn pump(&mut self) -> bool {
        if self.service.is_none() {
            return false;
        }
        let mut ready_changed = false;
        for _ in 0..DEFAULT_QUEUE_CAPACITY.saturating_add(1) {
            let result = self.service.as_ref().and_then(TimelineRenderCacheService::try_poll);
            let Some(result) = result else {
                break;
            };
            match result {
                TimelineRenderCacheResult::Lookup { identity, result } => match result {
                    TimelineRenderCacheLookup::Hit(frame) => {
                        self.remove_negative(identity);
                        self.ready = Some((identity, CpuColorFrame::working(frame.into_frame())));
                        ready_changed = true;
                    }
                    TimelineRenderCacheLookup::Miss => self.remember_negative(identity),
                    TimelineRenderCacheLookup::CorruptRemoved { detail } => {
                        tracing::warn!(%identity, %detail, "removed invalid Timeline render-cache artifact");
                        self.remember_negative(identity);
                    }
                },
                TimelineRenderCacheResult::Published { identity, .. } => {
                    self.remove_negative(identity);
                }
                TimelineRenderCacheResult::Failed { identity, detail } => {
                    tracing::debug!(%identity, %detail, "optional Timeline render-cache work failed");
                    self.remember_negative(identity);
                }
            }
        }
        ready_changed
    }

    pub(crate) fn diagnostics(&self) -> TimelineRenderCacheDiagnostics {
        self.service
            .as_ref()
            .map_or_else(TimelineRenderCacheDiagnostics::default, |service| {
                service.diagnostics()
            })
    }

    pub(crate) fn start_failure(&self) -> Option<&str> {
        self.start_failure.as_deref()
    }

    pub(crate) fn shutdown_and_wait(&mut self) -> PreviewTimelineRenderCacheShutdownEvidence {
        let Some(service) = self.service.take() else {
            return self.no_worker_shutdown_evidence();
        };
        let evidence = service.shutdown_and_wait();
        self.shutdown_evidence(evidence)
    }

    fn worker_outcome(evidence: TimelineRenderCacheShutdownEvidence) -> PreviewOwnedWorkerShutdown {
        if evidence.current_thread_skipped {
            PreviewOwnedWorkerShutdown::CurrentThreadSkipped
        } else if evidence.worker_panicked {
            PreviewOwnedWorkerShutdown::Panicked
        } else if evidence.worker_started && evidence.worker_terminated {
            PreviewOwnedWorkerShutdown::Terminated
        } else {
            PreviewOwnedWorkerShutdown::NotStarted
        }
    }

    pub(crate) fn begin_shutdown(&mut self) {
        if let Some(service) = self.service.as_mut() {
            service.begin_shutdown();
        }
    }

    pub(crate) fn shutdown_until(
        &mut self,
        deadline: Instant,
    ) -> PreviewTimelineRenderCacheShutdownEvidence {
        let Some(service) = self.service.take() else {
            return self.no_worker_shutdown_evidence();
        };
        let evidence = service.shutdown_until(deadline);
        self.shutdown_evidence(evidence)
    }

    fn shutdown_evidence(
        &self,
        worker: TimelineRenderCacheShutdownEvidence,
    ) -> PreviewTimelineRenderCacheShutdownEvidence {
        PreviewTimelineRenderCacheShutdownEvidence {
            schema_version: 1,
            required: self.required,
            start_failed: self.start_failure.is_some(),
            worker: Some(worker),
            aggregate_outcome: Self::worker_outcome(worker),
        }
    }

    fn no_worker_shutdown_evidence(&self) -> PreviewTimelineRenderCacheShutdownEvidence {
        PreviewTimelineRenderCacheShutdownEvidence {
            schema_version: 1,
            required: self.required,
            start_failed: self.start_failure.is_some(),
            worker: None,
            aggregate_outcome: PreviewOwnedWorkerShutdown::NotStarted,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_start_failure_for_test(detail: impl Into<String>) -> Self {
        Self {
            service: None,
            required: true,
            start_failure: Some(detail.into()),
            ready: None,
            negative: VecDeque::new(),
        }
    }

    fn remember_negative(&mut self, identity: TimelineRenderCacheIdentity) {
        self.remove_negative(identity);
        self.negative.push_back(identity);
        while self.negative.len() > NEGATIVE_IDENTITY_CAPACITY {
            self.negative.pop_front();
        }
    }

    fn remove_negative(&mut self, identity: TimelineRenderCacheIdentity) {
        self.negative.retain(|candidate| *candidate != identity);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewTimelineRenderCacheShutdownEvidence {
    pub(crate) schema_version: u32,
    pub(crate) required: bool,
    pub(crate) start_failed: bool,
    pub(crate) worker: Option<TimelineRenderCacheShutdownEvidence>,
    pub(crate) aggregate_outcome: PreviewOwnedWorkerShutdown,
}

impl PreviewTimelineRenderCacheShutdownEvidence {
    pub(crate) const fn all_resources_released(self) -> bool {
        self.schema_version == 1
            && (!self.required
                || (!self.start_failed
                    && matches!(self.worker, Some(worker) if worker.all_workers_terminated())))
    }
}

impl Default for PreviewTimelineRenderCacheShutdownEvidence {
    fn default() -> Self {
        Self {
            schema_version: 0,
            required: true,
            start_failed: true,
            worker: None,
            aggregate_outcome: PreviewOwnedWorkerShutdown::NotStarted,
        }
    }
}

#[cfg(not(test))]
fn default_config() -> Result<TimelineRenderCacheConfig, String> {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| std::env::current_dir().map_err(|error| error.to_string()))?;
    let root = std::path::absolute(base.join("Mondrian").join("cache"))
        .map_err(|error| error.to_string())?;
    TimelineRenderCacheConfig::new(
        root,
        DEFAULT_DISK_BUDGET_BYTES,
        DEFAULT_ARTIFACT_BUDGET_BYTES,
        DEFAULT_QUEUE_CAPACITY,
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{WorkingColorSpace, WorkingRgbaF32Frame};
    use mondrian_render_cache::{TimelineRenderCacheAlpha, TimelineRenderCacheFormat};
    use std::time::{Duration, Instant};

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "render-cache Adapter timed out");
            std::thread::yield_now();
        }
    }

    #[test]
    fn verified_hit_is_promoted_as_one_bounded_working_frame() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = TimelineRenderCacheService::start(
            TimelineRenderCacheConfig::new(temp.path().to_path_buf(), 1_048_576, 1_048_576, 2)
                .expect("config"),
        )
        .expect("service");
        let mut adapter = PreviewTimelineRenderCache::with_service(service);
        let materialization =
            mondrian_renderer::ResolvedVisualNodeMaterializationIdentity::from_canonical_bytes(
                b"preview-render-cache-adapter-test",
            );
        let identity = TimelineRenderCacheIdentity::for_resolved_visual(
            mondrian_renderer::ResolvedVisualFrameIdentity::from_materialization(materialization),
            2,
            1,
            TimelineRenderCacheFormat::LosslessRgba32FloatZstd,
            TimelineRenderCacheAlpha::StraightCoverage,
        );
        let frame = TimelineRenderCacheFrame::new(
            identity,
            WorkingRgbaF32Frame {
                width: 2,
                height: 1,
                data: vec![[0.25, 0.5, 1.0, 1.0]; 2],
                color_space: WorkingColorSpace::LinearRec709,
            },
        )
        .expect("frame");
        adapter.publish(frame);
        wait_until(|| {
            adapter.pump();
            adapter.diagnostics().publications == 1
        });
        assert_eq!(
            adapter.request_lookup(identity, WorkingColorSpace::LinearRec709),
            TimelineRenderCacheSubmission::Scheduled
        );
        wait_until(|| adapter.pump());
        let ready = adapter.ready_frame(identity).expect("verified working hit");
        assert_eq!(ready.rgba_f32().data[0], [0.25, 0.5, 1.0, 1.0]);
    }

    #[test]
    fn shutdown_retains_exact_required_cache_worker_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = TimelineRenderCacheService::start(
            TimelineRenderCacheConfig::new(temp.path().to_path_buf(), 1_048_576, 1_048_576, 2)
                .expect("config"),
        )
        .expect("service");
        let mut adapter = PreviewTimelineRenderCache::with_service(service);

        let evidence = adapter.shutdown_until(Instant::now() + Duration::from_secs(1));

        assert!(evidence.required);
        assert!(!evidence.start_failed);
        assert!(evidence.worker.expect("exact cache worker evidence").all_workers_terminated());
        assert_eq!(
            evidence.aggregate_outcome,
            PreviewOwnedWorkerShutdown::Terminated
        );
        assert!(evidence.all_resources_released());
    }

    #[test]
    fn required_cache_start_failure_cannot_be_reported_as_never_started_cleanly() {
        let mut adapter =
            PreviewTimelineRenderCache::with_start_failure_for_test("intentional start failure");

        let evidence = adapter.shutdown_until(Instant::now() + Duration::from_secs(1));

        assert!(evidence.required);
        assert!(evidence.start_failed);
        assert!(evidence.worker.is_none());
        assert_eq!(
            evidence.aggregate_outcome,
            PreviewOwnedWorkerShutdown::NotStarted
        );
        assert!(!evidence.all_resources_released());
    }
}
