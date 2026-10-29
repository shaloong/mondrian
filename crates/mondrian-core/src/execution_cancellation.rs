//! Monotonic cooperative cancellation shared across execution boundaries.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Cloneable authority for one immutable execution generation.
///
/// Cancellation is monotonic. Callers create a new token for a new generation;
/// resetting and reusing a canceled token is intentionally unsupported.
#[derive(Debug, Clone, Default)]
pub struct ExecutionCancellationToken {
    canceled: Arc<AtomicBool>,
    deadline: Option<Instant>,
}

impl ExecutionCancellationToken {
    /// Create one live execution token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Invalidate this token and every clone held by downstream Adapters.
    pub fn cancel(&self) {
        self.canceled.store(true, Ordering::Release);
    }

    /// Derive a downstream scope with the same cancellation authority and an
    /// absolute deadline that may only become earlier. Expiring this scope
    /// does not invalidate unrelated work in its parent generation.
    pub fn with_deadline(&self, deadline: Instant) -> Self {
        Self {
            canceled: Arc::clone(&self.canceled),
            deadline: Some(self.deadline.map_or(deadline, |parent| parent.min(deadline))),
        }
    }

    /// Exact scoped deadline, if any; cloning never renews it.
    pub const fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Return whether this scope expired or its shared generation was canceled.
    pub fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::Acquire)
            || self.deadline.is_some_and(|deadline| Instant::now() >= deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_shared_and_monotonic() {
        let token = ExecutionCancellationToken::new();
        let clone = token.clone();
        assert!(!clone.is_canceled());
        token.cancel();
        assert!(clone.is_canceled());
        assert!(!ExecutionCancellationToken::new().is_canceled());
    }

    #[test]
    fn downstream_deadline_never_renews_or_invalidates_parent_generation() {
        let root = ExecutionCancellationToken::new();
        let expired = root.with_deadline(Instant::now());
        assert!(expired.is_canceled());
        assert!(
            !root.is_canceled(),
            "scoped expiry is not generation cancellation"
        );
        let later = expired.with_deadline(Instant::now() + std::time::Duration::from_secs(60));
        assert_eq!(later.deadline(), expired.deadline());
        assert!(later.is_canceled());
        assert_eq!(later.clone().deadline(), expired.deadline());
    }

    #[test]
    fn explicit_cancel_still_reaches_all_deadline_scopes() {
        let root = ExecutionCancellationToken::new();
        let scope = root.with_deadline(Instant::now() + std::time::Duration::from_secs(60));
        let nested = scope.with_deadline(Instant::now() + std::time::Duration::from_secs(120));
        assert!(!nested.is_canceled());
        scope.cancel();
        assert!(root.is_canceled());
        assert!(nested.is_canceled());
    }
}
