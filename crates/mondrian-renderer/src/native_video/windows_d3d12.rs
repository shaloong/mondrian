//! D3D12VA decoder-resource admission for the wgpu DX12 import backend.

use super::windows_adapter::{
    renderer_adapter_luid, NativeVideoAdapterError, NativeVideoAdapterLuid,
};
use super::GPU_NATIVE_IMPORT_MAX_STORAGE_PIXEL_RATIO;
use crate::GpuNativeDecodedFrameTextureFormat;
use mondrian_media::{
    DecodedGpuFrameHandleKind, DecodedVideoSurfaceFormat, FfmpegNativeDecodedFrameResource,
    PreviewNativeDecodedFrame,
};
use windows::core::Interface;
use windows::Win32::Foundation::E_POINTER;
use windows::Win32::Graphics::Direct3D12::{
    ID3D12Device, ID3D12Fence, ID3D12Resource, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010};

/// Validated facts for one FFmpeg-owned D3D12VA decoded surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct D3D12NativeDecodedFrameInspection {
    /// Visible frame width consumed by the color pipeline.
    pub visible_width: u32,
    /// Visible frame height consumed by the color pipeline.
    pub visible_height: u32,
    /// Allocated decoder texture width, including codec alignment padding.
    pub storage_width: u32,
    /// Allocated decoder texture height, including codec alignment padding.
    pub storage_height: u32,
    /// Renderer source texture format proven from the D3D12 descriptor.
    pub source_texture_format: GpuNativeDecodedFrameTextureFormat,
    /// Adapter shared by the decoder D3D12 device and active wgpu DX12 adapter.
    pub adapter_luid: NativeVideoAdapterLuid,
    /// FFmpeg fence value that proves decode completion for this frame.
    pub decode_fence_value: u64,
}

