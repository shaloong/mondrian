//! UI-independent frame-work cancellation evidence and acceptance policy.

use crate::FrameWorkClass;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Stable semantic reason why frame-producing work terminated cooperatively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FrameCancellationCause {
    /// The execution owner is shutting down.
    Shutdown,
    /// A newer generation or binding superseded the execution.
    Superseded,
    /// Speculative playback work exhausted its usefulness deadline.
    PrefetchDeadline,
    /// Current playback work missed its presentation deadline.
    PlaybackDeadline,
    /// Speculative work yielded to visible current work.
    PrefetchPreemptedByCurrent,
    /// Deterministic still work yielded to realtime current work.
    StillPreemptedByRealtimeCurrent,
    /// The Adapter returned cancellation without attributable authority.
    Unknown,
}

/// One completed cooperative-cancellation observation at the Playback seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameCancellationObservation {
    /// Semantic work class of the canceled execution.
    pub work_class: FrameWorkClass,
    /// Authoritative cancellation cause projected by the execution Adapter.
    pub cause: FrameCancellationCause,
    /// Total worker execution lifetime before returning the canceled outcome.
    pub execution_duration: Duration,
    /// Worker-start to `LogicalCancellationObserved`.
    ///
    /// This is scheduler/observer evidence, never a claim that FFmpeg or any
    /// other concrete media checkpoint has already run.
    pub execution_to_logical_cancellation: Option<Duration>,
    /// Authority-request to `LogicalCancellationObserved` latency.
    pub request_to_logical_cancellation: Option<Duration>,
}

/// Exact streaming timing aggregate in microseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameCancellationTiming {
    /// Observations contributing to this timing.
    pub samples: u64,
    /// Saturating total across all observations.
    pub total_us: u64,
    /// Exact maximum across all observations.
    pub max_us: u64,
    /// Most recently observed value.
    pub last_us: u64,
}

impl FrameCancellationTiming {
    fn observe(&mut self, value: Duration) {
        let value_us = duration_us(value);
        self.samples = self.samples.saturating_add(1);
        self.total_us = self.total_us.saturating_add(value_us);
        self.max_us = self.max_us.max(value_us);
        self.last_us = value_us;
    }
}

/// Streaming evidence for one semantic frame-work class or the all-class rollup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameCancellationProfile {
    /// Completed canceled executions.
    pub cancellations: u64,
    /// Shutdown cancellations.
    pub shutdown: u64,
    /// Generation/binding supersession cancellations.
    pub superseded: u64,
    /// Speculative playback deadline cancellations.
    pub prefetch_deadline: u64,
    /// Current playback deadline cancellations.
    pub playback_deadline: u64,
    /// Speculative work preempted by current work.
    pub prefetch_preempted_by_current: u64,
    /// Still work preempted by realtime current work.
    pub still_preempted_by_realtime_current: u64,
    /// Unattributed cancellations.
    pub unknown: u64,
    /// Total worker lifetime of canceled executions.
    pub execution: FrameCancellationTiming,
    /// Worker start to `LogicalCancellationObserved`.
    pub execution_to_logical_cancellation: FrameCancellationTiming,
    /// Cancellation-authority request to `LogicalCancellationObserved`.
    pub request_to_logical_cancellation: FrameCancellationTiming,
    /// `LogicalCancellationObserved` to worker return.
    pub logical_cancellation_to_return: FrameCancellationTiming,
    /// Observations whose logical-cancellation ordering is mathematically impossible.
    pub invalid_timing_order: u64,
}

