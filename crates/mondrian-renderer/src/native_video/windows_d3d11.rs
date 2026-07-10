//! D3D11 decoder-surface admission for the wgpu DX12 import backend.

use mondrian_media::{
    DecodedGpuFrameHandleKind, DecodedVideoSurfaceFormat, FfmpegNativeDecodedFrameResource,
    PreviewNativeDecodedFrame,
};
use windows::core::Interface;
use windows::Win32::Foundation::{E_POINTER, LUID};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D, D3D11_TEXTURE2D_DESC};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;

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

/// Validated facts for one FFmpeg-owned D3D11 decoded surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct D3D11NativeDecodedFrameInspection {
    /// Visible frame width consumed by the color pipeline.
    pub visible_width: u32,
    /// Visible frame height consumed by the color pipeline.
    pub visible_height: u32,
    /// Allocated decoder texture width, which may include codec alignment padding.
    pub storage_width: u32,
    /// Allocated decoder texture height, which may include codec alignment padding.
    pub storage_height: u32,
    /// Array slice containing this decoded frame.
    pub array_slice: u32,
    /// Renderer source texture format proven from the DXGI resource descriptor.
    pub source_texture_format: crate::GpuNativeDecodedFrameTextureFormat,
    /// Adapter shared by the decoder D3D11 device and the active wgpu DX12 adapter.
    pub adapter_luid: NativeVideoAdapterLuid,
}

/// Error rejecting a D3D11 decoder surface before resource sharing is attempted.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum D3D11NativeDecodedFrameInspectionError {
    /// The payload does not carry FFmpeg's preferred D3D11 texture handle family.
    #[error("native frame handle {actual:?} is not a D3D11 texture")]
    UnsupportedHandleKind {
        /// Actual decoded-frame handle family.
        actual: DecodedGpuFrameHandleKind,
    },
    /// The payload is not backed by the retained FFmpeg resource implementation.
    #[error("D3D11 native frame is not backed by an FFmpeg retained frame resource")]
    UnsupportedResourceType,
    /// FFmpeg's preferred D3D11 frame ABI could not expose a texture and slice.
    #[error("FFmpeg D3D11 frame ABI is invalid: {reason}")]
    InvalidFfmpegFrameAbi {
        /// Stable media-layer rejection reason.
        reason: String,
    },
    /// The source color payload is not one of the native two-plane formats.
    #[error("decoded surface format {actual:?} is not supported by the D3D11 import backend")]
    UnsupportedSurfaceFormat {
        /// Actual decoded surface format.
        actual: DecodedVideoSurfaceFormat,
    },
    /// A Windows COM operation failed while inspecting the source resource.
    #[error("Windows native video inspection failed during {operation}: HRESULT {hresult:#010x}")]
    WindowsApi {
        /// Stable operation name for diagnostics.
        operation: &'static str,
        /// Raw HRESULT value.
        hresult: i32,
    },
    /// The D3D11 texture is smaller than the visible decoded frame.
    #[error(
        "D3D11 decoder texture {storage_width}x{storage_height} is smaller than visible frame {visible_width}x{visible_height}"
    )]
    StorageExtentTooSmall {
        /// Visible decoded width.
        visible_width: u32,
        /// Visible decoded height.
        visible_height: u32,
        /// Allocated texture width.
        storage_width: u32,
        /// Allocated texture height.
        storage_height: u32,
    },
    /// The D3D11 texture has a DXGI format inconsistent with media metadata.
    #[error("D3D11 decoder texture format {actual} does not match expected {expected}")]
    TextureFormatMismatch {
        /// Expected stable DXGI format name.
        expected: &'static str,
        /// Raw actual DXGI format value.
        actual: i32,
    },
    /// The FFmpeg array slice falls outside the D3D11 texture array.
    #[error("D3D11 decoder array slice {array_slice} is outside array size {array_size}")]
    ArraySliceOutOfRange {
        /// FFmpeg-provided slice index.
        array_slice: u32,
        /// D3D11 texture array size.
        array_size: u32,
    },
    /// Decoder resources must have exactly one mip level.
    #[error("D3D11 decoder texture has {mip_levels} mip levels; exactly one is required")]
    UnsupportedMipLevels {
        /// Actual mip-level count.
        mip_levels: u32,
    },
    /// Decoder resources must be single-sampled.
    #[error("D3D11 decoder texture sample descriptor is {count}x quality {quality}; single-sample quality zero is required")]
    UnsupportedSampleDescriptor {
        /// Actual sample count.
        count: u32,
        /// Actual sample quality.
        quality: u32,
    },
    /// Native 4:2:0 textures require even storage dimensions.
    #[error("D3D11 {format} decoder texture storage extent {width}x{height} is not even")]
    InvalidSubsampledStorageExtent {
        /// Stable format name.
        format: &'static str,
        /// Storage width.
        width: u32,
        /// Storage height.
        height: u32,
    },
    /// The decoder texture and wgpu renderer reside on different adapters.
    #[error(
        "D3D11 decoder adapter LUID {source_luid:#018x} does not match wgpu DX12 adapter LUID {renderer_luid:#018x}"
    )]
    AdapterMismatch {
        /// Decoder D3D11 device adapter.
        source_luid: u64,
        /// Active renderer adapter.
        renderer_luid: u64,
    },
    /// The active wgpu adapter is not backed by DX12.
    #[error("active wgpu adapter is not backed by DX12")]
    WgpuAdapterIsNotDx12,
}

