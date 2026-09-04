//! Consuming Viewer execution retirement; queue and Adapter barriers stay external.

use std::sync::Arc;

use crate::{
    GpuColorFrameWgpuResourcePool, GpuDisplayCalibrationRuntime, GpuFrameCompositor,
    GpuNativeDecodedFrameImportError, GpuProgramScopesRuntime, GpuSignalMonitorRuntime,
    RenderGpuOutputBoundaryRuntime, ViewerNativeVideoImportRuntime,
};

/// Observed terminal result after joining the actual CPU YUV upload worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewerCpuYuvUploadWorkerExit {
    /// The worker returned normally and its thread was joined.
    Returned,
    /// The worker unwound; it was joined, so release is safe but qualification fails.
    Panicked,
}

/// Renderer-owned retirement facts; not proof of Adapter or whole-queue closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewerGpuRetirementReceipt {
    /// Actual upload-worker terminal result, never inferred from queue idleness.
    pub cpu_yuv_upload: ViewerCpuYuvUploadWorkerExit,
    /// Native source release required typed physical device-removal evidence.
    pub native_device_removed: bool,
}

impl ViewerGpuRetirementReceipt {
    /// Whether the safely retired Renderer also exited without a physical fault.
    pub const fn is_healthy(self) -> bool {
        matches!(self.cpu_yuv_upload, ViewerCpuYuvUploadWorkerExit::Returned)
            && !self.native_device_removed
    }
}

/// Poll-only owner retaining all execution resources after upload admission closes.
///
/// Keep this owner until both [`Self::poll`] returns a receipt and the Adapter's
/// submission lifecycle and whole-device queue barrier prove safe release. A
/// returned receipt may be unhealthy; never retain an already joined panicked
/// worker forever, or turn its failure into a successful qualification.
#[must_use = "retain and poll the Renderer owner through the Adapter's queue retirement barrier"]
pub struct ViewerGpuExecutionRetirement {
    pub(crate) native_video_import: ViewerNativeVideoImportRuntime,
    pub(crate) cpu_yuv_upload: crate::cpu_yuv::CpuYuvUploadRetirement,
    pub(crate) terminal: Option<ViewerGpuRetirementReceipt>,
    pub(crate) native_device_removed: bool,
    pub(crate) _resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    pub(crate) _color_output: RenderGpuOutputBoundaryRuntime,
    pub(crate) _spatial: crate::viewer_spatial::GpuViewerSpatialRuntime,
    pub(crate) _display_calibration: GpuDisplayCalibrationRuntime,
    pub(crate) _program_scopes: GpuProgramScopesRuntime,
    pub(crate) _signal_monitor: GpuSignalMonitorRuntime,
    pub(crate) _working_compositor: GpuFrameCompositor,
}

impl Drop for ViewerGpuExecutionRetirement {
    fn drop(&mut self) {
        // Revoke outstanding presentation/encoder leases only at final release:
        // admission closure must keep idle resources through the queue barrier.
        self._resource_pool.invalidate();
    }
}

impl ViewerGpuExecutionRetirement {
    /// Non-blockingly join a finished upload worker and retire proven native reads.
    ///
    /// `None` or an error means retain the owner and keep driving the external
    /// queue barrier. A receipt is stable across repeated polls and proves only
    /// Renderer-owned termination, not completion of other Adapter submissions.
    pub fn poll(
        &mut self,
    ) -> Result<Option<ViewerGpuRetirementReceipt>, GpuNativeDecodedFrameImportError> {
        if let Some(terminal) = self.terminal {
            return Ok(Some(terminal));
        }
        let upload = self.cpu_yuv_upload.poll();
        match self.native_video_import.retire_completed_source_residency() {
            Ok(_) => {}
            Err(error) if error.is_native_device_removed() => {
                self.native_device_removed = true;
            }
            Err(error) => return Err(error),
        }
        if self.native_video_import.retained_source_count() != 0 {
            return Ok(None);
        }
        let Some(cpu_yuv_upload) = upload else {
            return Ok(None);
        };
        let terminal = ViewerGpuRetirementReceipt {
            cpu_yuv_upload,
            native_device_removed: self.native_device_removed,
        };
        self.terminal = Some(terminal);
        Ok(Some(terminal))
    }
}