impl FrameCancellationProfile {
    fn observe(&mut self, observation: FrameCancellationObservation) {
        self.cancellations = self.cancellations.saturating_add(1);
        let cause = match observation.cause {
            FrameCancellationCause::Shutdown => &mut self.shutdown,
            FrameCancellationCause::Superseded => &mut self.superseded,
            FrameCancellationCause::PrefetchDeadline => &mut self.prefetch_deadline,
            FrameCancellationCause::PlaybackDeadline => &mut self.playback_deadline,
            FrameCancellationCause::PrefetchPreemptedByCurrent => {
                &mut self.prefetch_preempted_by_current
            }
            FrameCancellationCause::StillPreemptedByRealtimeCurrent => {
                &mut self.still_preempted_by_realtime_current
            }
            FrameCancellationCause::Unknown => &mut self.unknown,
        };
        *cause = cause.saturating_add(1);
        self.execution.observe(observation.execution_duration);
        if let Some(execution_to_logical_cancellation) =
            observation.execution_to_logical_cancellation
        {
            self.execution_to_logical_cancellation
                .observe(execution_to_logical_cancellation);
        }
        if let Some(request_to_logical_cancellation) = observation.request_to_logical_cancellation {
            self.request_to_logical_cancellation.observe(request_to_logical_cancellation);
        }
        let invalid_timing_order = observation
            .execution_to_logical_cancellation
            .is_some_and(|observed| observed > observation.execution_duration)
            || observation
                .execution_to_logical_cancellation
                .zip(observation.request_to_logical_cancellation)
                .is_some_and(|(execution_age, request_age)| request_age > execution_age);
        if invalid_timing_order {
            self.invalid_timing_order = self.invalid_timing_order.saturating_add(1);
        }
        let logical_cancellation_to_return = observation
            .execution_to_logical_cancellation
            .map(|observed| observation.execution_duration.saturating_sub(observed))
            .unwrap_or(observation.execution_duration);
        self.logical_cancellation_to_return.observe(logical_cancellation_to_return);
    }
}

/// Immutable cancellation evidence shared by production and Headless Adapters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameCancellationEvidenceReport {
    /// All work classes combined.
    pub all: FrameCancellationProfile,
    /// Playback-current and playback-prefetch work.
    pub playback: FrameCancellationProfile,
    /// Latest-wins scrub/jog/shuttle work.
    pub interactive: FrameCancellationProfile,
    /// Deterministic random-access still work.
    pub still: FrameCancellationProfile,
}

impl FrameCancellationEvidenceReport {
    /// Return the profile for one semantic work class.
    pub const fn profile(self, work_class: FrameWorkClass) -> FrameCancellationProfile {
        match work_class {
            FrameWorkClass::Playback => self.playback,
            FrameWorkClass::Interactive => self.interactive,
            FrameWorkClass::Still => self.still,
        }
    }
}

/// Deep Module aggregating cancellation evidence independently of UI and media codecs.
#[derive(Debug, Default)]
pub struct FrameCancellationEvidenceCollector {
    report: FrameCancellationEvidenceReport,
}

impl FrameCancellationEvidenceCollector {
    /// Record one completed cooperative cancellation.
    pub fn observe(&mut self, observation: FrameCancellationObservation) {
        self.report.all.observe(observation);
        match observation.work_class {
            FrameWorkClass::Playback => self.report.playback.observe(observation),
            FrameWorkClass::Interactive => self.report.interactive.observe(observation),
            FrameWorkClass::Still => self.report.still.observe(observation),
        }
    }

    /// Return the current exact streaming aggregate.
    pub const fn report(&self) -> FrameCancellationEvidenceReport {
        self.report
    }
}

/// Product cancellation budgets used by production and Headless acceptance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameCancellationPolicy {
    /// Maximum authority-request to `LogicalCancellationObserved` latency.
    pub max_request_to_logical_cancellation: Duration,
    /// Maximum logical-cancellation-to-return latency for playback work.
    pub max_playback_logical_cancellation_to_return: Duration,
    /// Maximum logical-cancellation-to-return latency for interactive work.
    pub max_interactive_logical_cancellation_to_return: Duration,
    /// Maximum logical-cancellation-to-return latency for deterministic still work.
    pub max_still_logical_cancellation_to_return: Duration,
}

impl Default for FrameCancellationPolicy {
    fn default() -> Self {
        Self {
            max_request_to_logical_cancellation: Duration::from_millis(5),
            max_playback_logical_cancellation_to_return: Duration::from_millis(50),
            max_interactive_logical_cancellation_to_return: Duration::from_millis(50),
            max_still_logical_cancellation_to_return: Duration::from_millis(500),
        }
    }
}

