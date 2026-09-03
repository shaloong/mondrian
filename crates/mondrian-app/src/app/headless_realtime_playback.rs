//! Shared validation/test coordinator for production Headless realtime playback.
//!
//! This module is the sole interpreter for Headless current-candidate,
//! queue-publication, callback-retirement, successor, and bounded-wait
//! ordering. Qualification and performance producers may observe executions;
//! they must not implement a second coordinator.

// The remaining COL-047 concrete runtime will consume the interval methods in
// non-test validation builds; keep that future wiring compilable without
// widening this low-level module's visibility in the interim.
#![cfg_attr(not(test), allow(dead_code))]

use std::time::{Duration, Instant};

use anyhow::Context;
use serde::Serialize;

#[cfg(all(test, feature = "validation"))]
use super::audio_playback_acceptance::ProfessionalVideoCoordinatorObservation;
use super::endurance_shutdown::{AppBackgroundDomainSnapshot, AppBackgroundEnduranceSnapshot};
use super::headless_preview_presentation::{
    prepare_headless_preview_successor, present_headless_preview_candidate,
    present_headless_preview_candidate_at, stage_headless_preview_lookahead,
    HeadlessCompletedGpuDisposition, HeadlessPresentedOutput, HeadlessPreviewCandidate,
    HeadlessPreviewRuntime,
};
use super::headless_viewer_gpu::{
    HeadlessGpuCompletionDeadline, HeadlessViewerGpuAdapter, HeadlessViewerGpuEnduranceSnapshot,
    HeadlessViewerGpuExecution,
};
use super::native_video_import::resolve_playback_hardware_decode_admission;
use super::playback_preview::{pump_playback_preview, PlaybackPreviewPumpOutcome};
use super::preview_work_notification::{PreviewWorkRevision, PreviewWorkWatch};
use super::AppState;
use mondrian_platform::{PlaybackThreadScheduling, PlaybackThreadSchedulingStatus};

pub(crate) const HEADLESS_PREVIEW_CLOCK_TICK_MAX_WAIT: Duration = Duration::from_millis(1);

/// Observer seam for product-neutral Headless GPU execution facts.
///
/// Callbacks execute inside the realtime coordinator. Implementations must be
/// infallible, nonblocking, bounded O(1) fact collection and must never mutate
/// execution order or perform I/O.
pub(crate) trait HeadlessGpuExecutionObserver {
    fn execution_completed(
        &mut self,
        execution: HeadlessViewerGpuExecution,
        disposition: HeadlessGpuExecutionDisposition,
        completed_demand: Option<mondrian_playback::FrameDemandIdentity>,
    );

    fn current_output_presented(
        &mut self,
        completed_demand: Option<mondrian_playback::FrameDemandIdentity>,
    );

    fn successor_preparation(&mut self, ready: bool);
}

/// Physical disposition of one completed Headless GPU execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadlessGpuExecutionDisposition {
    PublishedCurrent,
    PreparedSuccessor,
    Released,
    TerminalRejected(mondrian_playback::FrameDeliveryKind),
}

/// Readiness sample for one exact pre-advance consumer intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeadlessPreviewSample {
    pub(crate) current_gpu_ready: bool,
    pub(crate) stale_output_available: bool,
    pub(crate) unavailable: bool,
}

