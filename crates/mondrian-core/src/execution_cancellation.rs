//! Monotonic cooperative cancellation shared across execution boundaries.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Cloneable authority for one immutable execution generation.
///
/// Cancellation is monotonic. Callers create a new token for a new generation;
/// resetting and reusing a canceled token is intentionally unsupported.
#[derive(Debug, Clone, Default)]
pub struct ExecutionCancellationToken {
    canceled: Arc<AtomicBool>,
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

    /// Return whether the owning execution generation has been invalidated.
    pub fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::Acquire)
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
}
