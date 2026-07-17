//! Semantic frame-work value types shared by the Playback Engine and Broker.

use crate::FrameDemandIdentity;
use std::time::Duration;

/// Semantic class of frame-producing work at the Playback seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// Atomic reason why one in-flight execution should stop producing visible work.
///
/// The Frame Work Broker derives this under the same lifecycle lock that owns
/// pending bindings and execution leases. Adapters must not reconstruct the
/// decision from separate freshness and competing-work queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameExecutionCancellation {
    /// The Broker is closed and no execution may publish.
    BrokerClosed,
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
}

impl FrameExecutionCancellation {
    /// Return the cancellation request age carried by Broker evidence.
    pub const fn request_age(self) -> Option<Duration> {
        match self {
            Self::BrokerClosed => None,
            Self::Superseded { age } => age,
            Self::PrefetchPreemptedByCurrent { request_age }
            | Self::StillPreemptedByRealtimeCurrent { request_age } => Some(request_age),
        }
    }
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
    /// Adapter-owned deadline; the Playback Module carries but never compares it.
    pub deadline: Option<D>,
}

/// Atomic completion decision plus the binding it is allowed to satisfy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRequestResolution<D> {
    /// Visibility/cache freshness classification.
    pub completion: FrameRequestCompletion,
    /// Exact latest binding satisfied by a current reusable completion.
    pub binding: Option<FrameRequestBinding<D>>,
}