/// Error rejecting a D3D12VA decoder surface before any copy is submitted.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum D3D12NativeDecodedFrameInspectionError {
    /// The payload does not carry a D3D12 resource handle family.
    #[error("native frame handle {actual:?} is not a D3D12 resource")]
    UnsupportedHandleKind {
        /// Actual decoded-frame handle family.
        actual: DecodedGpuFrameHandleKind,
    },
    /// The payload is not backed by the retained FFmpeg resource implementation.
    #[error("D3D12 native frame is not backed by an FFmpeg retained frame resource")]
    UnsupportedResourceType,
    /// FFmpeg's D3D12 frame ABI could not expose a resource and decode fence.
    #[error("FFmpeg D3D12 frame ABI is invalid: {reason}")]
    InvalidFfmpegFrameAbi {
        /// Stable media-layer rejection reason.
        reason: String,
    },
    /// The active renderer adapter does not expose a valid DX12 identity.
    #[error(transparent)]
    RendererAdapter(#[from] NativeVideoAdapterError),
    /// The source color payload is not one of the native two-plane formats.
    #[error("decoded surface format {actual:?} is not supported by the D3D12 import backend")]
    UnsupportedSurfaceFormat {
        /// Actual decoded surface format.
        actual: DecodedVideoSurfaceFormat,
    },
    /// A Windows COM operation failed while inspecting the source resource.
    #[error(
        "Windows D3D12 native video inspection failed during {operation}: HRESULT {hresult:#010x}"
    )]
    WindowsApi {
        /// Stable operation name for diagnostics.
        operation: &'static str,
        /// Raw HRESULT value.
        hresult: i32,
    },
    /// Decoder resources must be two-dimensional textures.
    #[error("D3D12 decoder resource dimension {actual} is not TEXTURE2D")]
    UnsupportedResourceDimension {
        /// Raw D3D12 resource-dimension value.
        actual: i32,
    },
    /// The D3D12 texture is smaller than the visible decoded frame.
    #[error(
        "D3D12 decoder texture {storage_width}x{storage_height} is smaller than visible frame {visible_width}x{visible_height}"
    )]
    StorageExtentTooSmall {
        /// Visible decoded width.
        visible_width: u32,
        /// Visible decoded height.
        visible_height: u32,
        /// Allocated texture width.
        storage_width: u64,
        /// Allocated texture height.
        storage_height: u32,
    },
    /// Codec padding exceeded the renderer-owned bridge allocation envelope.
    #[error(
        "D3D12 decoder texture {storage_width}x{storage_height} exceeds the native-import storage envelope for visible frame {visible_width}x{visible_height}"
    )]
    StorageExtentExceedsImportEnvelope {
        /// Visible decoded width.
        visible_width: u32,
        /// Visible decoded height.
        visible_height: u32,
        /// Allocated texture width.
        storage_width: u64,
        /// Allocated texture height.
        storage_height: u32,
    },
    /// The D3D12 resource width cannot be represented by renderer texture APIs.
    #[error("D3D12 decoder texture width {width} exceeds the renderer u32 extent contract")]
    StorageWidthOverflow {
        /// Raw D3D12 resource width.
        width: u64,
    },
    /// The D3D12 texture format conflicts with media metadata.
    #[error("D3D12 decoder texture format {actual} does not match expected {expected}")]
    TextureFormatMismatch {
        /// Expected stable DXGI format name.
        expected: &'static str,
        /// Raw actual DXGI format value.
        actual: i32,
    },
    /// FFmpeg D3D12VA output is one texture per frame, not an array resource.
    #[error(
        "D3D12 decoder texture depth/array size is {depth_or_array_size}; exactly one is required"
    )]
    UnsupportedDepthOrArraySize {
        /// Actual depth/array size.
        depth_or_array_size: u16,
    },
    /// Decoder resources must have exactly one mip level.
    #[error("D3D12 decoder texture has {mip_levels} mip levels; exactly one is required")]
    UnsupportedMipLevels {
        /// Actual mip-level count.
        mip_levels: u16,
    },
    /// Decoder resources must be single-sampled.
    #[error("D3D12 decoder texture sample descriptor is {count}x quality {quality}; single-sample quality zero is required")]
    UnsupportedSampleDescriptor {
        /// Actual sample count.
        count: u32,
        /// Actual sample quality.
        quality: u32,
    },
    /// Native 4:2:0 textures require even storage dimensions.
    #[error("D3D12 {format} decoder texture storage extent {width}x{height} is not even")]
    InvalidSubsampledStorageExtent {
        /// Stable format name.
        format: &'static str,
        /// Storage width.
        width: u32,
        /// Storage height.
        height: u32,
    },
    /// The decoder texture and active wgpu renderer reside on different adapters.
    #[error(
        "D3D12 decoder adapter LUID {source_luid:#018x} does not match wgpu DX12 adapter LUID {renderer_luid:#018x}"
    )]
    AdapterMismatch {
        /// Decoder D3D12 device adapter.
        source_luid: u64,
        /// Active renderer adapter.
        renderer_luid: u64,
    },
    /// FFmpeg exposed a fence created by a different D3D12 device.
    #[error("D3D12 decode-completion fence belongs to a different device than the texture")]
    DecodeFenceDeviceMismatch,
}

#[derive(Debug, Clone, Copy)]
struct D3D12TextureFacts {
    dimension: i32,
    width: u64,
    height: u32,
    depth_or_array_size: u16,
    mip_levels: u16,
    format: DXGI_FORMAT,
    sample_count: u32,
    sample_quality: u32,
}

#[derive(Debug, Clone, Copy)]
struct ExpectedDxgiFormat {
    raw: DXGI_FORMAT,
    name: &'static str,
    renderer_format: GpuNativeDecodedFrameTextureFormat,
}

pub(super) struct ValidatedD3D12NativeDecodedFrame {
    pub inspection: D3D12NativeDecodedFrameInspection,
    pub texture: ID3D12Resource,
    pub decode_fence: ID3D12Fence,
    pub device: ID3D12Device,
    pub dxgi_format: DXGI_FORMAT,
}

