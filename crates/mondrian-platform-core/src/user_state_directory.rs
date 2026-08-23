//! Platform-neutral stable per-user state-directory contract.

use std::path::PathBuf;

use crate::NoopPlatformService;

/// Failure to resolve the operating system's stable per-user state directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserStateDirectoryError {
    /// This platform or execution context has no configured state-directory Adapter.
    Unavailable,
    /// The native platform source failed or returned an invalid path.
    DiscoveryFailed {
        /// Platform diagnostic retained for product recovery guidance.
        reason: String,
    },
}

impl std::fmt::Display for UserStateDirectoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => {
                formatter.write_str("stable per-user state directory is unavailable")
            }
            Self::DiscoveryFailed { reason } => {
                write!(
                    formatter,
                    "stable per-user state directory discovery failed: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for UserStateDirectoryError {}

/// Narrow Interface for persistent per-user state placement.
///
/// The returned path is stable across process launches and independent from
/// process-temporary or session-runtime directories. Callers append their own
/// domain namespace and remain responsible for directory creation, ownership,
/// permissions, durability, and symlink policy.
pub trait UserStateDirectory: Send + Sync {
    /// Resolve the platform's absolute per-user state root without creating it.
    fn user_state_directory(&self) -> Result<PathBuf, UserStateDirectoryError>;
}

impl UserStateDirectory for NoopPlatformService {
    fn user_state_directory(&self) -> Result<PathBuf, UserStateDirectoryError> {
        Err(UserStateDirectoryError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_adapter_is_explicit_and_interface_is_object_safe() {
        let state_directory: &dyn UserStateDirectory = &NoopPlatformService;
        assert_eq!(
            state_directory.user_state_directory(),
            Err(UserStateDirectoryError::Unavailable)
        );
    }
}
