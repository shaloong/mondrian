//! Exact Headless partial-start ownership and consuming failure cleanup.
//!
//! Construction order differs between Endurance and Preview-first callers.
//! Keep Preview independent of GPU creation stage and never put live owners
//! into an anyhow error. Only owner-free receipts cross that error seam.

use std::fmt;
use std::time::Instant;

use super::headless_preview_presentation::HeadlessPreviewRuntime;
use super::headless_viewer_gpu::HeadlessViewerGpuAdapter;
use super::headless_viewer_gpu::HeadlessViewerGpuOutput;
use super::preview_runtime::PreviewRuntimeShutdownEvidence;
use super::preview_runtime::{PreviewStartupFailure, PreviewStartupOwner};
use super::preview_shutdown_evidence::PreviewStartupShutdownEvidence;
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
    /// Present only for an unpublished, partially constructed Preview Runtime.
    pub preview_startup: Option<PreviewStartupShutdownEvidence>,
    /// Exact GPU creation stage and its actual consuming receipt.
    pub gpu: HeadlessStartupGpuShutdownEvidence,
}

impl HeadlessStartupShutdownEvidence {
    /// Whether every created owner closed; a failed start remains a failed start.
    pub fn all_created_resources_released(&self) -> bool {
        !self.preview_construction_unverified
            && !self.opaque_panic_payload_abandoned
            && !(self.preview.is_some() && self.preview_startup.is_some())
            && self.preview.as_ref().is_none_or(|preview| preview.all_workers_terminated())
            && self
                .preview_startup
                .as_ref()
                .is_none_or(|preview| preview.all_created_resources_released())
            && self.gpu.all_created_resources_released()
    }
}

enum StartupGpuOwner {
    NotStarted,
    Partial(Box<ViewerGpuStartupOwner>),
    Adapter(Box<HeadlessViewerGpuAdapter>),
}

enum StartupPreviewOwner {
    NotStarted,
    Partial(Box<PreviewStartupOwner<HeadlessViewerGpuOutput>>),
    Runtime(Box<HeadlessPreviewRuntime>),
}

/// Owning failure, deliberately not an anyhow-convertible execution error.
#[must_use = "retain the actual startup owners until consuming shutdown"]
pub(crate) struct HeadlessExecutionStartFailure {
    preview_construction_unverified: bool,
    diagnostic: anyhow::Error,
    preview: StartupPreviewOwner,
    gpu: StartupGpuOwner,
}

impl fmt::Debug for HeadlessExecutionStartFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeadlessExecutionStartFailure")
            .field("diagnostic", &self.diagnostic)
            .field(
                "preview_created",
                &!matches!(self.preview, StartupPreviewOwner::NotStarted),
            )
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
            preview: StartupPreviewOwner::NotStarted,
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
            preview: StartupPreviewOwner::NotStarted,
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
            preview: StartupPreviewOwner::Runtime(Box::new(preview)),
            gpu: StartupGpuOwner::Adapter(Box::new(gpu)),
            preview_construction_unverified: false,
        }
    }

    /// Preserve an Adapter when Preview construction has not returned an owner.
    pub(crate) fn adapter(diagnostic: anyhow::Error, gpu: HeadlessViewerGpuAdapter) -> Self {
        Self {
            diagnostic,
            preview: StartupPreviewOwner::NotStarted,
            gpu: StartupGpuOwner::Adapter(Box::new(gpu)),
            preview_construction_unverified: true,
        }
    }

    /// Retain known partial Preview ownership, optionally alongside an existing GPU.
    pub(crate) fn partial_preview(
        failure: PreviewStartupFailure<HeadlessViewerGpuOutput>,
        gpu: Option<HeadlessViewerGpuAdapter>,
    ) -> Self {
        let (diagnostic, owner) = failure.into_parts();
        Self {
            diagnostic,
            preview: StartupPreviewOwner::Partial(Box::new(owner)),
            gpu: gpu.map_or(StartupGpuOwner::NotStarted, |gpu| {
                StartupGpuOwner::Adapter(Box::new(gpu))
            }),
            preview_construction_unverified: false,
        }
    }

    /// Attach the Preview-first caller's existing owner without replacing one.
    pub(crate) fn with_preview(mut self, preview: HeadlessPreviewRuntime) -> Self {
        assert!(
            matches!(self.preview, StartupPreviewOwner::NotStarted),
            "startup Preview is installed exactly once"
        );
        self.preview = StartupPreviewOwner::Runtime(Box::new(preview));
        self
    }

    /// Borrow the original structured diagnostic while retaining every owner.
    pub(crate) fn diagnostic(&self) -> &anyhow::Error {
        &self.diagnostic
    }

    /// Close producer admission before any consumer or App begins joining.
    pub(crate) fn begin_shutdown(&mut self) {
        match &mut self.preview {
            StartupPreviewOwner::NotStarted => {}
            StartupPreviewOwner::Partial(preview) => preview.begin_shutdown(),
            StartupPreviewOwner::Runtime(preview) => preview.begin_endurance_shutdown(),
        }
    }

    /// Consume Preview before GPU under one absolute deadline; return no live owner.
    pub(crate) fn shutdown_until(
        mut self,
        deadline: Instant,
    ) -> (anyhow::Error, HeadlessStartupShutdownEvidence) {
        self.begin_shutdown();
        let (preview, preview_startup) = match self.preview {
            StartupPreviewOwner::NotStarted => (None, None),
            StartupPreviewOwner::Partial(preview) => (None, Some(preview.shutdown_until(deadline))),
            StartupPreviewOwner::Runtime(preview) => (Some(preview.shutdown_until(deadline)), None),
        };
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
        let opaque_panic_payload_abandoned =
            super::execution_panic_diagnostic::opaque_panic_payload_abandoned(&self.diagnostic);
        (
            self.diagnostic,
            HeadlessStartupShutdownEvidence {
                preview,
                preview_startup,
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

pub(crate) use super::execution_panic_diagnostic::startup_panic_diagnostic;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_partial_preview_retains_diagnostic_and_exclusive_raw_inventory() {
        let failure = match HeadlessPreviewRuntime::try_start_with_checkpoint_for_test(0, |stage| {
            if stage == super::super::preview_runtime::PreviewStartupCheckpoint::Observer {
                panic!("partial Preview original failure");
            }
        }) {
            Err(failure) => failure,
            Ok(runtime) => {
                let _ = runtime.shutdown_and_wait();
                panic!("injection not reached");
            }
        };
        let (diagnostic, receipt) = HeadlessExecutionStartFailure::partial_preview(failure, None)
            .shutdown_until(Instant::now() + std::time::Duration::from_secs(5));
        assert!(diagnostic.to_string().contains("partial Preview original failure"));
        assert!(!receipt.preview_construction_unverified);
        assert!(receipt.preview.is_none());
        assert!(receipt.preview_startup.is_some());
        assert!(receipt.all_created_resources_released());
        assert!(receipt
            .preview_startup
            .as_ref()
            .expect("partial")
            .runtime
            .all_workers_terminated());
        let mut contradictory = receipt.clone();
        contradictory.preview = Some(receipt.preview_startup.expect("partial").runtime);
        assert!(!contradictory.all_created_resources_released());
    }

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