/// Inspect and validate one FFmpeg D3D12VA decoded frame against the active renderer adapter.
pub fn inspect_d3d12_native_decoded_frame(
    adapter: &wgpu::Adapter,
    frame: &PreviewNativeDecodedFrame,
) -> Result<D3D12NativeDecodedFrameInspection, D3D12NativeDecodedFrameInspectionError> {
    let renderer_luid = renderer_adapter_luid(adapter)?;
    Ok(validated_d3d12_native_decoded_frame_for_luid(renderer_luid, frame)?.inspection)
}

pub(super) fn validated_d3d12_native_decoded_frame_for_luid(
    renderer_adapter_luid: NativeVideoAdapterLuid,
    frame: &PreviewNativeDecodedFrame,
) -> Result<ValidatedD3D12NativeDecodedFrame, D3D12NativeDecodedFrameInspectionError> {
    if frame.handle_kind() != DecodedGpuFrameHandleKind::D3D12Resource {
        return Err(
            D3D12NativeDecodedFrameInspectionError::UnsupportedHandleKind {
                actual: frame.handle_kind(),
            },
        );
    }
    let resource = frame
        .handle
        .resource::<FfmpegNativeDecodedFrameResource>()
        .ok_or(D3D12NativeDecodedFrameInspectionError::UnsupportedResourceType)?;
    let view = resource.d3d12_texture().map_err(|error| {
        D3D12NativeDecodedFrameInspectionError::InvalidFfmpegFrameAbi { reason: error.to_string() }
    })?;
    let raw_texture = view.texture_ptr();
    let raw_fence = view.fence_ptr();
    // SAFETY: the retained AVFrame owns both COM interfaces for this borrow.
    let texture = unsafe { ID3D12Resource::from_raw_borrowed(&raw_texture) }
        .ok_or_else(|| windows_error("ID3D12Resource::from_raw_borrowed", E_POINTER))?;
    // SAFETY: same retained `AVD3D12VAFrame` sync contract as the resource.
    let decode_fence = unsafe { ID3D12Fence::from_raw_borrowed(&raw_fence) }
        .ok_or_else(|| windows_error("ID3D12Fence::from_raw_borrowed", E_POINTER))?;
    let expected = expected_dxgi_format(frame.surface_format)?;
    let facts = texture_facts(texture);
    let (storage_width, storage_height) =
        validate_texture_facts(frame.width, frame.height, expected, facts)?;
    let source_device = resource_device(texture, "ID3D12Resource::GetDevice")?;
    let fence_device = fence_device(decode_fence)?;
    if source_device.as_raw() != fence_device.as_raw() {
        return Err(D3D12NativeDecodedFrameInspectionError::DecodeFenceDeviceMismatch);
    }
    // SAFETY: source_device is live and GetAdapterLuid returns POD identity.
    let source_adapter_luid =
        NativeVideoAdapterLuid::from_windows(unsafe { source_device.GetAdapterLuid() });
    if source_adapter_luid != renderer_adapter_luid {
        return Err(D3D12NativeDecodedFrameInspectionError::AdapterMismatch {
            source_luid: source_adapter_luid.as_u64(),
            renderer_luid: renderer_adapter_luid.as_u64(),
        });
    }

    Ok(ValidatedD3D12NativeDecodedFrame {
        inspection: D3D12NativeDecodedFrameInspection {
            visible_width: frame.width,
            visible_height: frame.height,
            storage_width,
            storage_height,
            source_texture_format: expected.renderer_format,
            adapter_luid: source_adapter_luid,
            decode_fence_value: view.fence_value(),
        },
        texture: texture.clone(),
        decode_fence: decode_fence.clone(),
        device: source_device,
        dxgi_format: expected.raw,
    })
}

fn expected_dxgi_format(
    surface_format: DecodedVideoSurfaceFormat,
) -> Result<ExpectedDxgiFormat, D3D12NativeDecodedFrameInspectionError> {
    match surface_format {
        DecodedVideoSurfaceFormat::Nv12 => Ok(ExpectedDxgiFormat {
            raw: DXGI_FORMAT_NV12,
            name: "DXGI_FORMAT_NV12",
            renderer_format: GpuNativeDecodedFrameTextureFormat::Nv12,
        }),
        DecodedVideoSurfaceFormat::P010 => Ok(ExpectedDxgiFormat {
            raw: DXGI_FORMAT_P010,
            name: "DXGI_FORMAT_P010",
            renderer_format: GpuNativeDecodedFrameTextureFormat::P010,
        }),
        actual => Err(D3D12NativeDecodedFrameInspectionError::UnsupportedSurfaceFormat { actual }),
    }
}

