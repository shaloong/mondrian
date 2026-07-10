//! Platform renderer admission for hardware-decoded native video resources.

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
mod windows_d3d11_bridge;

#[cfg(target_os = "windows")]
pub use windows_d3d11::{
    inspect_d3d11_native_decoded_frame, D3D11NativeDecodedFrameInspection,
    D3D11NativeDecodedFrameInspectionError, NativeVideoAdapterLuid,
};
#[cfg(target_os = "windows")]
pub use windows_d3d11_bridge::{
    D3D11Dx12PreparedVideoFrame, D3D11Dx12SharedVideoTexture, D3D11Dx12SharedVideoTextureError,
    D3D11Dx12VideoPlaneViews,
};
