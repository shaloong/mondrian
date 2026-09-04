//! Exact Headless partial-start ownership and consuming failure cleanup.
//!
//! Construction order differs between Endurance and Preview-first callers.
//! Keep Preview independent of GPU creation stage and never put live owners
//! into an anyhow error. Only owner-free receipts cross that error seam.

use std::fmt;
use std::time::Instant;

use super::headless_preview_presentation::HeadlessPreviewRuntime;
use super::headless_viewer_gpu::HeadlessViewerGpuAdapter;
use super::preview_runtime::PreviewRuntimeShutdownEvidence;
use super::viewer_gpu_device_progress::ViewerGpuDeviceProgressShutdownEvidence;
use super::viewer_gpu_startup::{ViewerGpuStartupOwner, ViewerGpuStartupShutdownEvidence};

/// GPU inventory actually created before Headless startup failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "created", content = "receipt", rename_all = "snake_case")]
pub enum HeadlessStartupGpuShutdownEvidence {
    /// No Viewer progress/Renderer worker started (a raw device may have existed).
    NotStarted,
    /// The generation never became a complete presentation Adapter.
    Partial(ViewerGpuStartupShutdownEvidence),
    /// A complete Adapter was consumed, including presentation/lifecycle owners.
    Adapter(ViewerGpuDeviceProgressShutdownEvidence),
}

impl HeadlessStartupGpuShutdownEvidence {
    /// Fault-free closure of the actual inventory, not normal-runtime admission.
    pub const fn all_created_resources_released(self) -> bool {
        match self {
            Self::NotStarted => true,
            Self::Partial(receipt) => receipt.all_created_resources_released(),
            Self::Adapter(receipt) => receipt.qualifies_normal_runtime(),
        }
    }
}

/// Owner-free closure of a failed Headless construction or decoder binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessStartupShutdownEvidence {
    /// Preview construction unwound before returning a consuming owner.
    pub preview_construction_unverified: bool,
    /// An opaque unwind payload was deliberately retained, not proved released.
    pub opaque_panic_payload_abandoned: bool,
    /// Present only when a Preview Runtime was actually constructed.
    pub preview: Option<PreviewRuntimeShutdownEvidence>,
    /// Exact GPU creation stage and its actual consuming receipt.
    pub gpu: HeadlessStartupGpuShutdownEvidence,
}

impl HeadlessStartupShutdownEvidence {
    /// Whether every created owner closed; a failed start remains a failed start.
    pub fn all_created_resources_released(&self) -> bool {
        !self.preview_construction_unverified
            && !self.opaque_panic_payload_abandoned
            && self.preview.as_ref().is_none_or(|preview| preview.all_workers_terminated())
            && self.gpu.all_created_resources_released()
    }
}

enum StartupGpuOwner {
    NotStarted,
    Partial(Box<ViewerGpuStartupOwner>),
    Adapter(Box<HeadlessViewerGpuAdapter>),
}

/// Owning failure, deliberately not an anyhow-convertible execution error.
#[must_use = "retain the actual startup owners until consuming shutdown"]
pub(crate) struct HeadlessExecutionStartFailure {
    preview_construction_unverified: bool,
    diagnostic: anyhow::Error,
    preview: Option<Box<HeadlessPreviewRuntime>>,
    gpu: StartupGpuOwner,
}

impl fmt::Debug for HeadlessExecutionStartFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeadlessExecutionStartFailure")
            .field("diagnostic", &self.diagnostic)
            .field("preview_created", &self.preview.is_some())
            .field(
                "gpu",
                &match self.gpu {
                    StartupGpuOwner::NotStarted => "not_started",
                    StartupGpuOwner::Partial(_) => "partial",
                    StartupGpuOwner::Adapter(_) => "adapter",
                },
            )
            .finish()
    }
}

impl fmt::Display for HeadlessExecutionStartFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:#}", self.diagnostic)
    }
}