#[derive(Debug, Clone, Copy)]
struct D3D11TextureFacts {
    width: u32,
    height: u32,
    mip_levels: u32,
    array_size: u32,
    format: DXGI_FORMAT,
    sample_count: u32,
    sample_quality: u32,
}

/// Inspect and validate one FFmpeg D3D11 decoded frame against the active wgpu adapter.
///
/// This function performs admission only. It does not share, synchronize, adopt,
/// or sample the texture, and therefore does not establish renderer readiness.
pub fn inspect_d3d11_native_decoded_frame(
    adapter: &wgpu::Adapter,
    frame: &PreviewNativeDecodedFrame,
) -> Result<D3D11NativeDecodedFrameInspection, D3D11NativeDecodedFrameInspectionError> {
    Ok(validated_d3d11_native_decoded_frame(adapter, frame)?.inspection)
}

pub(super) struct ValidatedD3D11NativeDecodedFrame {
    pub inspection: D3D11NativeDecodedFrameInspection,
    pub texture: ID3D11Texture2D,
    pub device: ID3D11Device,
    pub dxgi_format: DXGI_FORMAT,
}

pub(super) fn validated_d3d11_native_decoded_frame(
    adapter: &wgpu::Adapter,
    frame: &PreviewNativeDecodedFrame,
) -> Result<ValidatedD3D11NativeDecodedFrame, D3D11NativeDecodedFrameInspectionError> {
    validated_d3d11_native_decoded_frame_for_luid(renderer_adapter_luid(adapter)?, frame)
}