/// Stable cancellation acceptance failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameCancellationGateFailureKind {
    /// A cancellation returned without a structured authority.
    UnknownCause,
    /// A structured cancellation lacked request-age evidence.
    MissingRequestToLogicalCancellation,
    /// A structured cancellation lacked logical-cancellation execution timing.
    MissingExecutionToLogicalCancellation,
    /// A sample violates execution/logical-cancellation/request temporal ordering.
    InvalidTimingOrder,
    /// A worker observed the authority request too late.
    RequestToLogicalCancellationExceeded,
    /// Cleanup after logical cancellation returned too late.
    LogicalCancellationToReturnExceeded,
}

/// One fail-closed cancellation acceptance failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameCancellationGateFailure {
    /// Work class containing the violation.
    pub work_class: FrameWorkClass,
    /// Stable violation class.
    pub kind: FrameCancellationGateFailureKind,
    /// Observed count or latency, depending on `kind`.
    pub observed: u64,
    /// Allowed count or latency, depending on `kind`.
    pub limit: u64,
}

/// Versioned-shape cancellation gate result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameCancellationGateReport {
    /// Stable standalone report schema.
    pub schema_version: u32,
    /// Whether all observed cancellation classes satisfy policy.
    pub passed: bool,
    /// Ordered failures by work class and policy check.
    pub failures: Vec<FrameCancellationGateFailure>,
}

/// Evaluate exact cancellation evidence without UI or codec interpretation.
pub fn evaluate_frame_cancellation(
    evidence: FrameCancellationEvidenceReport,
    policy: FrameCancellationPolicy,
) -> FrameCancellationGateReport {
    let mut failures = Vec::new();
    for work_class in [
        FrameWorkClass::Playback,
        FrameWorkClass::Interactive,
        FrameWorkClass::Still,
    ] {
        let profile = evidence.profile(work_class);
        if profile.unknown > 0 {
            failures.push(FrameCancellationGateFailure {
                work_class,
                kind: FrameCancellationGateFailureKind::UnknownCause,
                observed: profile.unknown,
                limit: 0,
            });
        }
        let structured = profile.cancellations.saturating_sub(profile.unknown);
        let unattributed =
            structured.saturating_sub(profile.request_to_logical_cancellation.samples);
        if unattributed > 0 {
            failures.push(FrameCancellationGateFailure {
                work_class,
                kind: FrameCancellationGateFailureKind::MissingRequestToLogicalCancellation,
                observed: unattributed,
                limit: 0,
            });
        }
        let missing_logical_cancellation =
            structured.saturating_sub(profile.execution_to_logical_cancellation.samples);
        if missing_logical_cancellation > 0 {
            failures.push(FrameCancellationGateFailure {
                work_class,
                kind: FrameCancellationGateFailureKind::MissingExecutionToLogicalCancellation,
                observed: missing_logical_cancellation,
                limit: 0,
            });
        }
        if profile.invalid_timing_order > 0 {
            failures.push(FrameCancellationGateFailure {
                work_class,
                kind: FrameCancellationGateFailureKind::InvalidTimingOrder,
                observed: profile.invalid_timing_order,
                limit: 0,
            });
        }
        let request_limit = duration_us(policy.max_request_to_logical_cancellation);
        if profile.request_to_logical_cancellation.max_us > request_limit {
            failures.push(FrameCancellationGateFailure {
                work_class,
                kind: FrameCancellationGateFailureKind::RequestToLogicalCancellationExceeded,
                observed: profile.request_to_logical_cancellation.max_us,
                limit: request_limit,
            });
        }
        let return_limit = duration_us(match work_class {
            FrameWorkClass::Playback => policy.max_playback_logical_cancellation_to_return,
            FrameWorkClass::Interactive => policy.max_interactive_logical_cancellation_to_return,
            FrameWorkClass::Still => policy.max_still_logical_cancellation_to_return,
        });
        if profile.logical_cancellation_to_return.max_us > return_limit {
            failures.push(FrameCancellationGateFailure {
                work_class,
                kind: FrameCancellationGateFailureKind::LogicalCancellationToReturnExceeded,
                observed: profile.logical_cancellation_to_return.max_us,
                limit: return_limit,
            });
        }
    }
    FrameCancellationGateReport {
        schema_version: FRAME_CANCELLATION_GATE_REPORT_SCHEMA_VERSION,
        passed: failures.is_empty(),
        failures,
    }
}