impl HeadlessExecutionStartFailure {
    /// Preserve a failure that occurred before any Viewer progress worker started.
    pub(crate) fn before_progress(diagnostic: anyhow::Error) -> Self {
        Self {
            diagnostic,
            preview: None,
            gpu: StartupGpuOwner::NotStarted,
            preview_construction_unverified: false,
        }
    }

    /// Preview-first construction unwound; partial worker inventory is unverified.
    pub(crate) fn preview_construction(diagnostic: anyhow::Error) -> Self {
        Self {
            preview_construction_unverified: true,
            ..Self::before_progress(diagnostic)
        }
    }

    /// Preserve the unactivated GPU guard before erasing constructor diagnostics.
    pub(crate) fn partial(diagnostic: anyhow::Error, owner: ViewerGpuStartupOwner) -> Self {
        Self {
            diagnostic,
            preview: None,
            gpu: StartupGpuOwner::Partial(Box::new(owner)),
            preview_construction_unverified: false,
        }
    }

    /// Preserve the complete pair after decoder binding failed.
    pub(crate) fn binding(
        diagnostic: anyhow::Error,
        preview: HeadlessPreviewRuntime,
        gpu: HeadlessViewerGpuAdapter,
    ) -> Self {
        Self {
            diagnostic,
            preview: Some(Box::new(preview)),
            gpu: StartupGpuOwner::Adapter(Box::new(gpu)),
            preview_construction_unverified: false,
        }
    }

    /// Preserve an Adapter when Preview construction has not returned an owner.
    pub(crate) fn adapter(diagnostic: anyhow::Error, gpu: HeadlessViewerGpuAdapter) -> Self {
        Self {
            diagnostic,
            preview: None,
            gpu: StartupGpuOwner::Adapter(Box::new(gpu)),
            preview_construction_unverified: true,
        }
    }

    /// Attach the Preview-first caller's existing owner without replacing one.
    pub(crate) fn with_preview(mut self, preview: HeadlessPreviewRuntime) -> Self {
        assert!(
            self.preview.is_none(),
            "startup Preview is installed exactly once"
        );
        self.preview = Some(Box::new(preview));
        self
    }

    /// Borrow the original structured diagnostic while retaining every owner.
    pub(crate) fn diagnostic(&self) -> &anyhow::Error {
        &self.diagnostic
    }

    /// Close producer admission before any consumer or App begins joining.
    pub(crate) fn begin_shutdown(&mut self) {
        if let Some(preview) = &mut self.preview {
            preview.begin_endurance_shutdown();
        }
    }

    /// Consume Preview before GPU under one absolute deadline; return no live owner.
    pub(crate) fn shutdown_until(
        mut self,
        deadline: Instant,
    ) -> (anyhow::Error, HeadlessStartupShutdownEvidence) {
        self.begin_shutdown();
        let preview = self.preview.take().map(|preview| preview.shutdown_until(deadline));
        let gpu = match self.gpu {
            StartupGpuOwner::NotStarted => HeadlessStartupGpuShutdownEvidence::NotStarted,
            StartupGpuOwner::Partial(owner) => HeadlessStartupGpuShutdownEvidence::Partial(
                owner
                    .shutdown_until(deadline)
                    .expect("failed construction retains its unactivated guard"),
            ),
            StartupGpuOwner::Adapter(owner) => {
                HeadlessStartupGpuShutdownEvidence::Adapter(owner.shutdown_until(deadline))
            }
        };
        let opaque_panic_payload_abandoned = self
            .diagnostic
            .downcast_ref::<StartupPanic>()
            .is_some_and(|panic| panic.opaque_payload_abandoned);
        (
            self.diagnostic,
            HeadlessStartupShutdownEvidence {
                preview,
                gpu,
                opaque_panic_payload_abandoned,
                preview_construction_unverified: self.preview_construction_unverified,
            },
        )
    }