fn texture_facts(texture: &ID3D12Resource) -> D3D12TextureFacts {
    // SAFETY: the retained resource is live and GetDesc returns a POD snapshot.
    let descriptor = unsafe { texture.GetDesc() };
    D3D12TextureFacts {
        dimension: descriptor.Dimension.0,
        width: descriptor.Width,
        height: descriptor.Height,
        depth_or_array_size: descriptor.DepthOrArraySize,
        mip_levels: descriptor.MipLevels,
        format: descriptor.Format,
        sample_count: descriptor.SampleDesc.Count,
        sample_quality: descriptor.SampleDesc.Quality,
    }
}

fn validate_texture_facts(
    visible_width: u32,
    visible_height: u32,
    expected: ExpectedDxgiFormat,
    facts: D3D12TextureFacts,
) -> Result<(u32, u32), D3D12NativeDecodedFrameInspectionError> {
    if facts.dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D.0 {
        return Err(
            D3D12NativeDecodedFrameInspectionError::UnsupportedResourceDimension {
                actual: facts.dimension,
            },
        );
    }
    if facts.width < u64::from(visible_width) || facts.height < visible_height {
        return Err(
            D3D12NativeDecodedFrameInspectionError::StorageExtentTooSmall {
                visible_width,
                visible_height,
                storage_width: facts.width,
                storage_height: facts.height,
            },
        );
    }
    let visible_pixels = u128::from(visible_width) * u128::from(visible_height);
    let storage_pixels = u128::from(facts.width) * u128::from(facts.height);
    if storage_pixels > visible_pixels * u128::from(GPU_NATIVE_IMPORT_MAX_STORAGE_PIXEL_RATIO) {
        return Err(
            D3D12NativeDecodedFrameInspectionError::StorageExtentExceedsImportEnvelope {
                visible_width,
                visible_height,
                storage_width: facts.width,
                storage_height: facts.height,
            },
        );
    }
    let storage_width = u32::try_from(facts.width).map_err(|_| {
        D3D12NativeDecodedFrameInspectionError::StorageWidthOverflow { width: facts.width }
    })?;
    if facts.format != expected.raw {
        return Err(
            D3D12NativeDecodedFrameInspectionError::TextureFormatMismatch {
                expected: expected.name,
                actual: facts.format.0,
            },
        );
    }
    if facts.depth_or_array_size != 1 {
        return Err(
            D3D12NativeDecodedFrameInspectionError::UnsupportedDepthOrArraySize {
                depth_or_array_size: facts.depth_or_array_size,
            },
        );
    }
    if facts.mip_levels != 1 {
        return Err(
            D3D12NativeDecodedFrameInspectionError::UnsupportedMipLevels {
                mip_levels: facts.mip_levels,
            },
        );
    }
    if facts.sample_count != 1 || facts.sample_quality != 0 {
        return Err(
            D3D12NativeDecodedFrameInspectionError::UnsupportedSampleDescriptor {
                count: facts.sample_count,
                quality: facts.sample_quality,
            },
        );
    }
    if !storage_width.is_multiple_of(2) || !facts.height.is_multiple_of(2) {
        return Err(
            D3D12NativeDecodedFrameInspectionError::InvalidSubsampledStorageExtent {
                format: expected.name,
                width: storage_width,
                height: facts.height,
            },
        );
    }
    Ok((storage_width, facts.height))
}

fn resource_device(
    resource: &ID3D12Resource,
    operation: &'static str,
) -> Result<ID3D12Device, D3D12NativeDecodedFrameInspectionError> {
    let mut device = None;
    // SAFETY: the resource is live and returns an owned creating-device reference.
    unsafe { resource.GetDevice(&mut device) }
        .map_err(|error| windows_error(operation, error.code()))?;
    device.ok_or_else(|| windows_error(operation, E_POINTER))
}