pub(super) fn validated_d3d11_native_decoded_frame_for_luid(
    renderer_adapter_luid: NativeVideoAdapterLuid,
    frame: &PreviewNativeDecodedFrame,
) -> Result<ValidatedD3D11NativeDecodedFrame, D3D11NativeDecodedFrameInspectionError> {
    if frame.handle_kind() != DecodedGpuFrameHandleKind::D3D11Texture2D {
        return Err(
            D3D11NativeDecodedFrameInspectionError::UnsupportedHandleKind {
                actual: frame.handle_kind(),
            },
        );
    }
    let resource = frame
        .handle
        .resource::<FfmpegNativeDecodedFrameResource>()
        .ok_or(D3D11NativeDecodedFrameInspectionError::UnsupportedResourceType)?;
    let view = resource.d3d11_texture().map_err(|error| {
        D3D11NativeDecodedFrameInspectionError::InvalidFfmpegFrameAbi { reason: error.to_string() }
    })?;
    let expected = expected_dxgi_format(frame.surface_format)?;
    let raw_texture = view.texture_ptr();
    // SAFETY: FfmpegNativeDecodedFrameResource retains the AVFrame and its
    // ID3D11Texture2D reference for this borrow. FFmpeg's preferred D3D11 ABI
    // stores that exact interface pointer in data[0].
    let texture = unsafe { ID3D11Texture2D::from_raw_borrowed(&raw_texture) }
        .ok_or_else(|| windows_error("ID3D11Texture2D::from_raw_borrowed", E_POINTER))?;
    let facts = texture_facts(texture);
    validate_texture_facts(
        frame.width,
        frame.height,
        view.array_slice(),
        expected,
        facts,
    )?;
    // SAFETY: texture is live for this call; GetDevice returns an owned COM reference.
    let source_device = unsafe { texture.GetDevice() }
        .map_err(|error| windows_error("ID3D11Texture2D::GetDevice", error.code()))?;
    let source_adapter_luid = source_adapter_luid(&source_device)?;
    if source_adapter_luid != renderer_adapter_luid {
        return Err(D3D11NativeDecodedFrameInspectionError::AdapterMismatch {
            source_luid: source_adapter_luid.as_u64(),
            renderer_luid: renderer_adapter_luid.as_u64(),
        });
    }

    Ok(ValidatedD3D11NativeDecodedFrame {
        inspection: D3D11NativeDecodedFrameInspection {
            visible_width: frame.width,
            visible_height: frame.height,
            storage_width: facts.width,
            storage_height: facts.height,
            array_slice: view.array_slice(),
            source_texture_format: expected.renderer_format,
            adapter_luid: source_adapter_luid,
        },
        texture: texture.clone(),
        device: source_device,
        dxgi_format: expected.raw,
    })
}

#[derive(Debug, Clone, Copy)]
struct ExpectedDxgiFormat {
    raw: DXGI_FORMAT,
    name: &'static str,
    renderer_format: crate::GpuNativeDecodedFrameTextureFormat,
}

fn expected_dxgi_format(
    surface_format: DecodedVideoSurfaceFormat,
) -> Result<ExpectedDxgiFormat, D3D11NativeDecodedFrameInspectionError> {
    match surface_format {
        DecodedVideoSurfaceFormat::Nv12 => Ok(ExpectedDxgiFormat {
            raw: DXGI_FORMAT_NV12,
            name: "DXGI_FORMAT_NV12",
            renderer_format: crate::GpuNativeDecodedFrameTextureFormat::Nv12,
        }),
        DecodedVideoSurfaceFormat::P010 => Ok(ExpectedDxgiFormat {
            raw: DXGI_FORMAT_P010,
            name: "DXGI_FORMAT_P010",
            renderer_format: crate::GpuNativeDecodedFrameTextureFormat::P010,
        }),
        actual => Err(D3D11NativeDecodedFrameInspectionError::UnsupportedSurfaceFormat { actual }),
    }
}

fn texture_facts(texture: &ID3D11Texture2D) -> D3D11TextureFacts {
    let mut descriptor = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: texture is a live ID3D11Texture2D retained by the media frame;
    // GetDesc writes the complete POD descriptor without changing resource state.
    unsafe { texture.GetDesc(&mut descriptor) };
    D3D11TextureFacts {
        width: descriptor.Width,
        height: descriptor.Height,
        mip_levels: descriptor.MipLevels,
        array_size: descriptor.ArraySize,
        format: descriptor.Format,
        sample_count: descriptor.SampleDesc.Count,
        sample_quality: descriptor.SampleDesc.Quality,
    }
}

