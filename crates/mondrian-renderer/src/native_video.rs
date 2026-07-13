//! Platform renderer admission for hardware-decoded native video resources.

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

#[cfg(test)]
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

pub use yuv_decode::{
    GpuNativeVideoExtent, GpuNativeYuvDecodePlan, GpuNativeYuvDecodePlanError,
    GpuNativeYuvDecodeRecordError, GpuNativeYuvDecoder, GpuNativeYuvPlaneViews,
    GpuNativeYuvPreparedPass,
};

#[cfg(target_os = "windows")]
mod sync_timeline;

#[cfg(target_os = "windows")]
mod windows_d3d11;
#[cfg(target_os = "windows")]
mod windows_d3d11_backend;
#[cfg(target_os = "windows")]
mod windows_d3d11_bridge;

#[cfg(target_os = "windows")]
pub use windows_d3d11::{
    inspect_d3d11_native_decoded_frame, D3D11NativeDecodedFrameInspection,
    D3D11NativeDecodedFrameInspectionError, NativeVideoAdapterLuid,
};
#[cfg(target_os = "windows")]
pub use windows_d3d11_backend::{
    D3D11Dx12NativeVideoImportBackend, D3D11Dx12NativeVideoImportBackendCreateError,
    D3D11Dx12NativeVideoImportBackendOptions,
};
#[cfg(target_os = "windows")]
pub use windows_d3d11_bridge::{
    D3D11Dx12PreparedVideoFrame, D3D11Dx12SharedVideoTexture, D3D11Dx12SharedVideoTextureError,
    D3D11Dx12VideoPlaneViews,
};