    /// Erase only owner-free evidence after consuming the actual startup owners.
    pub(crate) fn into_closed_error(self, deadline: Instant) -> anyhow::Error {
        let (diagnostic, evidence) = self.shutdown_until(deadline);
        HeadlessStartupClosedFailure { diagnostic, evidence }.into()
    }
}

/// Original startup failure with its exact owner-free consuming cleanup receipt.
#[derive(Debug, thiserror::Error)]
#[error("Headless startup failed: {diagnostic:#}; startup_closure={evidence:?}")]
pub struct HeadlessStartupClosedFailure {
    /// Original typed constructor or binding diagnostic.
    #[source]
    pub diagnostic: anyhow::Error,
    /// Actual inventory and cleanup result, never normal execution evidence.
    pub evidence: HeadlessStartupShutdownEvidence,
}

#[derive(Debug, thiserror::Error)]
#[error("{operation} panicked: {detail}; opaque_payload_abandoned={opaque_payload_abandoned}")]
struct StartupPanic {
    operation: &'static str,
    detail: String,
    opaque_payload_abandoned: bool,
}

/// Preserve a caught startup panic as a diagnostic, separately from live owners.
pub(crate) fn startup_panic_diagnostic(payload: Box<dyn std::any::Any + Send>) -> anyhow::Error {
    execution_panic_diagnostic(payload, "Headless startup")
}

/// Convert an unwind payload without executing an opaque user destructor.
pub(crate) fn execution_panic_diagnostic(
    payload: Box<dyn std::any::Any + Send>,
    operation: &'static str,
) -> anyhow::Error {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|value| (*value).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "opaque startup panic payload".to_owned());
    // Opaque payload destructors can themselves panic or carry foreign owners.
    // Match the product's explicit abandonment policy instead of unwinding here.
    let opaque_payload_abandoned = !(payload.is::<&'static str>() || payload.is::<String>());
    if opaque_payload_abandoned {
        std::mem::forget(payload);
    } else {
        drop(payload);
    }
    StartupPanic { operation, detail, opaque_payload_abandoned }.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_before_progress_preserves_original_error_without_inventing_owners() {
        let error = HeadlessExecutionStartFailure::before_progress(anyhow::anyhow!(
            "injected pre-progress fault"
        ))
        .into_closed_error(Instant::now());
        let closed =
            error.downcast_ref::<HeadlessStartupClosedFailure>().expect("owner-free error");
        assert_eq!(closed.diagnostic.to_string(), "injected pre-progress fault");
        assert!(closed.evidence.preview.is_none());
        assert_eq!(
            closed.evidence.gpu,
            HeadlessStartupGpuShutdownEvidence::NotStarted
        );
        assert!(closed.evidence.all_created_resources_released());
    }

    #[test]
    fn startup_preview_construction_unwind_is_not_a_not_started_receipt() {
        for payload in [
            Box::new("partial Preview panic") as Box<dyn std::any::Any + Send>,
            Box::new(String::from("partial Preview panic")),
        ] {
            let mut failure =
                HeadlessExecutionStartFailure::before_progress(startup_panic_diagnostic(payload));
            failure.preview_construction_unverified = true;
            let (_, receipt) = failure.shutdown_until(Instant::now());
            assert!(!receipt.opaque_panic_payload_abandoned);
            assert!(receipt.preview_construction_unverified);
            assert!(!receipt.all_created_resources_released());
        }
    }

    #[test]
    fn startup_opaque_unwind_payload_never_runs_its_destructor_or_qualifies_cleanup() {
        struct PanickingDrop;
        impl Drop for PanickingDrop {
            fn drop(&mut self) {
                panic!("must not run opaque payload destructor");
            }
        }
        let failure = HeadlessExecutionStartFailure::before_progress(startup_panic_diagnostic(
            Box::new(PanickingDrop),
        ));
        let (_, receipt) = failure.shutdown_until(Instant::now());
        assert!(receipt.opaque_panic_payload_abandoned);
        assert!(!receipt.all_created_resources_released());
    }
}