/// Current standalone cancellation-gate report schema.
pub const FRAME_CANCELLATION_GATE_REPORT_SCHEMA_VERSION: u32 = 2;

fn duration_us(value: Duration) -> u64 {
    value.as_micros().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collector_derives_logical_cancellation_to_return_and_keeps_class_locality() {
        let mut collector = FrameCancellationEvidenceCollector::default();
        collector.observe(FrameCancellationObservation {
            work_class: FrameWorkClass::Interactive,
            cause: FrameCancellationCause::Superseded,
            execution_duration: Duration::from_millis(40),
            execution_to_logical_cancellation: Some(Duration::from_millis(7)),
            request_to_logical_cancellation: Some(Duration::from_millis(2)),
        });

        let report = collector.report();
        assert_eq!(report.all.cancellations, 1);
        assert_eq!(report.interactive.superseded, 1);
        assert_eq!(report.interactive.execution.max_us, 40_000);
        assert_eq!(
            report.interactive.execution_to_logical_cancellation.max_us,
            7_000
        );
        assert_eq!(
            report.interactive.request_to_logical_cancellation.max_us,
            2_000
        );
        assert_eq!(
            report.interactive.logical_cancellation_to_return.max_us,
            33_000
        );
        assert_eq!(report.playback.cancellations, 0);
    }

    #[test]
    fn gate_applies_strict_realtime_and_distinct_still_return_budgets() {
        let mut collector = FrameCancellationEvidenceCollector::default();
        for work_class in [
            FrameWorkClass::Playback,
            FrameWorkClass::Interactive,
            FrameWorkClass::Still,
        ] {
            collector.observe(FrameCancellationObservation {
                work_class,
                cause: FrameCancellationCause::Superseded,
                execution_duration: Duration::from_micros(51_001),
                execution_to_logical_cancellation: Some(Duration::from_millis(1)),
                request_to_logical_cancellation: Some(Duration::from_millis(1)),
            });
        }

        let gate =
            evaluate_frame_cancellation(collector.report(), FrameCancellationPolicy::default());
        assert!(!gate.passed);
        assert_eq!(gate.failures.len(), 2);
        assert_eq!(gate.failures[0].work_class, FrameWorkClass::Playback);
        assert_eq!(
            gate.failures[0].kind,
            FrameCancellationGateFailureKind::LogicalCancellationToReturnExceeded
        );
        assert_eq!(gate.failures[0].observed, 50_001);
        assert_eq!(gate.failures[0].limit, 50_000);
        assert_eq!(gate.failures[1].work_class, FrameWorkClass::Interactive);
        assert_eq!(
            gate.failures[1].kind,
            FrameCancellationGateFailureKind::LogicalCancellationToReturnExceeded
        );
        assert_eq!(gate.failures[1].observed, 50_001);
        assert_eq!(gate.failures[1].limit, 50_000);
    }

    #[test]
    fn cancellation_return_budgets_have_exact_inclusive_boundaries() {
        let evaluate = |work_class: FrameWorkClass, return_latency: Duration| {
            let checkpoint = Duration::from_millis(1);
            let mut collector = FrameCancellationEvidenceCollector::default();
            collector.observe(FrameCancellationObservation {
                work_class,
                cause: FrameCancellationCause::Superseded,
                execution_duration: checkpoint.saturating_add(return_latency),
                execution_to_logical_cancellation: Some(checkpoint),
                request_to_logical_cancellation: Some(checkpoint),
            });
            evaluate_frame_cancellation(collector.report(), FrameCancellationPolicy::default())
        };

        for work_class in [FrameWorkClass::Playback, FrameWorkClass::Interactive] {
            assert!(evaluate(work_class, Duration::from_millis(50)).passed);
            let exceeded = evaluate(work_class, Duration::from_micros(50_001));
            assert!(!exceeded.passed);
            assert!(exceeded.failures.iter().any(|failure| {
                failure.work_class == work_class
                    && failure.kind
                        == FrameCancellationGateFailureKind::LogicalCancellationToReturnExceeded
                    && failure.observed == 50_001
                    && failure.limit == 50_000
            }));
        }

        assert!(evaluate(FrameWorkClass::Still, Duration::from_millis(500)).passed);
        let exceeded = evaluate(FrameWorkClass::Still, Duration::from_micros(500_001));
        assert!(!exceeded.passed);
        assert!(exceeded.failures.iter().any(|failure| {
            failure.work_class == FrameWorkClass::Still
                && failure.kind
                    == FrameCancellationGateFailureKind::LogicalCancellationToReturnExceeded
                && failure.observed == 500_001
                && failure.limit == 500_000
        }));
    }

    #[test]
    fn gate_rejects_unknown_unattributed_and_late_logical_cancellation_evidence() {
        let mut collector = FrameCancellationEvidenceCollector::default();
        collector.observe(FrameCancellationObservation {
            work_class: FrameWorkClass::Interactive,
            cause: FrameCancellationCause::Unknown,
            execution_duration: Duration::from_millis(1),
            execution_to_logical_cancellation: None,
            request_to_logical_cancellation: None,
        });
        collector.observe(FrameCancellationObservation {
            work_class: FrameWorkClass::Playback,
            cause: FrameCancellationCause::PlaybackDeadline,
            execution_duration: Duration::from_millis(10),
            execution_to_logical_cancellation: Some(Duration::from_millis(9)),
            request_to_logical_cancellation: Some(Duration::from_millis(8)),
        });

        let gate =
            evaluate_frame_cancellation(collector.report(), FrameCancellationPolicy::default());
        assert!(gate
            .failures
            .iter()
            .any(|failure| { failure.kind == FrameCancellationGateFailureKind::UnknownCause }));
        assert!(gate.failures.iter().any(|failure| {
            failure.kind == FrameCancellationGateFailureKind::RequestToLogicalCancellationExceeded
        }));
    }

    #[test]
    fn gate_rejects_missing_or_impossible_logical_cancellation_timing() {
        let mut collector = FrameCancellationEvidenceCollector::default();
        collector.observe(FrameCancellationObservation {
            work_class: FrameWorkClass::Playback,
            cause: FrameCancellationCause::PlaybackDeadline,
            execution_duration: Duration::from_millis(10),
            execution_to_logical_cancellation: None,
            request_to_logical_cancellation: Some(Duration::from_millis(1)),
        });
        collector.observe(FrameCancellationObservation {
            work_class: FrameWorkClass::Interactive,
            cause: FrameCancellationCause::Superseded,
            execution_duration: Duration::from_millis(10),
            execution_to_logical_cancellation: Some(Duration::from_millis(2)),
            request_to_logical_cancellation: Some(Duration::from_millis(3)),
        });

        let gate =
            evaluate_frame_cancellation(collector.report(), FrameCancellationPolicy::default());

        assert!(gate.failures.iter().any(|failure| {
            failure.kind == FrameCancellationGateFailureKind::MissingExecutionToLogicalCancellation
        }));
        assert!(gate.failures.iter().any(|failure| {
            failure.kind == FrameCancellationGateFailureKind::InvalidTimingOrder
        }));
    }

    #[test]
    fn request_to_logical_cancellation_observation_has_exact_five_ms_boundary() {
        let evaluate = |latency: Duration| {
            let mut collector = FrameCancellationEvidenceCollector::default();
            collector.observe(FrameCancellationObservation {
                work_class: FrameWorkClass::Playback,
                cause: FrameCancellationCause::PrefetchDeadline,
                execution_duration: latency,
                execution_to_logical_cancellation: Some(latency),
                request_to_logical_cancellation: Some(latency),
            });
            evaluate_frame_cancellation(
                collector.report(),
                FrameCancellationPolicy {
                    max_playback_logical_cancellation_to_return: Duration::ZERO,
                    ..FrameCancellationPolicy::default()
                },
            )
        };

        assert!(evaluate(Duration::from_micros(4_999)).passed);
        assert!(evaluate(Duration::from_micros(5_000)).passed);
        let exceeded = evaluate(Duration::from_micros(5_001));
        assert!(!exceeded.passed);
        assert!(exceeded.failures.iter().any(|failure| {
            failure.kind == FrameCancellationGateFailureKind::RequestToLogicalCancellationExceeded
                && failure.observed == 5_001
                && failure.limit == 5_000
        }));
    }
}