/// Result of advancing one exact realtime playback interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadlessRealtimeIntervalOutcome {
    Advanced {
        epoch: mondrian_playback::PlaybackEpoch,
        frame: i64,
        sample: HeadlessPreviewSample,
    },
    NaturalEnd {
        terminal_frame: i64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadlessGpuCandidateStatus {
    Ready,
    QueuedReady,
    InFlight,
    Loading,
    Backpressured,
    DroppedLate,
    Unavailable,
}

impl HeadlessGpuCandidateStatus {
    pub(crate) fn requires_bounded_wait(self) -> bool {
        matches!(
            self,
            Self::QueuedReady | Self::InFlight | Self::Backpressured | Self::Unavailable
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeadlessGpuCandidateIntent {
    pub(crate) epoch: mondrian_playback::PlaybackEpoch,
    pub(crate) quality_revision: u64,
    pub(crate) frame: i64,
    pub(crate) pending_demand: Option<mondrian_playback::FrameDemandIdentity>,
}

impl HeadlessGpuCandidateIntent {
    pub(crate) fn from_state(state: &AppState) -> Self {
        let playback = state.playback_engine.snapshot();
        Self {
            epoch: playback.epoch,
            quality_revision: playback.quality_revision,
            frame: playback.position.frame,
            pending_demand: state.pending_playback_frame_demand_identity(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadlessGpuCandidateBindingState {
    Attempted,
    Satisfied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeadlessGpuCandidateBinding {
    pub(crate) intent: HeadlessGpuCandidateIntent,
    pub(crate) state: HeadlessGpuCandidateBindingState,
}

impl HeadlessGpuCandidateBinding {
    pub(crate) fn covers_current(self, current: HeadlessGpuCandidateIntent) -> bool {
        if self.intent == current {
            return true;
        }
        self.state == HeadlessGpuCandidateBindingState::Satisfied
            && self.intent.epoch == current.epoch
            && self.intent.quality_revision == current.quality_revision
            && self.intent.frame == current.frame
            && self.intent.pending_demand.is_some()
            && current.pending_demand.is_none()
    }

    fn satisfies(self, sampled: HeadlessGpuCandidateIntent) -> bool {
        self.state == HeadlessGpuCandidateBindingState::Satisfied && self.covers_current(sampled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadlessGpuCandidateBindingUpdate {
    AttemptedIntent,
    SatisfiedIntent,
    RetryCurrentIntent,
    Preserve,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HeadlessGpuCandidateOutputBindingUpdate {
    Gpu(super::preview_execution::PreviewOutputKey),
    NonGpu,
    Preserve,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HeadlessGpuCandidateOutputBinding {
    Gpu(super::preview_execution::PreviewOutputKey),
    NonGpu,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeadlessGpuCandidateAttempt {
    pub(crate) status: HeadlessGpuCandidateStatus,
    pub(crate) binding: HeadlessGpuCandidateBindingUpdate,
    pub(crate) output_binding: HeadlessGpuCandidateOutputBindingUpdate,
}

pub(crate) fn apply_headless_candidate_binding(
    binding: &mut Option<HeadlessGpuCandidateBinding>,
    attempted_intent: HeadlessGpuCandidateIntent,
    update: HeadlessGpuCandidateBindingUpdate,
) {
    match update {
        HeadlessGpuCandidateBindingUpdate::AttemptedIntent => {
            *binding = Some(HeadlessGpuCandidateBinding {
                intent: attempted_intent,
                state: HeadlessGpuCandidateBindingState::Attempted,
            });
        }
        HeadlessGpuCandidateBindingUpdate::SatisfiedIntent => {
            if attempted_intent.pending_demand.is_none()
                && binding.is_some_and(|existing| existing.covers_current(attempted_intent))
            {
                return;
            }
            *binding = Some(HeadlessGpuCandidateBinding {
                intent: attempted_intent,
                state: HeadlessGpuCandidateBindingState::Satisfied,
            });
        }
        HeadlessGpuCandidateBindingUpdate::RetryCurrentIntent => *binding = None,
        HeadlessGpuCandidateBindingUpdate::Preserve => {}
    }
}

pub(crate) fn apply_headless_candidate_output_binding(
    output_binding: &mut Option<HeadlessGpuCandidateOutputBinding>,
    update: HeadlessGpuCandidateOutputBindingUpdate,
) {
    match update {
        HeadlessGpuCandidateOutputBindingUpdate::Gpu(key) => {
            *output_binding = Some(HeadlessGpuCandidateOutputBinding::Gpu(key));
        }
        HeadlessGpuCandidateOutputBindingUpdate::NonGpu => {
            *output_binding = Some(HeadlessGpuCandidateOutputBinding::NonGpu);
        }
        HeadlessGpuCandidateOutputBindingUpdate::Preserve => {}
    }
}

pub(crate) fn headless_candidate_is_ready_for_sample(
    status: HeadlessGpuCandidateStatus,
    binding: Option<HeadlessGpuCandidateBinding>,
    sampled_intent: HeadlessGpuCandidateIntent,
    output_binding_matches: bool,
) -> bool {
    matches!(
        status,
        HeadlessGpuCandidateStatus::Ready | HeadlessGpuCandidateStatus::QueuedReady
    ) && binding.is_some_and(|binding| binding.satisfies(sampled_intent))
        && output_binding_matches
}

pub(crate) fn headless_candidate_may_prepare_successor(status: HeadlessGpuCandidateStatus) -> bool {
    matches!(
        status,
        HeadlessGpuCandidateStatus::Ready
            | HeadlessGpuCandidateStatus::QueuedReady
            | HeadlessGpuCandidateStatus::InFlight
    )
}

pub(crate) fn should_attempt_headless_gpu_candidate(
    status: HeadlessGpuCandidateStatus,
    candidate_binding: Option<HeadlessGpuCandidateBinding>,
    current_intent: HeadlessGpuCandidateIntent,
    pump_outcome: PlaybackPreviewPumpOutcome,
) -> bool {
    let preview_progress = pump_outcome.visible_change
        || pump_outcome.transport_change
        || pump_outcome.candidate_retry_required;
    let binding_covers_current =
        candidate_binding.is_some_and(|binding| binding.covers_current(current_intent));
    match status {
        HeadlessGpuCandidateStatus::Ready => !binding_covers_current,
        HeadlessGpuCandidateStatus::QueuedReady | HeadlessGpuCandidateStatus::InFlight => true,
        HeadlessGpuCandidateStatus::Loading => !binding_covers_current || preview_progress,
        HeadlessGpuCandidateStatus::Backpressured | HeadlessGpuCandidateStatus::Unavailable => true,
        HeadlessGpuCandidateStatus::DroppedLate => current_intent.pending_demand.is_some(),
    }
}

pub(crate) fn headless_candidate_output_binding_matches(
    binding: Option<&HeadlessGpuCandidateOutputBinding>,
    preview: &HeadlessPreviewRuntime,
) -> bool {
    match binding {
        Some(HeadlessGpuCandidateOutputBinding::Gpu(key)) => preview.has_gpu_output_for_key(key),
        Some(HeadlessGpuCandidateOutputBinding::NonGpu) => true,
        None => false,
    }
}

pub(crate) fn headless_preview_wait_budget(
    deadline: Instant,
    now: Instant,
    needs_follow_up_poll: bool,
) -> Option<Duration> {
    if needs_follow_up_poll {
        return None;
    }
    let max_wait = deadline
        .saturating_duration_since(now)
        .min(HEADLESS_PREVIEW_CLOCK_TICK_MAX_WAIT);
    (!max_wait.is_zero()).then_some(max_wait)
}

pub(crate) fn wait_for_headless_preview_revision(
    watch: &PreviewWorkWatch,
    drain_target_revision: PreviewWorkRevision,
    deadline: Instant,
    needs_follow_up_poll: bool,
) {
    if let Some(max_wait) =
        headless_preview_wait_budget(deadline, Instant::now(), needs_follow_up_poll)
    {
        #[cfg(windows)]
        {
            if watch.revision() != drain_target_revision {
                return;
            }
            if super::viewer_gpu_device_progress::wait_with_high_resolution_timer(max_wait) {
                return;
            }
        }
        let _ = watch.wait_for_change(drain_target_revision, max_wait);
    }
}

/// Bind one Preview Runtime to the exact GPU Adapter that supplies its decoder admission.
pub(crate) fn configure_headless_gpu_decode_admission(
    preview: &HeadlessPreviewRuntime,
    gpu: &mut HeadlessViewerGpuAdapter,
) -> anyhow::Result<()> {
    gpu.install_completion_waker(preview.work_watch().completion_waker());
    let admission = resolve_playback_hardware_decode_admission(&gpu.native_import_support());
    preview
        .set_renderer_hardware_decode_admission(admission, gpu.native_decode_device_root())
        .context("bind Headless Preview to the renderer-qualified decoder device")
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub(crate) struct HeadlessRealtimeStageTiming {
    observations: u64,
    max_us: u64,
    above_5ms: u64,
    above_20ms: u64,
    above_frame_interval: u64,
}

impl HeadlessRealtimeStageTiming {
    fn observe(&mut self, duration: Duration) {
        self.observations = self.observations.saturating_add(1);
        let elapsed_us = duration.as_micros().min(u128::from(u64::MAX)) as u64;
        self.max_us = self.max_us.max(elapsed_us);
        self.above_5ms = self.above_5ms.saturating_add(u64::from(elapsed_us > 5_000));
        self.above_20ms = self.above_20ms.saturating_add(u64::from(elapsed_us > 20_000));
        self.above_frame_interval =
            self.above_frame_interval.saturating_add(u64::from(elapsed_us > 33_366));
    }

    fn merge(&mut self, interval: Self) {
        self.observations = self.observations.saturating_add(interval.observations);
        self.max_us = self.max_us.max(interval.max_us);
        self.above_5ms = self.above_5ms.saturating_add(interval.above_5ms);
        self.above_20ms = self.above_20ms.saturating_add(interval.above_20ms);
        self.above_frame_interval =
            self.above_frame_interval.saturating_add(interval.above_frame_interval);
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct HeadlessRealtimeIntervalTiming {
    total: HeadlessRealtimeStageTiming,
    audio_pump: HeadlessRealtimeStageTiming,
    clock_advance: HeadlessRealtimeStageTiming,
    preview_pump: HeadlessRealtimeStageTiming,
    candidate: HeadlessRealtimeStageTiming,
    successor: HeadlessRealtimeStageTiming,
    lookahead: HeadlessRealtimeStageTiming,
    wait: HeadlessRealtimeStageTiming,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub(crate) struct HeadlessRealtimeCoordinatorTiming {
    pub(crate) intervals: u64,
    pub(crate) stale_intervals: u64,
    pub(crate) candidate_attempts: u64,
    pub(crate) stale_without_candidate_attempt: u64,
    pub(crate) stale_after_candidate_attempt: u64,
    pub(crate) advanced_frames: u64,
    pub(crate) non_unit_frame_advances: u64,
    pub(crate) maximum_frame_advance: u64,
    pub(crate) stale_bursts: u64,
    pub(crate) max_consecutive_stale: u64,
    stale_candidate_status: HeadlessRealtimeCandidateStatusCounts,
    #[serde(skip)]
    current_consecutive_stale: u64,
    total: HeadlessRealtimeStageTiming,
    audio_pump: HeadlessRealtimeStageTiming,
    clock_advance: HeadlessRealtimeStageTiming,
    preview_pump: HeadlessRealtimeStageTiming,
    candidate: HeadlessRealtimeStageTiming,
    successor: HeadlessRealtimeStageTiming,
    lookahead: HeadlessRealtimeStageTiming,
    wait: HeadlessRealtimeStageTiming,
}

impl HeadlessRealtimeCoordinatorTiming {
    fn record_interval(
        &mut self,
        sample: HeadlessPreviewSample,
        candidate_status: HeadlessGpuCandidateStatus,
        advanced_frames: u64,
        interval: HeadlessRealtimeIntervalTiming,
    ) {
        self.intervals = self.intervals.saturating_add(1);
        self.candidate_attempts =
            self.candidate_attempts.saturating_add(interval.candidate.observations);
        self.advanced_frames = self.advanced_frames.saturating_add(advanced_frames);
        self.non_unit_frame_advances =
            self.non_unit_frame_advances.saturating_add(u64::from(advanced_frames != 1));
        self.maximum_frame_advance = self.maximum_frame_advance.max(advanced_frames);
        if sample.current_gpu_ready {
            self.current_consecutive_stale = 0;
        } else {
            self.stale_intervals = self.stale_intervals.saturating_add(1);
            self.current_consecutive_stale = self.current_consecutive_stale.saturating_add(1);
            if self.current_consecutive_stale == 1 {
                self.stale_bursts = self.stale_bursts.saturating_add(1);
            }
            self.max_consecutive_stale =
                self.max_consecutive_stale.max(self.current_consecutive_stale);
            if interval.candidate.observations == 0 {
                self.stale_without_candidate_attempt =
                    self.stale_without_candidate_attempt.saturating_add(1);
            } else {
                self.stale_after_candidate_attempt =
                    self.stale_after_candidate_attempt.saturating_add(1);
            }
            self.stale_candidate_status.record(candidate_status);
        }
        self.total.merge(interval.total);
        self.audio_pump.merge(interval.audio_pump);
        self.clock_advance.merge(interval.clock_advance);
        self.preview_pump.merge(interval.preview_pump);
        self.candidate.merge(interval.candidate);
        self.successor.merge(interval.successor);
        self.lookahead.merge(interval.lookahead);
        self.wait.merge(interval.wait);
    }

    #[cfg(all(test, feature = "validation"))]
    pub(crate) fn professional_observation(self) -> ProfessionalVideoCoordinatorObservation {
        ProfessionalVideoCoordinatorObservation {
            intervals: self.intervals,
            stale_intervals: self.stale_intervals,
            candidate_attempts: self.candidate_attempts,
            stale_without_candidate_attempt: self.stale_without_candidate_attempt,
            stale_after_candidate_attempt: self.stale_after_candidate_attempt,
            advanced_frames: self.advanced_frames,
            non_unit_frame_advances: self.non_unit_frame_advances,
            maximum_frame_advance: self.maximum_frame_advance,
            stale_bursts: self.stale_bursts,
            max_consecutive_stale: self.max_consecutive_stale,
            stale_ready: self.stale_candidate_status.ready,
            stale_queued_ready: self.stale_candidate_status.queued_ready,
            stale_in_flight: self.stale_candidate_status.in_flight,
            stale_loading: self.stale_candidate_status.loading,
            stale_backpressured: self.stale_candidate_status.backpressured,
            stale_dropped_late: self.stale_candidate_status.dropped_late,
            stale_unavailable: self.stale_candidate_status.unavailable,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
struct HeadlessRealtimeCandidateStatusCounts {
    ready: u64,
    queued_ready: u64,
    in_flight: u64,
    loading: u64,
    backpressured: u64,
    dropped_late: u64,
    unavailable: u64,
}

impl HeadlessRealtimeCandidateStatusCounts {
    fn record(&mut self, status: HeadlessGpuCandidateStatus) {
        let counter = match status {
            HeadlessGpuCandidateStatus::Ready => &mut self.ready,
            HeadlessGpuCandidateStatus::QueuedReady => &mut self.queued_ready,
            HeadlessGpuCandidateStatus::InFlight => &mut self.in_flight,
            HeadlessGpuCandidateStatus::Loading => &mut self.loading,
            HeadlessGpuCandidateStatus::Backpressured => &mut self.backpressured,
            HeadlessGpuCandidateStatus::DroppedLate => &mut self.dropped_late,
            HeadlessGpuCandidateStatus::Unavailable => &mut self.unavailable,
        };
        *counter = counter.saturating_add(1);
    }
}

#[derive(Debug)]
struct HeadlessRealtimePlaybackDriver {
    absolute_deadline: Option<Instant>,
    thread_scheduling: PlaybackThreadScheduling,
    candidate_binding: Option<HeadlessGpuCandidateBinding>,
    candidate_output_binding: Option<HeadlessGpuCandidateOutputBinding>,
    candidate_status: HeadlessGpuCandidateStatus,
    prepared_successor_intent: Option<super::preview_execution::PreviewPlaybackIntent>,
    timing: HeadlessRealtimeCoordinatorTiming,
}

impl HeadlessRealtimePlaybackDriver {
    fn with_absolute_deadline(absolute_deadline: Option<Instant>) -> anyhow::Result<Self> {
        let mut thread_scheduling = PlaybackThreadScheduling::default();
        let scheduling_status = thread_scheduling
            .synchronize(true)
            .context("enter native playback thread scheduling class")?;
        #[cfg(target_os = "windows")]
        anyhow::ensure!(
            scheduling_status == PlaybackThreadSchedulingStatus::Active,
            "Windows Headless realtime playback did not enter the native multimedia scheduling class"
        );
        #[cfg(not(target_os = "windows"))]
        let _ = scheduling_status;
        Ok(Self {
            absolute_deadline,
            thread_scheduling,
            candidate_binding: None,
            candidate_output_binding: None,
            candidate_status: HeadlessGpuCandidateStatus::Loading,
            prepared_successor_intent: None,
            timing: HeadlessRealtimeCoordinatorTiming::default(),
        })
    }

    fn finish(mut self) -> anyhow::Result<HeadlessRealtimeCoordinatorTiming> {
        let scheduling_status = self
            .thread_scheduling
            .synchronize(false)
            .context("leave native playback thread scheduling class")?;
        anyhow::ensure!(
            scheduling_status == PlaybackThreadSchedulingStatus::Inactive,
            "Headless realtime playback did not leave its native thread scheduling class"
        );
        Ok(self.timing)
    }

    fn abort(mut self) {
        let _ = self.thread_scheduling.synchronize(false);
    }

    fn reset_candidate_reconciliation(&mut self) {
        self.candidate_binding = None;
        self.candidate_output_binding = None;
        self.candidate_status = HeadlessGpuCandidateStatus::Loading;
    }

    fn sample(
        &self,
        sampled_intent: HeadlessGpuCandidateIntent,
        preview: &HeadlessPreviewRuntime,
    ) -> HeadlessPreviewSample {
        let output_binding_matches = headless_candidate_output_binding_matches(
            self.candidate_output_binding.as_ref(),
            preview,
        );
        HeadlessPreviewSample {
            current_gpu_ready: headless_candidate_is_ready_for_sample(
                self.candidate_status,
                self.candidate_binding,
                sampled_intent,
                output_binding_matches,
            ),
            stale_output_available: preview.has_retained_gpu_output(),
            unavailable: self.candidate_status == HeadlessGpuCandidateStatus::Unavailable,
        }
    }
}

/// Fixed-size owner-derived facts captured only outside realtime residency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeadlessEnduranceOwnerSnapshot {
    playback_pending: u64,
    other_queue_depth: u64,
    owned_resource_units: u64,
    gpu_device_losses: u64,
    gpu_fatal_errors: u64,
    other_fatal_errors: u64,
    app_background: AppBackgroundEnduranceSnapshot,
    fatal_error_total: u64,
    owner_capture_failed: bool,
}

/// Consuming closure facts that can only be produced after the owner group ran
/// every synchronous shutdown path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeadlessEnduranceShutdownProjection {
    pub(crate) playback_owner_consumed: bool,
    pub(crate) preview_closed: bool,
    pub(crate) audio_closed: bool,
    pub(crate) app_residual_owners_closed: bool,
    pub(crate) app_background: Option<AppBackgroundEnduranceSnapshot>,
    pub(crate) gpu_closed: bool,
    pub(crate) gpu_device_losses: u64,
    pub(crate) gpu_fatal_errors: u64,
    pub(crate) transport_shutdown_failed: bool,
}

impl HeadlessEnduranceOwnerSnapshot {
    pub(crate) const fn playback_pending(self) -> u64 {
        self.playback_pending
    }

    pub(crate) const fn other_queue_depth(self) -> u64 {
        self.other_queue_depth
    }

    pub(crate) const fn owned_resource_units(self) -> u64 {
        self.owned_resource_units
    }

    pub(crate) const fn gpu_device_losses(self) -> u64 {
        self.gpu_device_losses
    }

    pub(crate) const fn fatal_errors(self) -> u64 {
        self.fatal_error_total
    }

    #[cfg(test)]
    pub(crate) const fn test_fixture(
        playback_pending: u64,
        other_queue_depth: u64,
        owned_resource_units: u64,
        gpu_device_losses: u64,
        fatal_errors: u64,
    ) -> Self {
        Self {
            playback_pending,
            other_queue_depth,
            owned_resource_units,
            gpu_device_losses,
            gpu_fatal_errors: 0,
            other_fatal_errors: fatal_errors,
            app_background: AppBackgroundEnduranceSnapshot {
                audio_idle_warmup: super::endurance_shutdown::AppBackgroundDomainSnapshot {
                    queue_depth: 0,
                    owned_resource_units: 0,
                    cumulative_failures: 0,
                    worker_health_failures: 0,
                },
                media_import: super::endurance_shutdown::AppBackgroundDomainSnapshot {
                    queue_depth: 0,
                    owned_resource_units: 0,
                    cumulative_failures: 0,
                    worker_health_failures: 0,
                },
                media_asset_mutation: super::endurance_shutdown::AppBackgroundDomainSnapshot {
                    queue_depth: 0,
                    owned_resource_units: 0,
                    cumulative_failures: 0,
                    worker_health_failures: 0,
                },
                visual_tracking: super::endurance_shutdown::AppBackgroundDomainSnapshot {
                    queue_depth: 0,
                    owned_resource_units: 0,
                    cumulative_failures: 0,
                    worker_health_failures: 0,
                },
                proxy_generation: super::endurance_shutdown::AppBackgroundDomainSnapshot {
                    queue_depth: 0,
                    owned_resource_units: 0,
                    cumulative_failures: 0,
                    worker_health_failures: 0,
                },
                infrastructure: super::endurance_shutdown::AppBackgroundDomainSnapshot {
                    queue_depth: 0,
                    owned_resource_units: 0,
                    cumulative_failures: 0,
                    worker_health_failures: 0,
                },
            },
            fatal_error_total: fatal_errors,
            owner_capture_failed: false,
        }
    }

    pub(crate) fn failed_capture() -> Self {
        Self {
            playback_pending: 1,
            other_queue_depth: 1,
            owned_resource_units: 1,
            gpu_device_losses: 0,
            gpu_fatal_errors: 0,
            other_fatal_errors: 0,
            app_background: AppBackgroundEnduranceSnapshot::default(),
            fatal_error_total: 0,
            owner_capture_failed: true,
        }
    }

    /// Project a consuming owner closure without losing pre-shutdown counters.
    pub(crate) fn after_shutdown(
        self,
        closure: HeadlessEnduranceShutdownProjection,
    ) -> Result<Self, String> {
        let terminal_app_background = closure
            .app_background
            .ok_or_else(|| "App background terminal snapshot is unavailable".to_owned())?;
        let app_background = self.app_background.merge_terminal(terminal_app_background);
        let background_totals = app_background.totals()?;
        let app_background_fatal_errors = background_totals
            .cumulative_failures
            .checked_add(background_totals.worker_health_failures)
            .ok_or_else(|| "App background terminal failure count overflowed u64".to_owned())?;
        let closure_failures = [
            !closure.playback_owner_consumed,
            !closure.preview_closed,
            !closure.audio_closed,
            !closure.app_residual_owners_closed,
            !closure.gpu_closed,
            self.owner_capture_failed,
        ]
        .into_iter()
        .try_fold(0_u64, |total, failed| total.checked_add(u64::from(failed)))
        .ok_or_else(|| "Headless closure failure count overflowed u64".to_owned())?;
        let background_owners_open =
            background_totals.queue_depth != 0 || background_totals.owned_resource_units != 0;
        let unclosed_resource_domains = closure_failures
            .checked_add(u64::from(background_owners_open))
            .ok_or_else(|| "Headless open-domain count overflowed u64".to_owned())?;
        let all_closed = unclosed_resource_domains == 0;
        let other_fatal_errors = self
            .other_fatal_errors
            .checked_add(closure_failures)
            .and_then(|value| value.checked_add(u64::from(closure.transport_shutdown_failed)))
            .ok_or_else(|| "Headless terminal failure count overflowed u64".to_owned())?;
        let gpu_fatal_errors = self.gpu_fatal_errors.max(closure.gpu_fatal_errors);
        let fatal_error_total = gpu_fatal_errors
            .checked_add(other_fatal_errors)
            .and_then(|value| value.checked_add(app_background_fatal_errors))
            .ok_or_else(|| "Headless total terminal failure count overflowed u64".to_owned())?;
        Ok(Self {
            playback_pending: if all_closed {
                0
            } else {
                self.playback_pending.max(1)
            },
            other_queue_depth: if all_closed {
                0
            } else {
                self.other_queue_depth.max(1)
            },
            owned_resource_units: if all_closed {
                0
            } else {
                self.owned_resource_units.max(unclosed_resource_domains)
            },
            gpu_device_losses: self.gpu_device_losses.max(closure.gpu_device_losses),
            gpu_fatal_errors,
            other_fatal_errors,
            app_background,
            fatal_error_total,
            owner_capture_failed: self.owner_capture_failed,
        })
    }

    /// Retain every known counter and fail closed when terminal projection
    /// cannot be completed after the execution owners have been consumed.
    pub(crate) fn fail_closed_after_shutdown(
        self,
        closure: HeadlessEnduranceShutdownProjection,
    ) -> Self {
        let app_background = closure.app_background.map_or(self.app_background, |terminal| {
            merge_background_fail_closed(self.app_background, terminal)
        });
        let closure_failures = [
            !closure.playback_owner_consumed,
            !closure.preview_closed,
            !closure.audio_closed,
            !closure.app_residual_owners_closed,
            !closure.gpu_closed,
            self.owner_capture_failed,
        ]
        .into_iter()
        .fold(0_u64, |total, failed| {
            total.saturating_add(u64::from(failed))
        });
        let other_fatal_errors = self
            .other_fatal_errors
            .saturating_add(closure_failures)
            .saturating_add(u64::from(closure.transport_shutdown_failed))
            .saturating_add(1);
        let gpu_fatal_errors = self.gpu_fatal_errors.max(closure.gpu_fatal_errors);
        let fatal_error_total = self
            .fatal_error_total
            .max(
                gpu_fatal_errors
                    .saturating_add(other_fatal_errors)
                    .saturating_add(saturating_background_failure_total(app_background)),
            )
            .max(1);
        let minimum_open_resources = closure_failures.saturating_add(1);
        Self {
            playback_pending: self.playback_pending.max(1),
            other_queue_depth: self.other_queue_depth.max(1),
            owned_resource_units: self.owned_resource_units.max(minimum_open_resources),
            gpu_device_losses: self.gpu_device_losses.max(closure.gpu_device_losses),
            gpu_fatal_errors,
            other_fatal_errors,
            app_background,
            fatal_error_total,
            owner_capture_failed: self.owner_capture_failed,
        }
    }
}

fn merge_background_fail_closed(
    running: AppBackgroundEnduranceSnapshot,
    terminal: AppBackgroundEnduranceSnapshot,
) -> AppBackgroundEnduranceSnapshot {
    AppBackgroundEnduranceSnapshot {
        audio_idle_warmup: merge_background_domain_fail_closed(
            running.audio_idle_warmup,
            terminal.audio_idle_warmup,
        ),
        media_import: merge_background_domain_fail_closed(
            running.media_import,
            terminal.media_import,
        ),
        media_asset_mutation: merge_background_domain_fail_closed(
            running.media_asset_mutation,
            terminal.media_asset_mutation,
        ),
        visual_tracking: merge_background_domain_fail_closed(
            running.visual_tracking,
            terminal.visual_tracking,
        ),
        proxy_generation: merge_background_domain_fail_closed(
            running.proxy_generation,
            terminal.proxy_generation,
        ),
        infrastructure: merge_background_domain_fail_closed(
            running.infrastructure,
            terminal.infrastructure,
        ),
    }
}

fn merge_background_domain_fail_closed(
    running: AppBackgroundDomainSnapshot,
    terminal: AppBackgroundDomainSnapshot,
) -> AppBackgroundDomainSnapshot {
    AppBackgroundDomainSnapshot {
        queue_depth: running.queue_depth.max(terminal.queue_depth),
        owned_resource_units: running.owned_resource_units.max(terminal.owned_resource_units),
        cumulative_failures: running.cumulative_failures.max(terminal.cumulative_failures),
        worker_health_failures: running.worker_health_failures.max(terminal.worker_health_failures),
    }
}

fn saturating_background_failure_total(snapshot: AppBackgroundEnduranceSnapshot) -> u64 {
    [
        snapshot.audio_idle_warmup,
        snapshot.media_import,
        snapshot.media_asset_mutation,
        snapshot.visual_tracking,
        snapshot.proxy_generation,
        snapshot.infrastructure,
    ]
    .into_iter()
    .fold(0_u64, |total, domain| {
        total
            .saturating_add(domain.cumulative_failures)
            .saturating_add(domain.worker_health_failures)
    })
}

/// One correctly paired Preview/GPU/driver lifetime for Headless realtime work.
pub(crate) struct HeadlessRealtimePlaybackSession {
    preview: HeadlessPreviewRuntime,
    gpu: HeadlessViewerGpuAdapter,
    driver: Option<HeadlessRealtimePlaybackDriver>,
}

type HeadlessRealtimeBindFailure = Box<(
    anyhow::Error,
    HeadlessPreviewRuntime,
    HeadlessViewerGpuAdapter,
)>;

impl HeadlessRealtimePlaybackSession {
    /// Create and bind the default real Headless GPU Adapter.
    pub(crate) fn new() -> anyhow::Result<Self> {
        let gpu = HeadlessViewerGpuAdapter::new().context("create Headless Viewer GPU Adapter")?;
        Self::with_gpu_adapter(gpu)
    }

    /// Bind an explicitly configured Adapter without entering realtime residency.
    pub(crate) fn with_gpu_adapter(gpu: HeadlessViewerGpuAdapter) -> anyhow::Result<Self> {
        let preview = HeadlessPreviewRuntime::new();
        Self::with_shutdown_owners(preview, gpu).map_err(|failure| {
            let (error, _, _) = *failure;
            error
        })
    }

    /// Bind already-owned Preview/GPU resources while preserving both owners
    /// for an exact consuming shutdown if admission setup fails.
    pub(crate) fn with_shutdown_owners(
        preview: HeadlessPreviewRuntime,
        mut gpu: HeadlessViewerGpuAdapter,
    ) -> Result<Self, HeadlessRealtimeBindFailure> {
        if let Err(error) = configure_headless_gpu_decode_admission(&preview, &mut gpu) {
            return Err(Box::new((error, preview, gpu)));
        }
        Ok(Self { preview, gpu, driver: None })
    }

    /// The bound Preview owner for setup, diagnostics, and settled operations.
    pub(crate) fn preview(&self) -> anyhow::Result<&HeadlessPreviewRuntime> {
        anyhow::ensure!(
            self.driver.is_none(),
            "Headless Preview setup access is unavailable during realtime residency"
        );
        Ok(&self.preview)
    }

    /// The bound GPU owner for immutable adapter evidence.
    pub(crate) fn gpu(&self) -> anyhow::Result<&HeadlessViewerGpuAdapter> {
        anyhow::ensure!(
            self.driver.is_none(),
            "Headless GPU evidence access is unavailable during realtime residency"
        );
        Ok(&self.gpu)
    }

    /// The bound GPU owner for post-realtime evidence collection.
    pub(crate) fn gpu_mut(&mut self) -> anyhow::Result<&mut HeadlessViewerGpuAdapter> {
        anyhow::ensure!(
            self.driver.is_none(),
            "Headless GPU mutable access is unavailable during realtime residency"
        );
        Ok(&mut self.gpu)
    }

    /// Borrow the exact Preview/GPU pair for setup or settled presentation work.
    pub(crate) fn bound_resources(
        &mut self,
    ) -> anyhow::Result<(&HeadlessPreviewRuntime, &mut HeadlessViewerGpuAdapter)> {
        anyhow::ensure!(
            self.driver.is_none(),
            "Headless Preview/GPU setup access is unavailable during realtime residency"
        );
        Ok((&self.preview, &mut self.gpu))
    }

    /// Capture the paired execution inventory at a declared settled boundary.
    ///
    /// This is one coordinator-owned capture envelope, not a claim that the
    /// independent Preview, Audio, and GPU threads share a global linearization
    /// instant. This path does not schedule, pump, or poll phase work; a domain
    /// snapshot may still refresh its own bounded diagnostic cache.
    pub(crate) fn endurance_owner_snapshot(
        &self,
        state: &AppState,
    ) -> anyhow::Result<HeadlessEnduranceOwnerSnapshot> {
        anyhow::ensure!(
            self.driver.is_none(),
            "Headless endurance owner snapshots require a settled realtime boundary"
        );
        capture_headless_endurance_owner_snapshot(&self.preview, &self.gpu, state)
    }

    /// Enter one fresh realtime residency after the transport starts Playing.
    pub(crate) fn begin_realtime(
        &mut self,
        state: &AppState,
        absolute_deadline: Option<Instant>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.driver.is_none(),
            "Headless realtime playback residency is already active"
        );
        anyhow::ensure!(
            state.is_playing(),
            "Headless realtime playback residency requires Playing transport"
        );
        self.driver = Some(HeadlessRealtimePlaybackDriver::with_absolute_deadline(
            absolute_deadline,
        )?);
        Ok(())
    }

    /// Execute one production A/V interval through the paired coordinator.
    pub(crate) fn run_production_av_interval<O: HeadlessGpuExecutionObserver>(
        &mut self,
        state: &mut AppState,
        observer: &mut O,
        gpu_completion_timeout: Duration,
    ) -> anyhow::Result<HeadlessPreviewSample> {
        let driver = self
            .driver
            .as_mut()
            .context("Headless realtime playback residency is not active")?;
        run_headless_production_av_interval(
            &self.preview,
            state,
            &mut self.gpu,
            observer,
            gpu_completion_timeout,
            driver,
        )
    }

    /// Execute one video-only interval through the paired coordinator.
    pub(crate) fn run_video_interval<O: HeadlessGpuExecutionObserver>(
        &mut self,
        state: &mut AppState,
        observer: &mut O,
        gpu_completion_timeout: Duration,
    ) -> anyhow::Result<HeadlessRealtimeIntervalOutcome> {
        let driver = self
            .driver
            .as_mut()
            .context("Headless realtime playback residency is not active")?;
        run_headless_realtime_video_interval(
            &self.preview,
            state,
            &mut self.gpu,
            observer,
            gpu_completion_timeout,
            driver,
        )
    }

    /// Drain completions once at a declared realtime observation boundary.
    pub(crate) fn pump_preview_completion(
        &mut self,
        state: &mut AppState,
    ) -> anyhow::Result<PlaybackPreviewPumpOutcome> {
        anyhow::ensure!(
            self.driver.is_some(),
            "Headless realtime playback residency is not active"
        );
        Ok(pump_playback_preview(state, &self.preview))
    }

    /// Close the unresolved exact current-frame demand without advancing time.
    pub(crate) fn complete_current_video_opportunity<O: HeadlessGpuExecutionObserver>(
        &mut self,
        state: &mut AppState,
        observer: &mut O,
        timeout: Duration,
    ) -> anyhow::Result<HeadlessPreviewSample> {
        let driver = self
            .driver
            .as_mut()
            .context("Headless realtime playback residency is not active")?;
        let interval_deadline = Instant::now()
            .checked_add(timeout)
            .context("derive Headless terminal-observation deadline")?;
        let deadline = driver.absolute_deadline.map_or(interval_deadline, |absolute| {
            absolute.min(interval_deadline)
        });
        let completion_deadline = HeadlessGpuCompletionDeadline::at(deadline);
        let work_watch = self.preview.work_watch();
        let mut target_intent = HeadlessGpuCandidateIntent::from_state(state);
        loop {
            let drain_target_revision = work_watch.revision();
            let pump_outcome = pump_playback_preview(state, &self.preview);
            let current_intent = HeadlessGpuCandidateIntent::from_state(state);
            if target_intent.pending_demand.is_some() {
                let attempt = execute_headless_gpu_candidate(
                    &self.preview,
                    state,
                    &mut self.gpu,
                    observer,
                    completion_deadline,
                )?;
                driver.candidate_status = attempt.status;
                apply_headless_candidate_binding(
                    &mut driver.candidate_binding,
                    current_intent,
                    attempt.binding,
                );
                apply_headless_candidate_output_binding(
                    &mut driver.candidate_output_binding,
                    attempt.output_binding,
                );
            }
            let sampled_intent = HeadlessGpuCandidateIntent::from_state(state);
            let sample = driver.sample(sampled_intent, &self.preview);
            if sample.current_gpu_ready {
                return Ok(sample);
            }
            if headless_terminal_observation_may_retarget_quality(target_intent, sampled_intent) {
                target_intent = sampled_intent;
                driver.reset_candidate_reconciliation();
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "timed out retargeting Headless terminal observation to the latest quality demand: {target_intent:?}"
                );
                continue;
            }
            anyhow::ensure!(
                sampled_intent.epoch == target_intent.epoch
                    && sampled_intent.quality_revision == target_intent.quality_revision
                    && sampled_intent.frame == target_intent.frame,
                "Headless terminal observation changed intent before resolving its exact demand: target={target_intent:?}, current={sampled_intent:?}"
            );
            if state.playback_engine.snapshot().state == mondrian_playback::TransportState::Ended {
                anyhow::ensure!(
                    state.playback_engine.active_playback_demand().is_none(),
                    "ended transport must retire its timed playback demand: {:?}",
                    state.playback_engine.frame_demand()
                );
            }
            if headless_demand_resolved_without_ready(target_intent, sampled_intent) {
                let output_published = self.preview.has_retained_gpu_output();
                return Ok(HeadlessPreviewSample {
                    current_gpu_ready: output_published,
                    stale_output_available: output_published,
                    unavailable: driver.candidate_status == HeadlessGpuCandidateStatus::Unavailable,
                });
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "timed out closing a real Headless terminal presentation; target_intent={target_intent:?}, current_intent={sampled_intent:?}, candidate_status={:?}, pending_demand={:?}, transport={:?}, diagnostics={:?}",
                driver.candidate_status,
                state.pending_playback_frame_demand_identity(),
                state.playback_engine.snapshot(),
                self.preview.diagnostics(),
            );
            wait_for_headless_preview_revision(
                &work_watch,
                drain_target_revision,
                deadline,
                pump_outcome.needs_follow_up_poll
                    && !driver.candidate_status.requires_bounded_wait(),
            );
        }
    }

    /// Leave realtime coordinator scheduling at the declared observation boundary.
    pub(crate) fn finish_realtime(&mut self) -> anyhow::Result<HeadlessRealtimeCoordinatorTiming> {
        self.driver
            .take()
            .context("Headless realtime playback residency is not active")?
            .finish()
    }

    pub(crate) fn into_shutdown_owners(
        mut self,
    ) -> (HeadlessPreviewRuntime, HeadlessViewerGpuAdapter) {
        if let Some(driver) = self.driver.take() {
            driver.abort();
        }
        (self.preview, self.gpu)
    }
}

pub(crate) fn capture_headless_endurance_owner_snapshot(
    preview_owner: &HeadlessPreviewRuntime,
    gpu_owner: &HeadlessViewerGpuAdapter,
    state: &AppState,
) -> anyhow::Result<HeadlessEnduranceOwnerSnapshot> {
    project_headless_endurance_owner_snapshot(
        preview_owner.diagnostics(),
        gpu_owner.endurance_snapshot(),
        state.audio_endurance_snapshot(),
        state.pending_playback_frame_demand_identity().is_some(),
        state.background_endurance_snapshot().map_err(anyhow::Error::msg)?,
    )
}

fn project_headless_endurance_owner_snapshot(
    preview: super::preview_runtime::PreviewDiagnostics,
    gpu: HeadlessViewerGpuEnduranceSnapshot,
    audio: mondrian_media::AudioPlaybackSnapshot,
    playback_pending: bool,
    app_background: AppBackgroundEnduranceSnapshot,
) -> anyhow::Result<HeadlessEnduranceOwnerSnapshot> {
    let audio_buffer_owner =
        usize::from(audio.output.as_ref().is_some_and(|output| output.buffered_frames > 0));
    let pinned_viewer_owner = usize::from(preview.frame_store.pinned_viewer_bytes > 0);
    let owned_resource_units = [
        preview.frame_store.media_aggregate_entries,
        preview.frame_store.media_aggregate_resource_units,
        preview.frame_store.viewer_entries,
        pinned_viewer_owner,
        preview.visual_program_cache.entries,
        gpu.submission_owners(),
        gpu.physical_output_owners(),
        gpu.staged_successor_owners(),
        audio.in_flight,
        audio_buffer_owner,
    ]
    .into_iter()
    .try_fold(0_usize, |total, value| total.checked_add(value))
    .ok_or_else(|| anyhow::anyhow!("Headless owned-resource inventory overflowed usize"))?;
    let preview_fatal_errors = [
        u64::from(preview.visual_execution_health_failed),
        u64::from(preview.media_worker_health_failed),
        preview.worker_disconnected_drops,
        u64::from(preview.timeline_render_cache_start_failed),
    ]
    .into_iter()
    .try_fold(0_u64, |total, value| total.checked_add(value))
    .ok_or_else(|| anyhow::anyhow!("Preview failure count overflowed u64"))?;
    let audio_fatal_errors = [
        u64::from(matches!(
            audio.state,
            mondrian_media::AudioPlaybackState::ExecutionUnavailable
        )),
        audio.render_substitution_count,
        audio.render_generation_recovery_count,
        audio.underrun_recovery_count,
        audio.output_lifecycle.backend_loss_count,
        audio.output_lifecycle.deactivation_failed_count,
    ]
    .into_iter()
    .try_fold(0_u64, |total, value| total.checked_add(value))
    .ok_or_else(|| anyhow::anyhow!("Audio failure count overflowed u64"))?;
    let other_queue_depth = [
        preview.scheduler.pending_requests,
        preview.worker_queue.queued_jobs,
        preview.worker_queue.in_flight_jobs,
        audio.in_flight,
    ]
    .into_iter()
    .try_fold(0_usize, |total, value| total.checked_add(value))
    .ok_or_else(|| anyhow::anyhow!("Headless queue inventory overflowed usize"))?;
    let background = app_background.totals().map_err(anyhow::Error::msg)?;
    let other_queue_depth = usize_to_u64(other_queue_depth)?
        .checked_add(background.queue_depth)
        .ok_or_else(|| anyhow::anyhow!("Headless total queue inventory overflowed u64"))?;
    let owned_resource_units = usize_to_u64(owned_resource_units)?
        .checked_add(background.owned_resource_units)
        .ok_or_else(|| anyhow::anyhow!("Headless total resource inventory overflowed u64"))?;
    let other_fatal_errors = preview_fatal_errors
        .checked_add(audio_fatal_errors)
        .ok_or_else(|| anyhow::anyhow!("Headless non-GPU failure count overflowed u64"))?;
    let app_background_fatal_errors = background
        .cumulative_failures
        .checked_add(background.worker_health_failures)
        .ok_or_else(|| anyhow::anyhow!("App background failure count overflowed u64"))?;
    let gpu_fatal_errors = gpu.fatal_error_count();
    let fatal_error_total = gpu_fatal_errors
        .checked_add(other_fatal_errors)
        .and_then(|value| value.checked_add(app_background_fatal_errors))
        .ok_or_else(|| anyhow::anyhow!("Headless total failure count overflowed u64"))?;
    Ok(HeadlessEnduranceOwnerSnapshot {
        playback_pending: u64::from(playback_pending),
        other_queue_depth,
        owned_resource_units,
        gpu_device_losses: gpu.device_loss_count(),
        gpu_fatal_errors,
        other_fatal_errors,
        app_background,
        fatal_error_total,
        owner_capture_failed: false,
    })
}

fn usize_to_u64(value: usize) -> anyhow::Result<u64> {
    u64::try_from(value).map_err(|_| anyhow::anyhow!("usize inventory exceeded u64"))
}

pub(crate) fn headless_terminal_observation_may_retarget_quality(
    target: HeadlessGpuCandidateIntent,
    current: HeadlessGpuCandidateIntent,
) -> bool {
    let Some(current_demand) = current.pending_demand else {
        return false;
    };
    target.epoch == current.epoch
        && target.frame == current.frame
        && current.quality_revision > target.quality_revision
        && current.pending_demand != target.pending_demand
        && current_demand.epoch == current.epoch
        && current_demand.quality_revision == current.quality_revision
        && current_demand.target_frame == current.frame
}

pub(crate) fn headless_demand_resolved_without_ready(
    target: HeadlessGpuCandidateIntent,
    current: HeadlessGpuCandidateIntent,
) -> bool {
    match target.pending_demand {
        Some(target_demand) => current.pending_demand != Some(target_demand),
        None => true,
    }
}

fn run_headless_production_av_interval<O: HeadlessGpuExecutionObserver>(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    observer: &mut O,
    gpu_completion_timeout: Duration,
    driver: &mut HeadlessRealtimePlaybackDriver,
) -> anyhow::Result<HeadlessPreviewSample> {
    match run_headless_realtime_interval(
        preview_service,
        state,
        gpu_adapter,
        observer,
        gpu_completion_timeout,
        driver,
        true,
    )? {
        HeadlessRealtimeIntervalOutcome::Advanced { sample, .. } => Ok(sample),
        HeadlessRealtimeIntervalOutcome::NaturalEnd { terminal_frame } => anyhow::bail!(
            "production A/V playback reached its authored natural end before the caller closed the observation window at frame {terminal_frame}"
        ),
    }
}

fn run_headless_realtime_video_interval<O: HeadlessGpuExecutionObserver>(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    observer: &mut O,
    gpu_completion_timeout: Duration,
    driver: &mut HeadlessRealtimePlaybackDriver,
) -> anyhow::Result<HeadlessRealtimeIntervalOutcome> {
    run_headless_realtime_interval(
        preview_service,
        state,
        gpu_adapter,
        observer,
        gpu_completion_timeout,
        driver,
        false,
    )
}

fn run_headless_realtime_interval<O: HeadlessGpuExecutionObserver>(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    observer: &mut O,
    gpu_completion_timeout: Duration,
    driver: &mut HeadlessRealtimePlaybackDriver,
    pump_audio: bool,
) -> anyhow::Result<HeadlessRealtimeIntervalOutcome> {
    let interval_started = Instant::now();
    let mut interval_timing = HeadlessRealtimeIntervalTiming::default();
    let work_watch = preview_service.work_watch();
    let interval_deadline = Instant::now()
        .checked_add(gpu_completion_timeout)
        .context("derive Headless GPU completion safety deadline")?;
    let safety_deadline_instant = driver.absolute_deadline.map_or(interval_deadline, |deadline| {
        deadline.min(interval_deadline)
    });
    let safety_deadline = HeadlessGpuCompletionDeadline::at(safety_deadline_instant);
    let sampled_epoch = state.playback_epoch();
    let sampled_frame = state.current_frame();
    let sampled_intent = HeadlessGpuCandidateIntent::from_state(state);
    let last_content_frame = state
        .last_content_frame()
        .context("resolve exact Headless realtime terminal frame")?;
    loop {
        let drain_target_revision = work_watch.revision();
        pump_headless_realtime_audio(state, pump_audio, &mut interval_timing)?;
        let now = Instant::now();
        let clock_started = Instant::now();
        state.advance_playback_clock_at(now);
        interval_timing.clock_advance.observe(clock_started.elapsed());
        let preview_pump_started = Instant::now();
        let pump_outcome = pump_playback_preview(state, preview_service);
        interval_timing.preview_pump.observe(preview_pump_started.elapsed());
        if state.playback_epoch() != sampled_epoch || state.current_frame() != sampled_frame {
            let sample = driver.sample(sampled_intent, preview_service);
            let sample_candidate_status = driver.candidate_status;
            let current_intent = HeadlessGpuCandidateIntent::from_state(state);
            let current_playback_intent =
                state.preview_execution_snapshot(Instant::now()).transport().playback_intent();
            let already_visible_at = preview_service
                .already_visible_successor_output_key(current_playback_intent)
                .filter(|key| gpu_adapter.has_current_physical_output_for_key(key))
                .map(|_| now);
            let candidate_started = Instant::now();
            let attempt = execute_headless_gpu_candidate_at(
                preview_service,
                state,
                gpu_adapter,
                observer,
                safety_deadline,
                already_visible_at,
            )?;
            interval_timing.candidate.observe(candidate_started.elapsed());
            driver.candidate_status = attempt.status;
            apply_headless_candidate_binding(
                &mut driver.candidate_binding,
                current_intent,
                attempt.binding,
            );
            apply_headless_candidate_output_binding(
                &mut driver.candidate_output_binding,
                attempt.output_binding,
            );
            interval_timing.total.observe(interval_started.elapsed());
            let advanced_frames = if state.playback_epoch() == sampled_epoch {
                state.current_frame().saturating_sub(sampled_frame).max(1) as u64
            } else {
                1
            };
            driver.timing.record_interval(
                sample,
                sample_candidate_status,
                advanced_frames,
                interval_timing,
            );
            return Ok(HeadlessRealtimeIntervalOutcome::Advanced {
                epoch: sampled_epoch,
                frame: sampled_frame,
                sample,
            });
        }
        let transport = state.playback_engine.snapshot();
        if transport.epoch == sampled_epoch
            && transport.state == mondrian_playback::TransportState::Ended
            && transport.position.frame == sampled_frame
            && transport.position.frame == last_content_frame
        {
            return Ok(HeadlessRealtimeIntervalOutcome::NaturalEnd {
                terminal_frame: transport.position.frame,
            });
        }
        anyhow::ensure!(
            state.is_playing(),
            "Headless realtime playback stopped before the sampled frame advanced: sampled_epoch={sampled_epoch:?}, sampled_frame={sampled_frame}, current_epoch={:?}, current_frame={}, transport={:?}, last_content_frame={:?}",
            transport.epoch,
            transport.position.frame,
            transport.state,
            state.last_content_frame().ok(),
        );

        let current_intent = HeadlessGpuCandidateIntent::from_state(state);
        if gpu_adapter.has_submission_in_flight()
            || should_attempt_headless_gpu_candidate(
                driver.candidate_status,
                driver.candidate_binding,
                current_intent,
                pump_outcome,
            )
        {
            let candidate_started = Instant::now();
            let attempt = execute_headless_gpu_candidate(
                preview_service,
                state,
                gpu_adapter,
                observer,
                safety_deadline,
            )?;
            interval_timing.candidate.observe(candidate_started.elapsed());
            driver.candidate_status = attempt.status;
            apply_headless_candidate_binding(
                &mut driver.candidate_binding,
                current_intent,
                attempt.binding,
            );
            apply_headless_candidate_output_binding(
                &mut driver.candidate_output_binding,
                attempt.output_binding,
            );
        }
        if headless_candidate_may_prepare_successor(driver.candidate_status) {
            let successor_intent = state
                .preview_successor_execution_request(Instant::now())
                .map(|request| request.snapshot().transport().playback_intent());
            let already_prepared = successor_intent.is_some_and(|intent| {
                driver.prepared_successor_intent == Some(intent)
                    && (preview_service.has_prepared_successor_for_intent(intent)
                        || gpu_adapter.has_successor_submission_for_intent(intent))
            });
            if !already_prepared {
                let successor_started = Instant::now();
                let ready = if let Some(intent) = prepare_headless_preview_successor(
                    preview_service,
                    state,
                    gpu_adapter,
                    safety_deadline,
                )? {
                    driver.prepared_successor_intent = Some(intent);
                    true
                } else {
                    false
                };
                observer.successor_preparation(ready);
                interval_timing.successor.observe(successor_started.elapsed());
            }
            let lookahead_started = Instant::now();
            stage_headless_preview_lookahead(preview_service, state, gpu_adapter)?;
            interval_timing.lookahead.observe(lookahead_started.elapsed());
        }

        let wait_observed_at = Instant::now();
        let phase_wake = state
            .playback_next_wake_delay()
            .and_then(|delay| wait_observed_at.checked_add(delay));
        let presentation_deadline = state.playback_frame_deadline_at(wait_observed_at);
        let wake_deadline = [phase_wake, presentation_deadline]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(safety_deadline_instant)
            .min(safety_deadline_instant);
        anyhow::ensure!(
            wait_observed_at < safety_deadline_instant,
            "Headless realtime playback interval exceeded its safety deadline"
        );
        let wait_started = Instant::now();
        wait_for_headless_preview_revision(
            &work_watch,
            drain_target_revision,
            wake_deadline,
            pump_outcome.needs_follow_up_poll && !driver.candidate_status.requires_bounded_wait(),
        );
        interval_timing.wait.observe(wait_started.elapsed());
    }
}

fn pump_headless_realtime_audio(
    state: &mut AppState,
    pump_audio: bool,
    timing: &mut HeadlessRealtimeIntervalTiming,
) -> anyhow::Result<()> {
    if !pump_audio {
        return Ok(());
    }
    let audio_started = Instant::now();
    state.pump_audio_output()?;
    timing.audio_pump.observe(audio_started.elapsed());
    Ok(())
}

pub(crate) fn execute_headless_gpu_candidate<O: HeadlessGpuExecutionObserver>(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    observer: &mut O,
    gpu_completion_deadline: HeadlessGpuCompletionDeadline,
) -> anyhow::Result<HeadlessGpuCandidateAttempt> {
    execute_headless_gpu_candidate_at(
        preview_service,
        state,
        gpu_adapter,
        observer,
        gpu_completion_deadline,
        None,
    )
}

pub(crate) fn execute_headless_gpu_candidate_at<O: HeadlessGpuExecutionObserver>(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    observer: &mut O,
    gpu_completion_deadline: HeadlessGpuCompletionDeadline,
    already_visible_at: Option<Instant>,
) -> anyhow::Result<HeadlessGpuCandidateAttempt> {
    execute_headless_gpu_candidate_after_completion_drain(
        preview_service,
        state,
        gpu_adapter,
        observer,
        gpu_completion_deadline,
        already_visible_at,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_headless_gpu_candidate_after_completion_drain<O: HeadlessGpuExecutionObserver>(
    preview_service: &HeadlessPreviewRuntime,
    state: &mut AppState,
    gpu_adapter: &mut HeadlessViewerGpuAdapter,
    observer: &mut O,
    gpu_completion_deadline: HeadlessGpuCompletionDeadline,
    already_visible_at: Option<Instant>,
    may_continue_after_completion: bool,
) -> anyhow::Result<HeadlessGpuCandidateAttempt> {
    let submission_was_in_flight = gpu_adapter.has_submission_in_flight();
    let candidate = if already_visible_at.is_some() {
        present_headless_preview_candidate_at(
            preview_service,
            state,
            gpu_adapter,
            gpu_completion_deadline,
            already_visible_at,
        )?
    } else {
        present_headless_preview_candidate(
            preview_service,
            state,
            gpu_adapter,
            gpu_completion_deadline,
        )?
    };
    match candidate {
        HeadlessPreviewCandidate::Ready { output, completed_demand } => {
            let (status, binding, output_binding) = match output {
                HeadlessPresentedOutput::Gpu { execution } => {
                    let output_key = preview_service.registered_gpu_output_key().context(
                        "published Headless GPU execution omitted its Runtime output binding",
                    )?;
                    observer.execution_completed(
                        *execution,
                        HeadlessGpuExecutionDisposition::PublishedCurrent,
                        completed_demand,
                    );
                    (
                        HeadlessGpuCandidateStatus::Ready,
                        HeadlessGpuCandidateBindingUpdate::SatisfiedIntent,
                        HeadlessGpuCandidateOutputBindingUpdate::Gpu(output_key),
                    )
                }
                HeadlessPresentedOutput::QueuedGpu => {
                    let output_key = preview_service.registered_gpu_output_key().context(
                        "queue-published Headless GPU output omitted its Runtime binding",
                    )?;
                    (
                        HeadlessGpuCandidateStatus::QueuedReady,
                        HeadlessGpuCandidateBindingUpdate::SatisfiedIntent,
                        HeadlessGpuCandidateOutputBindingUpdate::Gpu(output_key),
                    )
                }
                HeadlessPresentedOutput::CurrentGpu => {
                    let output_key = preview_service
                        .registered_gpu_output_key()
                        .context("current Headless GPU output omitted its Runtime binding")?;
                    observer.current_output_presented(completed_demand);
                    if gpu_adapter.has_submission_in_flight() {
                        (
                            HeadlessGpuCandidateStatus::QueuedReady,
                            HeadlessGpuCandidateBindingUpdate::SatisfiedIntent,
                            HeadlessGpuCandidateOutputBindingUpdate::Gpu(output_key),
                        )
                    } else {
                        (
                            HeadlessGpuCandidateStatus::Ready,
                            HeadlessGpuCandidateBindingUpdate::SatisfiedIntent,
                            HeadlessGpuCandidateOutputBindingUpdate::Gpu(output_key),
                        )
                    }
                }
                HeadlessPresentedOutput::Transparent | HeadlessPresentedOutput::Raster(_) => (
                    HeadlessGpuCandidateStatus::Ready,
                    HeadlessGpuCandidateBindingUpdate::SatisfiedIntent,
                    HeadlessGpuCandidateOutputBindingUpdate::NonGpu,
                ),
            };
            Ok(HeadlessGpuCandidateAttempt { status, binding, output_binding })
        }
        HeadlessPreviewCandidate::CompletedGpu { execution, disposition } => {
            let (publication, completed_demand) = match disposition {
                HeadlessCompletedGpuDisposition::PublishedCurrent { completed_demand } => (
                    HeadlessGpuExecutionDisposition::PublishedCurrent,
                    completed_demand,
                ),
                HeadlessCompletedGpuDisposition::PreparedSuccessor => {
                    (HeadlessGpuExecutionDisposition::PreparedSuccessor, None)
                }
                HeadlessCompletedGpuDisposition::Released => {
                    (HeadlessGpuExecutionDisposition::Released, None)
                }
                HeadlessCompletedGpuDisposition::TerminalDelivery(kind) => (
                    HeadlessGpuExecutionDisposition::TerminalRejected(kind),
                    None,
                ),
            };
            observer.execution_completed(*execution, publication, completed_demand);
            if may_continue_after_completion {
                return execute_headless_gpu_candidate_after_completion_drain(
                    preview_service,
                    state,
                    gpu_adapter,
                    observer,
                    gpu_completion_deadline,
                    already_visible_at,
                    false,
                );
            }
            Ok(HeadlessGpuCandidateAttempt {
                status: HeadlessGpuCandidateStatus::Loading,
                binding: HeadlessGpuCandidateBindingUpdate::RetryCurrentIntent,
                output_binding: HeadlessGpuCandidateOutputBindingUpdate::Preserve,
            })
        }
        HeadlessPreviewCandidate::Loading => {
            let submission_is_in_flight = gpu_adapter.has_submission_in_flight();
            Ok(HeadlessGpuCandidateAttempt {
                status: if submission_is_in_flight {
                    HeadlessGpuCandidateStatus::InFlight
                } else {
                    HeadlessGpuCandidateStatus::Loading
                },
                binding: if submission_is_in_flight && submission_was_in_flight {
                    HeadlessGpuCandidateBindingUpdate::Preserve
                } else {
                    HeadlessGpuCandidateBindingUpdate::AttemptedIntent
                },
                output_binding: HeadlessGpuCandidateOutputBindingUpdate::Preserve,
            })
        }
        HeadlessPreviewCandidate::Backpressured => Ok(HeadlessGpuCandidateAttempt {
            status: HeadlessGpuCandidateStatus::Backpressured,
            binding: HeadlessGpuCandidateBindingUpdate::AttemptedIntent,
            output_binding: HeadlessGpuCandidateOutputBindingUpdate::Preserve,
        }),
        HeadlessPreviewCandidate::DroppedLate => Ok(HeadlessGpuCandidateAttempt {
            status: HeadlessGpuCandidateStatus::DroppedLate,
            binding: HeadlessGpuCandidateBindingUpdate::AttemptedIntent,
            output_binding: HeadlessGpuCandidateOutputBindingUpdate::Preserve,
        }),
        HeadlessPreviewCandidate::Unavailable(_) => Ok(HeadlessGpuCandidateAttempt {
            status: HeadlessGpuCandidateStatus::Unavailable,
            binding: HeadlessGpuCandidateBindingUpdate::AttemptedIntent,
            output_binding: HeadlessGpuCandidateOutputBindingUpdate::Preserve,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_gpu_output_is_an_explicit_usable_binding() {
        let preview = HeadlessPreviewRuntime::new();
        assert!(headless_candidate_output_binding_matches(
            Some(&HeadlessGpuCandidateOutputBinding::NonGpu),
            &preview,
        ));
        assert!(!headless_candidate_output_binding_matches(None, &preview));
    }

    #[test]
    fn preview_backlog_never_waits_before_the_next_bounded_drain() {
        let now = Instant::now();
        assert_eq!(
            headless_preview_wait_budget(now + Duration::from_secs(1), now, true),
            None
        );
        assert_eq!(headless_preview_wait_budget(now, now, false), None);
        assert_eq!(
            headless_preview_wait_budget(now + Duration::from_secs(1), now, false),
            Some(HEADLESS_PREVIEW_CLOCK_TICK_MAX_WAIT)
        );
    }

    #[test]
    fn video_only_interval_does_not_fabricate_an_audio_stage_observation() {
        let mut state = AppState::new();
        let mut timing = HeadlessRealtimeIntervalTiming::default();

        pump_headless_realtime_audio(&mut state, false, &mut timing)
            .expect("video-only pre-clock stage");

        assert_eq!(timing.audio_pump.observations, 0);
    }

    #[test]
    fn terminal_owner_projection_clears_only_proven_closed_domains() {
        let running = HeadlessEnduranceOwnerSnapshot {
            playback_pending: 1,
            other_queue_depth: 4,
            owned_resource_units: 9,
            gpu_device_losses: 2,
            gpu_fatal_errors: 1,
            other_fatal_errors: 2,
            app_background: AppBackgroundEnduranceSnapshot::default(),
            fatal_error_total: 3,
            owner_capture_failed: false,
        };

        let closed = running
            .after_shutdown(HeadlessEnduranceShutdownProjection {
                playback_owner_consumed: true,
                preview_closed: true,
                audio_closed: true,
                app_residual_owners_closed: true,
                app_background: Some(AppBackgroundEnduranceSnapshot::default()),
                gpu_closed: true,
                gpu_device_losses: 2,
                gpu_fatal_errors: 1,
                transport_shutdown_failed: false,
            })
            .expect("clean terminal projection");
        assert_eq!(closed.playback_pending, 0);
        assert_eq!(closed.other_queue_depth, 0);
        assert_eq!(closed.owned_resource_units, 0);
        assert_eq!(closed.gpu_device_losses, 2);
        assert_eq!(closed.fatal_errors(), 3);

        let playback_retained = running
            .after_shutdown(HeadlessEnduranceShutdownProjection {
                playback_owner_consumed: false,
                preview_closed: true,
                audio_closed: true,
                app_residual_owners_closed: true,
                app_background: Some(AppBackgroundEnduranceSnapshot::default()),
                gpu_closed: true,
                gpu_device_losses: 2,
                gpu_fatal_errors: 1,
                transport_shutdown_failed: false,
            })
            .expect("retained terminal projection");
        assert_eq!(playback_retained.playback_pending, 1);
        assert_eq!(playback_retained.other_queue_depth, 4);
        assert_eq!(playback_retained.owned_resource_units, 9);
        assert_eq!(playback_retained.fatal_errors(), 4);

        let incomplete = running
            .after_shutdown(HeadlessEnduranceShutdownProjection {
                playback_owner_consumed: true,
                preview_closed: false,
                audio_closed: true,
                app_residual_owners_closed: false,
                app_background: Some(AppBackgroundEnduranceSnapshot::default()),
                gpu_closed: false,
                gpu_device_losses: 4,
                gpu_fatal_errors: 2,
                transport_shutdown_failed: true,
            })
            .expect("incomplete terminal projection");
        assert_eq!(incomplete.playback_pending, 1);
        assert_eq!(incomplete.other_queue_depth, 4);
        assert_eq!(incomplete.owned_resource_units, 9);
        assert_eq!(incomplete.gpu_device_losses, 4);
        assert_eq!(incomplete.fatal_errors(), 8);
    }

    #[test]
    fn failed_terminal_projection_preserves_running_counters_and_fails_closed() {
        let running_background = AppBackgroundEnduranceSnapshot {
            media_import: AppBackgroundDomainSnapshot {
                queue_depth: 5,
                owned_resource_units: 6,
                cumulative_failures: 7,
                worker_health_failures: 8,
            },
            ..AppBackgroundEnduranceSnapshot::default()
        };
        let running = HeadlessEnduranceOwnerSnapshot {
            playback_pending: 0,
            other_queue_depth: 4,
            owned_resource_units: 9,
            gpu_device_losses: 2,
            gpu_fatal_errors: 1,
            other_fatal_errors: 2,
            app_background: running_background,
            fatal_error_total: 18,
            owner_capture_failed: false,
        };

        let failed = running.fail_closed_after_shutdown(HeadlessEnduranceShutdownProjection {
            playback_owner_consumed: true,
            preview_closed: true,
            audio_closed: true,
            app_residual_owners_closed: true,
            app_background: None,
            gpu_closed: true,
            gpu_device_losses: 3,
            gpu_fatal_errors: 1,
            transport_shutdown_failed: false,
        });

        assert_eq!(failed.playback_pending, 1);
        assert_eq!(failed.other_queue_depth, 4);
        assert_eq!(failed.owned_resource_units, 9);
        assert_eq!(failed.gpu_device_losses, 3);
        assert_eq!(failed.app_background, running_background);
        assert_eq!(failed.fatal_error_total, 19);
    }

    #[test]
    fn failed_terminal_projection_saturates_when_exact_projection_overflows() {
        let running_background = AppBackgroundEnduranceSnapshot {
            audio_idle_warmup: AppBackgroundDomainSnapshot {
                cumulative_failures: u64::MAX,
                ..AppBackgroundDomainSnapshot::default()
            },
            media_import: AppBackgroundDomainSnapshot {
                worker_health_failures: 1,
                ..AppBackgroundDomainSnapshot::default()
            },
            ..AppBackgroundEnduranceSnapshot::default()
        };
        let running = HeadlessEnduranceOwnerSnapshot {
            playback_pending: 0,
            other_queue_depth: 0,
            owned_resource_units: 0,
            gpu_device_losses: 0,
            gpu_fatal_errors: 0,
            other_fatal_errors: 0,
            app_background: running_background,
            fatal_error_total: u64::MAX,
            owner_capture_failed: false,
        };
        let projection = HeadlessEnduranceShutdownProjection {
            playback_owner_consumed: true,
            preview_closed: true,
            audio_closed: true,
            app_residual_owners_closed: true,
            app_background: Some(AppBackgroundEnduranceSnapshot::default()),
            gpu_closed: true,
            gpu_device_losses: 0,
            gpu_fatal_errors: 0,
            transport_shutdown_failed: false,
        };

        assert!(running.after_shutdown(projection).is_err());
        let failed = running.fail_closed_after_shutdown(projection);
        assert_eq!(failed.playback_pending, 1);
        assert_eq!(failed.other_queue_depth, 1);
        assert_eq!(failed.owned_resource_units, 1);
        assert_eq!(failed.fatal_error_total, u64::MAX);
        assert_eq!(failed.app_background, running_background);
    }

    #[test]
    fn owner_projection_counts_worker_backlog_pins_and_monotonic_failures() {
        let mut preview = crate::app::preview_runtime::PreviewDiagnostics::default();
        preview.scheduler.pending_requests = 1;
        preview.worker_queue.queued_jobs = 2;
        preview.worker_queue.in_flight_jobs = 3;
        preview.frame_store.media_aggregate_entries = 4;
        preview.frame_store.media_aggregate_resource_units = 5;
        preview.frame_store.viewer_entries = 6;
        preview.frame_store.pinned_viewer_bytes = 1;
        preview.visual_program_cache.entries = 7;
        preview.worker_disconnected_drops = 8;

        let mut audio = mondrian_media::AudioPlaybackSnapshot::execution_unavailable();
        audio.in_flight = 9;
        audio.render_substitution_count = 1;
        audio.render_generation_recovery_count = 2;
        audio.underrun_recovery_count = 3;
        audio.output_lifecycle.backend_loss_count = 4;
        audio.output_lifecycle.deactivation_failed_count = 5;

        let projected = project_headless_endurance_owner_snapshot(
            preview,
            HeadlessViewerGpuEnduranceSnapshot::test_fixture(1, 2, 3, 4, 5),
            audio,
            true,
            AppBackgroundEnduranceSnapshot::default(),
        )
        .expect("valid owner projection");

        assert_eq!(projected.playback_pending(), 1);
        assert_eq!(projected.other_queue_depth(), 15);
        assert_eq!(projected.owned_resource_units(), 38);
        assert_eq!(projected.gpu_device_losses(), 4);
        assert_eq!(projected.fatal_errors(), 29);
    }
}
