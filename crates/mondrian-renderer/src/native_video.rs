//! Platform renderer admission for hardware-decoded native video resources.

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod direct_backend;
mod gpu_timing;

pub use gpu_timing::{
    NativeVideoImportCandidateTimingReceipt, NativeVideoImportCandidateToken,
    NativeVideoImportGpuTimingDiagnostics, NativeVideoImportGpuTimingPolicy,
    NativeVideoImportGpuTimingSample, NativeVideoImportToken,
    NATIVE_VIDEO_IMPORT_GPU_TIMING_MAX_CAPACITY, NATIVE_VIDEO_IMPORT_GPU_TIMING_SCHEMA_VERSION,
};

/// CPU command-preparation attribution for native decoded-frame import.
///
/// These measurements cover host-side validation, bridge coordination, and
/// command recording. They are not GPU execution timings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct NativeVideoImportCpuTimings {
    /// Native payload validation and immutable contract construction.
    pub source_validation_us: u64,
    /// Bridge-slot acquisition, decoder-surface copy publication, and GPU-side acquire setup.
    pub bridge_acquire_us: u64,
    /// Cached YUV pass preparation, intermediate acquisition, and encoder creation.
    pub pipeline_prepare_us: u64,
    /// Native YUV-to-encoded-RGB command recording.
    pub yuv_record_us: u64,
    /// Source-to-working color-stage preparation, allocation, and command recording.
    pub color_stage_us: u64,
    /// Typed resource-table extraction after command recording.
    pub resource_extract_us: u64,
    /// Internal bridge command submission and native-resource release publication.
    pub submit_us: u64,
    /// Entire successful import call.
    pub total_us: u64,
}

impl NativeVideoImportCpuTimings {
    /// Accumulate per-stage attribution across the bridge sub-executions of
    /// one import. Currently only the D3D12 backend splits an import into
    /// separately measured bridge stages.
    #[cfg(target_os = "windows")]
    pub(crate) fn accumulate(&mut self, other: Self) {
        self.source_validation_us =
            self.source_validation_us.saturating_add(other.source_validation_us);
        self.bridge_acquire_us = self.bridge_acquire_us.saturating_add(other.bridge_acquire_us);
        self.pipeline_prepare_us =
            self.pipeline_prepare_us.saturating_add(other.pipeline_prepare_us);
        self.yuv_record_us = self.yuv_record_us.saturating_add(other.yuv_record_us);
        self.color_stage_us = self.color_stage_us.saturating_add(other.color_stage_us);
        self.resource_extract_us =
            self.resource_extract_us.saturating_add(other.resource_extract_us);
        self.submit_us = self.submit_us.saturating_add(other.submit_us);
        self.total_us = self.total_us.saturating_add(other.total_us);
    }
}

#[cfg(all(test, target_os = "windows"))]
mod timing_tests {
    use super::NativeVideoImportCpuTimings;

    #[test]
    fn native_import_timings_accumulate_with_saturation() {
        let mut total = NativeVideoImportCpuTimings {
            source_validation_us: u64::MAX,
            total_us: 7,
            ..NativeVideoImportCpuTimings::default()
        };
        total.accumulate(NativeVideoImportCpuTimings {
            source_validation_us: 1,
            bridge_acquire_us: 2,
            total_us: 5,
            ..NativeVideoImportCpuTimings::default()
        });

        assert_eq!(total.source_validation_us, u64::MAX);
        assert_eq!(total.bridge_acquire_us, 2);
        assert_eq!(total.total_us, 12);
    }
}

mod yuv_decode;

/// Maximum codec-padding inflation admitted by the native-import bridge.
///
/// The Viewer active-texture estimator reserves this multiple of the visible
/// NV12/P010 surface bytes for one renderer-owned bridge texture. Native
/// backend validation rejects a decoder allocation outside the same envelope
/// before creating or growing a bridge entry, so the request-only estimate
/// remains a hard upper bound without platform inspection in the pure
/// estimator.
pub const GPU_NATIVE_IMPORT_MAX_STORAGE_PIXEL_RATIO: u64 = 2;

pub use yuv_decode::{
    GpuNativeVideoExtent, GpuNativeYuvDecodePlan, GpuNativeYuvDecodePlanError,
    GpuNativeYuvDecodeRecordError, GpuNativeYuvDecoder, GpuNativeYuvPlaneViews,
    GpuNativeYuvPreparedPass,
};

#[cfg(target_os = "windows")]
mod sync_timeline;

#[cfg(target_os = "macos")]
mod metal_backend;
#[cfg(target_os = "linux")]
mod vulkan_backend;
#[cfg(target_os = "windows")]
mod windows_adapter;
#[cfg(target_os = "windows")]
mod windows_d3d12;
#[cfg(target_os = "windows")]
mod windows_d3d12_backend;
#[cfg(target_os = "windows")]
mod windows_d3d12_bridge;

#[cfg(target_os = "macos")]
pub use metal_backend::{MetalNativeVideoImportBackend, MetalNativeVideoImportBackendCreateError};
#[cfg(target_os = "linux")]
pub use vulkan_backend::{
    VulkanNativeVideoImportBackend, VulkanNativeVideoImportBackendCreateError,
};
#[cfg(target_os = "windows")]
pub use windows_adapter::{NativeVideoAdapterError, NativeVideoAdapterLuid};
#[cfg(target_os = "windows")]
pub use windows_d3d12::{
    inspect_d3d12_native_decoded_frame, D3D12NativeDecodedFrameInspection,
    D3D12NativeDecodedFrameInspectionError,
};
#[cfg(target_os = "windows")]
pub use windows_d3d12_backend::{
    D3D12NativeVideoImportBackend, D3D12NativeVideoImportBackendCreateError,
    D3D12NativeVideoImportBackendOptions,
};
