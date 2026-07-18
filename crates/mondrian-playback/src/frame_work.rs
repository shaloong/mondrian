//! Semantic frame-work value types shared by the Playback Engine and Broker.

use crate::FrameDemandIdentity;
use std::time::Duration;

/// Semantic class of frame-producing work at the Playback seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FrameWorkClass {
    /// Current playback cursor or forward playback prefetch.
    Playback,
    /// Latest-wins interactive playhead movement, jog, or shuttle work.
    Interactive,
    /// Deterministic one-off still extraction without realtime privilege.
    Still,
}

/// Admission priority for frame-producing work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameWorkPriority {
    /// Speculative work that must yield to visible work.
    Prefetch,
    /// Work required for the current visible position.
    Current,
}

/// One Adapter deadline lowered to a Broker-owned remaining-time budget.
///
/// The Adapter samples its own clock once immediately before submission and
/// supplies both the opaque absolute value needed by execution code and the
/// remaining duration to that same instant. The Broker compares only the
/// duration after converting it to its injected [`crate::MonotonicRuntimeClock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameWorkDeadline<D> {
    adapter_deadline: D,
    remaining_at_admission: Duration,
}

impl<D: Copy> FrameWorkDeadline<D> {
    /// Bind an opaque Adapter deadline to its remaining duration at admission.
    pub const fn from_remaining(adapter_deadline: D, remaining_at_admission: Duration) -> Self {
        Self { adapter_deadline, remaining_at_admission }
    }

    /// Return the opaque absolute deadline for the execution Adapter.
    pub const fn adapter_deadline(self) -> D {
        self.adapter_deadline
    }

    /// Return the remaining budget sampled immediately before admission.
    pub const fn remaining_at_admission(self) -> Duration {
        self.remaining_at_admission
    }
}

/// Freshness disposition for completed Adapter work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRequestCompletion {
    /// The completion satisfies the current request.
    Current,
    /// The completion may populate a cache but must not be presented as current.
    CacheOnly,
    /// The completion is obsolete and must not affect visible state.
    Stale,
}

/// Broker-owned deadline classification at the worker's completion timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameWorkDeadlineStatus {
    /// The latest binding has no execution deadline.
    NotApplicable,
    /// The worker completed strictly before the latest binding deadline.
    OnTime,
    /// The worker completed at or after the latest binding deadline.
    Missed {
        /// Elapsed time past the deadline; zero means completion at the boundary.
        late_by: Duration,
    },
}

impl FrameWorkDeadlineStatus {
    /// Return whether the worker missed the latest binding deadline.
    pub const fn is_missed(self) -> bool {
        matches!(self, Self::Missed { .. })
    }
}

/// Atomic reason why one in-flight execution should stop producing visible work.
///
/// The Frame Work Broker derives this under the same lifecycle lock that owns
/// pending bindings and execution leases. Adapters must not reconstruct the
/// decision from separate freshness and competing-work queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameExecutionCancellation {
    /// The Broker is closed and no execution may publish.
    BrokerClosed {
        /// Elapsed time since the Broker first closed.
        age: Duration,
    },
    /// The lease no longer matches the latest generation/binding.
    Superseded {
        /// Elapsed time since the Broker first observed invalidation.
        age: Option<Duration>,
    },
    /// Speculative work yielded to another current request.
    PrefetchPreemptedByCurrent {
        /// Age of the oldest competing current request.
        request_age: Duration,
    },
    /// Deterministic still work yielded to current realtime work.
    StillPreemptedByRealtimeCurrent {
        /// Age of the oldest competing realtime request.
        request_age: Duration,
    },
    /// The latest binding's lowered execution deadline has elapsed.
    DeadlineExpired {
        /// Elapsed time since the Broker-owned deadline instant.
        age: Duration,
    },
}

impl FrameExecutionCancellation {
    /// Return the cancellation request age carried by Broker evidence.
    pub const fn request_age(self) -> Option<Duration> {
        match self {
            Self::BrokerClosed { age } => Some(age),
            Self::Superseded { age } => age,
            Self::PrefetchPreemptedByCurrent { request_age }
            | Self::StillPreemptedByRealtimeCurrent { request_age } => Some(request_age),
            Self::DeadlineExpired { age } => Some(age),
        }
    }
}

/// One atomic Broker-clock sample of cancellation and execution age.
///
/// Adapters must use this evidence when comparing request-to-checkpoint and
/// execution-to-checkpoint durations. Reconstructing execution age from an
/// Adapter-local codec entry instant creates incompatible time origins during
/// dequeue/cancellation races.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameExecutionCancellationEvidence {
    /// Authoritative semantic cancellation reason and request age.
    pub cancellation: FrameExecutionCancellation,
    /// Age of the execution lease at the same Broker clock sample.
    pub execution_age: Duration,
}

impl FrameRequestCompletion {
    /// Return whether this completion may satisfy visible current-frame work.
    pub const fn is_current(self) -> bool {
        matches!(self, Self::Current)
    }

    /// Return whether an Adapter may retain the result in a semantic cache.
    pub const fn should_cache(self) -> bool {
        matches!(self, Self::Current | Self::CacheOnly)
    }
}

/// Latest semantic binding attached to one admitted request key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRequestBinding<D> {
    /// Latest-wins generation that owns this binding.
    pub generation: u64,
    /// Current or speculative admission priority.
    pub priority: FrameWorkPriority,
    /// Semantic work class used for worker eligibility.
    pub work_class: FrameWorkClass,
    /// Playback demand identity, when the request is demand-backed.
    pub demand_identity: Option<FrameDemandIdentity>,
    /// Opaque Adapter deadline associated with the Broker-compared budget.
    pub deadline: Option<D>,
}

/// Atomic completion decision plus the binding it is allowed to satisfy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRequestResolution<D> {
    /// Visibility/cache freshness classification.
    pub completion: FrameRequestCompletion,
    /// Exact latest binding satisfied by a current reusable completion.
    pub binding: Option<FrameRequestBinding<D>>,
    /// Deadline result at the Broker-recorded worker completion instant.
    pub deadline: FrameWorkDeadlineStatus,
}
