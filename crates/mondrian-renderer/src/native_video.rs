//! Platform renderer admission for hardware-decoded native video resources.

#[cfg(target_os = "windows")]
mod windows_d3d11;

#[cfg(target_os = "windows")]
pub use windows_d3d11::{
    inspect_d3d11_native_decoded_frame, D3D11NativeDecodedFrameInspection,
    D3D11NativeDecodedFrameInspectionError, NativeVideoAdapterLuid,
};