fn validate_texture_facts(
    visible_width: u32,
    visible_height: u32,
    array_slice: u32,
    expected: ExpectedDxgiFormat,
    facts: D3D11TextureFacts,
) -> Result<(), D3D11NativeDecodedFrameInspectionError> {
    if facts.width < visible_width || facts.height < visible_height {
        return Err(
            D3D11NativeDecodedFrameInspectionError::StorageExtentTooSmall {
                visible_width,
                visible_height,
                storage_width: facts.width,
                storage_height: facts.height,
            },
        );
    }
    if facts.format != expected.raw {
        return Err(
            D3D11NativeDecodedFrameInspectionError::TextureFormatMismatch {
                expected: expected.name,
                actual: facts.format.0,
            },
        );
    }
    if !facts.width.is_multiple_of(2) || !facts.height.is_multiple_of(2) {
        return Err(
            D3D11NativeDecodedFrameInspectionError::InvalidSubsampledStorageExtent {
                format: expected.name,
                width: facts.width,
                height: facts.height,
            },
        );
    }
    if array_slice >= facts.array_size {
        return Err(
            D3D11NativeDecodedFrameInspectionError::ArraySliceOutOfRange {
                array_slice,
                array_size: facts.array_size,
            },
        );
    }
    if facts.mip_levels != 1 {
        return Err(
            D3D11NativeDecodedFrameInspectionError::UnsupportedMipLevels {
                mip_levels: facts.mip_levels,
            },
        );
    }
    if facts.sample_count != 1 || facts.sample_quality != 0 {
        return Err(
            D3D11NativeDecodedFrameInspectionError::UnsupportedSampleDescriptor {
                count: facts.sample_count,
                quality: facts.sample_quality,
            },
        );
    }
    Ok(())
}

fn source_adapter_luid(
    device: &ID3D11Device,
) -> Result<NativeVideoAdapterLuid, D3D11NativeDecodedFrameInspectionError> {
    let dxgi_device: IDXGIDevice = device.cast().map_err(|error| {
        windows_error("ID3D11Device::QueryInterface<IDXGIDevice>", error.code())
    })?;
    // SAFETY: dxgi_device is a live COM interface and returns owned adapter metadata.
    let adapter = unsafe { dxgi_device.GetAdapter() }
        .map_err(|error| windows_error("IDXGIDevice::GetAdapter", error.code()))?;
    // SAFETY: adapter is live and GetDesc returns a POD snapshot.
    let descriptor = unsafe { adapter.GetDesc() }
        .map_err(|error| windows_error("IDXGIAdapter::GetDesc", error.code()))?;
    Ok(NativeVideoAdapterLuid::from_windows(descriptor.AdapterLuid))
}

pub(super) fn renderer_adapter_luid(
    adapter: &wgpu::Adapter,
) -> Result<NativeVideoAdapterLuid, D3D11NativeDecodedFrameInspectionError> {
    // SAFETY: the guard is borrowed only for this metadata query and no HAL
    // resource is destroyed or mutated.
    let hal_adapter = unsafe { adapter.as_hal::<wgpu::hal::api::Dx12>() }
        .ok_or(D3D11NativeDecodedFrameInspectionError::WgpuAdapterIsNotDx12)?;
    // SAFETY: the HAL adapter guard keeps the IDXGIAdapter alive for GetDesc2.
    let descriptor = unsafe { hal_adapter.raw_adapter().GetDesc2() }
        .map_err(|error| windows_error("IDXGIAdapter2::GetDesc2", error.code()))?;
    Ok(NativeVideoAdapterLuid::from_windows(descriptor.AdapterLuid))
}