fn fence_device(
    fence: &ID3D12Fence,
) -> Result<ID3D12Device, D3D12NativeDecodedFrameInspectionError> {
    let mut device = None;
    // SAFETY: the fence is live and returns an owned creating-device reference.
    unsafe { fence.GetDevice(&mut device) }
        .map_err(|error| windows_error("ID3D12Fence::GetDevice", error.code()))?;
    device.ok_or_else(|| windows_error("ID3D12Fence::GetDevice", E_POINTER))
}

fn windows_error(
    operation: &'static str,
    code: windows::core::HRESULT,
) -> D3D12NativeDecodedFrameInspectionError {
    D3D12NativeDecodedFrameInspectionError::WindowsApi { operation, hresult: code.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_facts(format: DXGI_FORMAT) -> D3D12TextureFacts {
        D3D12TextureFacts {
            dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D.0,
            width: 3840,
            height: 2176,
            depth_or_array_size: 1,
            mip_levels: 1,
            format,
            sample_count: 1,
            sample_quality: 0,
        }
    }

    #[test]
    fn texture_validation_accepts_aligned_p010_storage_extent() {
        assert_eq!(
            validate_texture_facts(
                3840,
                2160,
                expected_dxgi_format(DecodedVideoSurfaceFormat::P010)
                    .expect("P010 must be supported"),
                valid_facts(DXGI_FORMAT_P010),
            )
            .expect("aligned storage must be accepted"),
            (3840, 2176)
        );
    }

    #[test]
    fn texture_validation_rejects_non_single_frame_resources() {
        let mut facts = valid_facts(DXGI_FORMAT_NV12);
        facts.depth_or_array_size = 4;
        assert_eq!(
            validate_texture_facts(
                3840,
                2160,
                expected_dxgi_format(DecodedVideoSurfaceFormat::Nv12)
                    .expect("NV12 must be supported"),
                facts,
            )
            .expect_err("FFmpeg D3D12VA output must be one resource per frame"),
            D3D12NativeDecodedFrameInspectionError::UnsupportedDepthOrArraySize {
                depth_or_array_size: 4,
            }
        );
    }

    #[test]
    fn texture_validation_rejects_format_spoofing() {
        let error = validate_texture_facts(
            3840,
            2160,
            expected_dxgi_format(DecodedVideoSurfaceFormat::P010).expect("P010 must be supported"),
            valid_facts(DXGI_FORMAT_NV12),
        )
        .expect_err("P010 metadata over NV12 resource must fail");
        assert_eq!(
            error,
            D3D12NativeDecodedFrameInspectionError::TextureFormatMismatch {
                expected: "DXGI_FORMAT_P010",
                actual: DXGI_FORMAT_NV12.0,
            }
        );
    }

    #[test]
    fn texture_validation_enforces_active_budget_storage_envelope() {
        let mut at_limit = valid_facts(DXGI_FORMAT_NV12);
        at_limit.width = 1280;
        at_limit.height = 360;
        assert_eq!(
            validate_texture_facts(
                640,
                360,
                expected_dxgi_format(DecodedVideoSurfaceFormat::Nv12)
                    .expect("NV12 must be supported"),
                at_limit,
            )
            .expect("two-times pixel storage is the admitted boundary"),
            (1280, 360)
        );

        let mut outside = at_limit;
        outside.height = 720;
        assert_eq!(
            validate_texture_facts(
                640,
                360,
                expected_dxgi_format(DecodedVideoSurfaceFormat::Nv12)
                    .expect("NV12 must be supported"),
                outside,
            )
            .expect_err("storage outside the estimated bridge envelope must fail"),
            D3D12NativeDecodedFrameInspectionError::StorageExtentExceedsImportEnvelope {
                visible_width: 640,
                visible_height: 360,
                storage_width: 1280,
                storage_height: 720,
            }
        );
    }
}
