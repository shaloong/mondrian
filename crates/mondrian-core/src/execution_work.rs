//! Cross-domain execution intent and terminal evidence value types.
//!
//! These types deliberately do not define a scheduler. Realtime frame work,
//! background analysis, proxy generation, and export retain independent queue
//! and worker policies while sharing deadline and terminal-result semantics.

use std::time::Duration;

/// Cross-domain urgency used when execution evidence is compared or reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPriority {
    /// Must meet an active realtime presentation or consumption boundary.
    Realtime,
    /// Direct user request that should progress ahead of speculative work.
    UserInitiated,
    /// Cacheable or derived work with no active presentation deadline.
    Background,
    /// Opportunistic cleanup, indexing, or maintenance work.
    Maintenance,
}

/// One Adapter deadline lowered to an owning Module's remaining-time budget.
///
/// The Adapter samples its clock once immediately before admission and supplies
/// both the opaque absolute value needed by execution code and the remaining
/// duration at that same instant. The owning Module compares only the lowered
/// duration using its monotonic runtime clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionDeadline<D> {
    adapter_deadline: D,
    remaining_at_admission: Duration,
}

impl<D: Copy> ExecutionDeadline<D> {
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

/// Deadline classification at the owning Module's completion timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExecutionDeadlineStatus {
    /// This work has no execution deadline.
    NotApplicable,
    /// Execution completed strictly before its latest authoritative deadline.
    OnTime,
    /// Execution completed at or after its latest authoritative deadline.
    Missed {
        /// Elapsed time past the deadline; zero means completion at the boundary.
        late_by: Duration,
    },
}

impl ExecutionDeadlineStatus {
    /// Return whether execution missed its authoritative deadline.
    pub const fn is_missed(self) -> bool {
        matches!(self, Self::Missed { .. })
    }
}

/// Cross-domain terminal disposition for one admitted execution attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTerminalDisposition {
    /// The requested artifact or result completed successfully.
    Completed,
    /// Execution ran and failed with domain-owned structured detail.
    Failed,
    /// The owning generation requested cooperative cancellation.
    Canceled,
    /// A newer binding made this completion ineligible to publish.
    Superseded,
    /// Bounded admission rejected the attempt before execution began.
    Rejected,
}

/// Minimal terminal evidence shared across independent execution Modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionTerminalEvidence {
    /// Module-local monotonic generation that owned the attempt.
    pub generation: u64,
    /// Cross-domain urgency declared at admission.
    pub priority: ExecutionPriority,
    /// Why the attempt ended.
    pub disposition: ExecutionTerminalDisposition,
    /// Deadline result at the authoritative completion timestamp.
    pub deadline: ExecutionDeadlineStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_retains_one_adapter_sample_and_lowered_budget() {
        let deadline = ExecutionDeadline::from_remaining(42_u64, Duration::from_millis(7));
        assert_eq!(deadline.adapter_deadline(), 42);
        assert_eq!(deadline.remaining_at_admission(), Duration::from_millis(7));
    }

    #[test]
    fn terminal_evidence_keeps_scheduler_independent_semantics() {
        let evidence = ExecutionTerminalEvidence {
            generation: 3,
            priority: ExecutionPriority::Background,
            disposition: ExecutionTerminalDisposition::Superseded,
            deadline: ExecutionDeadlineStatus::NotApplicable,
        };
        assert_eq!(evidence.generation, 3);
        assert!(!evidence.deadline.is_missed());
    }
}