fn windows_error(
    operation: &'static str,
    code: windows::core::HRESULT,
) -> D3D11NativeDecodedFrameInspectionError {
    D3D11NativeDecodedFrameInspectionError::WindowsApi { operation, hresult: code.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_facts(format: DXGI_FORMAT) -> D3D11TextureFacts {
        D3D11TextureFacts {
            width: 1920,
            height: 1088,
            mip_levels: 1,
            array_size: 4,
            format,
            sample_count: 1,
            sample_quality: 0,
        }
    }

    #[test]
    fn luid_preserves_signed_high_bits() {
        let luid =
            NativeVideoAdapterLuid::from_windows(LUID { LowPart: 0x7654_3210, HighPart: -2 });
        assert_eq!(luid.as_u64(), 0xffff_fffe_7654_3210);
    }

    #[test]
    fn texture_validation_accepts_aligned_storage_extent() {
        validate_texture_facts(
            1920,
            1080,
            3,
            expected_dxgi_format(DecodedVideoSurfaceFormat::Nv12).expect("NV12 must be supported"),
            valid_facts(DXGI_FORMAT_NV12),
        )
        .expect("aligned decoder storage larger than the visible frame must be accepted");
    }

    #[test]
    fn texture_validation_rejects_format_spoofing() {
        let error = validate_texture_facts(
            1920,
            1080,
            0,
            expected_dxgi_format(DecodedVideoSurfaceFormat::P010).expect("P010 must be supported"),
            valid_facts(DXGI_FORMAT_NV12),
        )
        .expect_err("P010 metadata over an NV12 resource must fail");
        assert_eq!(
            error,
            D3D11NativeDecodedFrameInspectionError::TextureFormatMismatch {
                expected: "DXGI_FORMAT_P010",
                actual: DXGI_FORMAT_NV12.0,
            }
        );
    }

    #[test]
    fn texture_validation_rejects_out_of_range_slice() {
        let error = validate_texture_facts(
            1920,
            1080,
            4,
            expected_dxgi_format(DecodedVideoSurfaceFormat::Nv12).expect("NV12 must be supported"),
            valid_facts(DXGI_FORMAT_NV12),
        )
        .expect_err("array slice equal to array size must fail");
        assert_eq!(
            error,
            D3D11NativeDecodedFrameInspectionError::ArraySliceOutOfRange {
                array_slice: 4,
                array_size: 4,
            }
        );
    }

    #[test]
    fn texture_validation_rejects_storage_smaller_than_visible_frame() {
        let mut facts = valid_facts(DXGI_FORMAT_NV12);
        facts.width = 1919;
        let error = validate_texture_facts(
            1920,
            1080,
            0,
            expected_dxgi_format(DecodedVideoSurfaceFormat::Nv12).expect("NV12 must be supported"),
            facts,
        )
        .expect_err("decoder storage must cover the full visible frame");
        assert_eq!(
            error,
            D3D11NativeDecodedFrameInspectionError::StorageExtentTooSmall {
                visible_width: 1920,
                visible_height: 1080,
                storage_width: 1919,
                storage_height: 1088,
            }
        );
    }

    #[test]
    fn texture_validation_rejects_nontrivial_mips_and_samples() {
        let expected =
            expected_dxgi_format(DecodedVideoSurfaceFormat::Nv12).expect("NV12 must be supported");
        let mut mipmapped = valid_facts(DXGI_FORMAT_NV12);
        mipmapped.mip_levels = 2;
        assert_eq!(
            validate_texture_facts(1920, 1080, 0, expected, mipmapped)
                .expect_err("decoder surfaces must not be mipmapped"),
            D3D11NativeDecodedFrameInspectionError::UnsupportedMipLevels { mip_levels: 2 }
        );

        let mut multisampled = valid_facts(DXGI_FORMAT_NV12);
        multisampled.sample_count = 2;
        multisampled.sample_quality = 1;
        assert_eq!(
            validate_texture_facts(1920, 1080, 0, expected, multisampled)
                .expect_err("decoder surfaces must be single-sampled"),
            D3D11NativeDecodedFrameInspectionError::UnsupportedSampleDescriptor {
                count: 2,
                quality: 1,
            }
        );
    }

    #[test]
    fn texture_validation_rejects_odd_subsampled_storage_extent() {
        let mut facts = valid_facts(DXGI_FORMAT_NV12);
        facts.height = 1087;
        assert_eq!(
            validate_texture_facts(
                1920,
                1080,
                0,
                expected_dxgi_format(DecodedVideoSurfaceFormat::Nv12)
                    .expect("NV12 must be supported"),
                facts,
            )
            .expect_err("4:2:0 decoder storage must have even dimensions"),
            D3D11NativeDecodedFrameInspectionError::InvalidSubsampledStorageExtent {
                format: "DXGI_FORMAT_NV12",
                width: 1920,
                height: 1087,
            }
        );
    }
}
