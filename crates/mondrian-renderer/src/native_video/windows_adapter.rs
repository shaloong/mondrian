//! Windows adapter identity shared by native-video decode and render devices.

use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ERROR_NOT_FOUND};

/// Stable adapter identity used to reject cross-adapter native video imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NativeVideoAdapterLuid(u64);

impl NativeVideoAdapterLuid {
    pub(super) fn from_windows(value: LUID) -> Self {
        Self((u64::from(value.HighPart as u32) << 32) | u64::from(value.LowPart))
    }

    /// Raw 64-bit Windows LUID representation for diagnostics and cache keys.
    pub fn as_u64(self) -> u64 {
        self.0
    }

    #[cfg(test)]
    pub(super) fn from_raw_for_test(value: u64) -> Self {
        Self(value)
    }
}

/// Error resolving the active wgpu DX12 adapter for native-video decode.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum NativeVideoAdapterError {
    /// The active wgpu adapter is not backed by DX12.
    #[error("active wgpu adapter is not backed by DX12")]
    WgpuAdapterIsNotDx12,
    /// A Windows adapter metadata operation failed.
    #[error("Windows adapter query failed during {operation}: HRESULT {hresult:#010x}")]
    WindowsApi {
        /// Stable operation name for diagnostics.
        operation: &'static str,
        /// Raw HRESULT value.
        hresult: i32,
    },
    /// The active DX12 adapter was not found in FFmpeg's DXGI enumeration.
    #[error("wgpu DX12 adapter LUID 0x{luid:016x} was not found in DXGI adapter enumeration")]
    RendererAdapterIndexNotFound {
        /// Renderer adapter LUID that could not be mapped.
        luid: u64,
    },
}

pub(super) fn renderer_adapter_luid(
    adapter: &wgpu::Adapter,
) -> Result<NativeVideoAdapterLuid, NativeVideoAdapterError> {
    // SAFETY: the guard is borrowed only for this metadata query and no HAL
    // resource is destroyed or mutated.
    let hal_adapter = unsafe { adapter.as_hal::<wgpu::hal::api::Dx12>() }
        .ok_or(NativeVideoAdapterError::WgpuAdapterIsNotDx12)?;
    // SAFETY: the HAL adapter guard keeps the IDXGIAdapter alive for GetDesc2.
    let descriptor = unsafe { hal_adapter.raw_adapter().GetDesc2() }
        .map_err(|error| windows_error("IDXGIAdapter2::GetDesc2", error.code()))?;
    Ok(NativeVideoAdapterLuid::from_windows(descriptor.AdapterLuid))
}

pub(super) fn renderer_adapter_dxgi_index(
    adapter: &wgpu::Adapter,
) -> Result<u32, NativeVideoAdapterError> {
    let target_luid = renderer_adapter_luid(adapter)?;
    // SAFETY: CreateDXGIFactory1 returns an owned COM factory on success.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }
        .map_err(|error| windows_error("CreateDXGIFactory1", error.code()))?;
    for index in 0..u32::MAX {
        // SAFETY: the factory remains alive for enumeration and returns an
        // owned adapter reference on success.
        let enumerated = match unsafe { factory.EnumAdapters1(index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => {
                return Err(windows_error("IDXGIFactory1::EnumAdapters1", error.code()));
            }
        };
        // SAFETY: the enumerated adapter is live for this descriptor query.
        let descriptor = unsafe { enumerated.GetDesc1() }
            .map_err(|error| windows_error("IDXGIAdapter1::GetDesc1", error.code()))?;
        if NativeVideoAdapterLuid::from_windows(descriptor.AdapterLuid) == target_luid {
            return Ok(index);
        }
    }
    Err(NativeVideoAdapterError::RendererAdapterIndexNotFound { luid: target_luid.as_u64() })
}

fn windows_error(operation: &'static str, code: windows::core::HRESULT) -> NativeVideoAdapterError {
    NativeVideoAdapterError::WindowsApi { operation, hresult: code.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luid_preserves_signed_high_bits() {
        let luid =
            NativeVideoAdapterLuid::from_windows(LUID { LowPart: 0x7654_3210, HighPart: -2 });
        assert_eq!(luid.as_u64(), 0xffff_fffe_7654_3210);
    }
}
