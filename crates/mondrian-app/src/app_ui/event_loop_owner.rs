//! Typed ownership evidence for the process-local winit event loop.
//!
//! Dropping the Rust owner proves only that Mondrian returned its in-process
//! authority. It deliberately does not claim that the OS compositor or native
//! display server has reached a physically terminal state.

/// Stable classification of a winit event-loop construction failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppUiEventLoopConstructionFailureKind {
    /// The selected platform backend cannot provide the requested event loop.
    NotSupported,
    /// The operating system rejected event-loop construction.
    OperatingSystem,
    /// Winit rejected a second process-local event-loop owner.
    RecreationAttempt,
    /// Winit reported an application exit status while constructing/running.
    ExitFailure(i32),
}

/// Exact typed failure returned before an event-loop owner exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUiEventLoopConstructionFailure {
    kind: AppUiEventLoopConstructionFailureKind,
    diagnostic: String,
}

impl AppUiEventLoopConstructionFailure {
    /// Stable failure classification.
    pub const fn kind(&self) -> AppUiEventLoopConstructionFailureKind {
        self.kind
    }

    /// Original human-readable winit diagnostic.
    pub fn diagnostic(&self) -> &str {
        &self.diagnostic
    }

    #[cfg(test)]
    pub(crate) fn synthetic(
        kind: AppUiEventLoopConstructionFailureKind,
        diagnostic: impl Into<String>,
    ) -> Self {
        Self { kind, diagnostic: diagnostic.into() }
    }
}

impl From<winit::error::EventLoopError> for AppUiEventLoopConstructionFailure {
    fn from(error: winit::error::EventLoopError) -> Self {
        let kind = match &error {
            winit::error::EventLoopError::NotSupported(_) => {
                AppUiEventLoopConstructionFailureKind::NotSupported
            }
            winit::error::EventLoopError::Os(_) => {
                AppUiEventLoopConstructionFailureKind::OperatingSystem
            }
            winit::error::EventLoopError::RecreationAttempt => {
                AppUiEventLoopConstructionFailureKind::RecreationAttempt
            }
            winit::error::EventLoopError::ExitFailure(status) => {
                AppUiEventLoopConstructionFailureKind::ExitFailure(*status)
            }
        };
        Self { kind, diagnostic: error.to_string() }
    }
}

impl std::fmt::Display for AppUiEventLoopConstructionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.diagnostic)
    }
}

impl std::error::Error for AppUiEventLoopConstructionFailure {}

/// Rust-owner handback evidence emitted only after the event-loop drop returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppUiEventLoopShutdownEvidence {
    rust_owner_released: bool,
}

impl AppUiEventLoopShutdownEvidence {
    pub(crate) const fn after_owner_drop() -> Self {
        Self { rust_owner_released: true }
    }

    /// Whether the process-local Rust event-loop owner was released.
    pub const fn rust_owner_released(self) -> bool {
        self.rust_owner_released
    }

    /// Native physical termination is outside this Rust ownership proof.
    pub const fn qualifies_physical_native_termination(self) -> bool {
        false
    }
}
