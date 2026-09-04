//! Partial GPU-generation ownership before an Adapter can admit Viewer work.
//!
//! Keep this guard in the caller, outside any fallible/panicking construction
//! closure. UI windows and surfaces stay on the UI thread. Only the GPU owner
//! moves to the existing progress domain on abandonment; no new reaper exists.

use mondrian_renderer::{ViewerGpuExecutionRetirement, ViewerGpuExecutionRuntime};

#[cfg(feature = "validation")]
use super::viewer_gpu_device_progress::ViewerGpuDeviceGenerationId;
use super::viewer_gpu_device_progress::{
    ViewerGpuDeviceGenerationRetirement, ViewerGpuDeviceGenerationRetirementReceipt,
    ViewerGpuDeviceGenerationTerminal, ViewerGpuDeviceProgressOwner,
    ViewerGpuDeviceProgressStartError, ViewerGpuDeviceProgressWake,
};

/// Exact created inventory and bounded retirement of a partial GPU generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ViewerGpuStartupShutdownEvidence {
    /// Observed before retirement, not inferred from an absent terminal receipt.
    pub renderer_created: bool,
    /// Actual progress-worker and optional Renderer retirement facts.
    pub progress: super::viewer_gpu_device_progress::ViewerGpuDeviceProgressShutdownEvidence,
}

impl ViewerGpuStartupShutdownEvidence {
    /// Whether every actually created owner closed without a qualification fault.
    pub const fn all_created_resources_released(self) -> bool {
        self.progress.qualifies_created_inventory(self.renderer_created)
    }
}

/// Owns a started progress worker and exactly the Renderer created so far.
#[must_use = "activate the generation or retain it through consuming startup shutdown"]
pub(crate) struct ViewerGpuStartupOwner {
    handles: Option<(wgpu::Device, wgpu::Queue)>,
    progress: Option<ViewerGpuDeviceProgressOwner>,
    runtime: Option<ViewerGpuExecutionRuntime>,
}

impl ViewerGpuStartupOwner {
    /// Install the unique device callback before constructing GPU consumers.
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        wake: ViewerGpuDeviceProgressWake,
    ) -> Result<Self, ViewerGpuDeviceProgressStartError> {
        // Prepare handles before starting the worker; after spawn only moves remain.
        let device = device.clone();
        let queue = queue.clone();
        let progress = ViewerGpuDeviceProgressOwner::new(&device, wake)?;
        Ok(Self {
            handles: Some((device, queue)),
            progress: Some(progress),
            runtime: None,
        })
    }

    /// Retain a successfully created Renderer before any later setup can fail.
    pub(crate) fn install_runtime(&mut self, runtime: ViewerGpuExecutionRuntime) {
        assert!(
            self.progress.is_some() && self.runtime.is_none(),
            "startup runtime installed once"
        );
        self.runtime = Some(runtime);
    }

    /// Borrow the retained runtime without transferring partial-construction ownership.
    pub(crate) fn runtime(&self) -> Option<&ViewerGpuExecutionRuntime> {
        self.runtime.as_ref()
    }

    /// Identity reserved by the partial generation before it is published.
    #[cfg(feature = "validation")]
    pub(crate) fn generation_id(&self) -> ViewerGpuDeviceGenerationId {
        self.progress
            .as_ref()
            .expect("unactivated Viewer GPU startup retains progress ownership")
            .generation_id()
    }

    /// Move a complete generation into its Adapter after all other setup succeeds.
    pub(crate) fn activate(
        &mut self,
    ) -> Option<(ViewerGpuDeviceProgressOwner, ViewerGpuExecutionRuntime)> {
        if self.progress.is_none() || self.runtime.is_none() {
            return None;
        }
        // Check both members before taking either: failed activation keeps ownership.
        let generation = (self.progress.take()?, self.runtime.take()?);
        // Initial Window guards can outlive multiple device reopens. Once
        // activated they must not pin the previous generation's device/queue.
        self.handles.take();
        Some(generation)
    }

    /// Consume the actual partial inventory using one unchanged caller deadline.
    /// An already activated guard has no remaining owner and returns no receipt.
    pub(crate) fn shutdown_until(
        mut self,
        deadline: std::time::Instant,
    ) -> Option<ViewerGpuStartupShutdownEvidence> {
        let renderer_created = self.runtime.is_some();
        let (progress, retirement) = self.take_retirement()?;
        Some(ViewerGpuStartupShutdownEvidence {
            renderer_created,
            progress: progress.retire_device_generation_until(retirement, deadline),
        })
    }

    fn take_retirement(&mut self) -> Option<(ViewerGpuDeviceProgressOwner, StartupRetirement)> {
        let progress = self.progress.take()?;
        let (device, queue) = self.handles.take().expect("unactivated startup GPU handles");
        Some((
            progress,
            StartupRetirement {
                runtime: self.runtime.take().map(ViewerGpuExecutionRuntime::into_retirement),
                _device: device,
                _queue: queue,
                native_error_logged: false,
            },
        ))
    }
}

impl Drop for ViewerGpuStartupOwner {
    fn drop(&mut self) {
        if let Some((progress, retirement)) = self.take_retirement() {
            progress.retire_device_generation(retirement);
        }
    }
}

struct StartupRetirement {
    runtime: Option<ViewerGpuExecutionRetirement>,
    _device: wgpu::Device,
    _queue: wgpu::Queue,
    native_error_logged: bool,
}

impl ViewerGpuDeviceGenerationRetirement for StartupRetirement {
    fn label(&self) -> &'static str {
        "partially constructed Viewer GPU generation"
    }

    fn poll_retirement(
        &mut self,
        _terminal: Option<&ViewerGpuDeviceGenerationTerminal>,
    ) -> Option<ViewerGpuDeviceGenerationRetirementReceipt> {
        let Some(runtime) = self.runtime.as_mut() else {
            // Explicitly not constructed; the progress worker still must prove
            // its independent whole-queue barrier before releasing this envelope.
            return Some(ViewerGpuDeviceGenerationRetirementReceipt { renderer: None });
        };
        match runtime.poll() {
            Ok(Some(renderer)) => {
                Some(ViewerGpuDeviceGenerationRetirementReceipt { renderer: Some(renderer) })
            }
            Ok(None) => None,
            Err(error) => {
                if !self.native_error_logged {
                    self.native_error_logged = true;
                    tracing::error!(%error, "partial Viewer GPU startup retirement pending");
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests;
