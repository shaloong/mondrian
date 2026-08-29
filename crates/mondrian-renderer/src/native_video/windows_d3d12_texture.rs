//! D3D12 texture adoption and explicit raw/wgpu ownership transitions.

use std::mem::ManuallyDrop;

use super::windows_d3d12::D3D12NativeDecodedFrameInspection;
use crate::GpuNativeDecodedFrameTextureFormat;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D12::{
    ID3D12CommandAllocator, ID3D12CommandList, ID3D12CommandQueue, ID3D12Device,
    ID3D12GraphicsCommandList, ID3D12PipelineState, ID3D12Resource, D3D12_COMMAND_LIST_TYPE_DIRECT,
    D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
    D3D12_RESOURCE_STATES, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
    D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE, D3D12_RESOURCE_TRANSITION_BARRIER,
};

pub(super) const WGPU_RESOURCE_STATE: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATES(
    D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE.0 | D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE.0,
);

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub(super) enum D3D12NativeTextureError {
    #[error("wgpu device did not enable native video texture feature {feature}")]
    DeviceFeatureMissing { feature: &'static str },
    #[error("D3D12 native texture operation {operation} failed: HRESULT {hresult:#010x}")]
    WindowsApi {
        operation: &'static str,
        hresult: i32,
    },
    #[error("native video texture {width}x{height} exceeds wgpu limit {limit}")]
    DeviceTextureLimitExceeded { width: u32, height: u32, limit: u32 },
    #[error("unsupported D3D12 native texture format {format:?}")]
    UnsupportedNativeTextureFormat {
        format: GpuNativeDecodedFrameTextureFormat,
    },
}

pub(super) struct D3D12TransitionCommands {
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
}

impl D3D12TransitionCommands {
    pub(super) fn new(device: &ID3D12Device) -> Result<Self, D3D12NativeTextureError> {
        let allocator = unsafe {
            device.CreateCommandAllocator::<ID3D12CommandAllocator>(D3D12_COMMAND_LIST_TYPE_DIRECT)
        }
        .map_err(|error| windows_error("ID3D12Device::CreateCommandAllocator", error))?;
        let list = unsafe {
            device.CreateCommandList::<_, _, ID3D12GraphicsCommandList>(
                0,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                &allocator,
                None::<&ID3D12PipelineState>,
            )
        }
        .map_err(|error| windows_error("ID3D12Device::CreateCommandList", error))?;
        unsafe { list.Close() }
            .map_err(|error| windows_error("ID3D12GraphicsCommandList::Close", error))?;
        Ok(Self { allocator, list })
    }

    pub(super) fn record_transition(
        &mut self,
        resource: &ID3D12Resource,
        before: D3D12_RESOURCE_STATES,
        after: D3D12_RESOURCE_STATES,
    ) -> Result<(), D3D12NativeTextureError> {
        unsafe { self.allocator.Reset() }
            .map_err(|error| windows_error("ID3D12CommandAllocator::Reset", error))?;
        unsafe { self.list.Reset(&self.allocator, None::<&ID3D12PipelineState>) }
            .map_err(|error| windows_error("ID3D12GraphicsCommandList::Reset", error))?;
        let mut barrier = transition_barrier(resource, before, after);
        unsafe { self.list.ResourceBarrier(std::slice::from_ref(&barrier)) };
        release_barrier_resource_reference(&mut barrier);
        unsafe { self.list.Close() }
            .map_err(|error| windows_error("ID3D12GraphicsCommandList::Close", error))
    }

    pub(super) fn execute(
        &self,
        queue: &ID3D12CommandQueue,
    ) -> Result<(), D3D12NativeTextureError> {
        let command_list: ID3D12CommandList = self
            .list
            .cast()
            .map_err(|error| windows_error("QueryInterface<ID3D12CommandList>", error))?;
        unsafe { queue.ExecuteCommandLists(&[Some(command_list)]) };
        Ok(())
    }
}

pub(super) fn adopt_wgpu_video_texture(
    device: &wgpu::Device,
    resource: ID3D12Resource,
    inspection: D3D12NativeDecodedFrameInspection,
) -> Result<(wgpu::Texture, wgpu::TextureView, wgpu::TextureView), D3D12NativeTextureError> {
    let format = wgpu_texture_format(inspection.source_texture_format)?;
    let extent = wgpu::Extent3d {
        width: inspection.storage_width,
        height: inspection.storage_height,
        depth_or_array_layers: 1,
    };
    let hal_texture = unsafe {
        wgpu::hal::dx12::Device::texture_from_raw(
            resource,
            format,
            wgpu::TextureDimension::D2,
            extent,
            1,
            1,
        )
    };
    let descriptor = wgpu::TextureDescriptor {
        label: Some("mondrian.native-video.d3d12-decoder-surface"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    };
    let texture = unsafe {
        device.create_texture_from_hal::<wgpu::hal::api::Dx12>(
            hal_texture,
            &descriptor,
            wgpu::wgt::TextureUses::RESOURCE,
        )
    };
    let luma = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("mondrian.native-video.d3d12-luma"),
        format: Some(plane_format(format, wgpu::TextureAspect::Plane0)?),
        aspect: wgpu::TextureAspect::Plane0,
        ..wgpu::TextureViewDescriptor::default()
    });
    let chroma = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("mondrian.native-video.d3d12-chroma"),
        format: Some(plane_format(format, wgpu::TextureAspect::Plane1)?),
        aspect: wgpu::TextureAspect::Plane1,
        ..wgpu::TextureViewDescriptor::default()
    });
    Ok((texture, luma, chroma))
}

pub(super) fn validate_device_feature(
    device: &wgpu::Device,
    format: GpuNativeDecodedFrameTextureFormat,
) -> Result<(), D3D12NativeTextureError> {
    let (feature, name) = match format {
        GpuNativeDecodedFrameTextureFormat::Nv12 => {
            (wgpu::Features::TEXTURE_FORMAT_NV12, "TEXTURE_FORMAT_NV12")
        }
        GpuNativeDecodedFrameTextureFormat::P010 => {
            (wgpu::Features::TEXTURE_FORMAT_P010, "TEXTURE_FORMAT_P010")
        }
        GpuNativeDecodedFrameTextureFormat::P012
        | GpuNativeDecodedFrameTextureFormat::P016
        | GpuNativeDecodedFrameTextureFormat::P210
        | GpuNativeDecodedFrameTextureFormat::P212
        | GpuNativeDecodedFrameTextureFormat::P216
        | GpuNativeDecodedFrameTextureFormat::P410
        | GpuNativeDecodedFrameTextureFormat::P412
        | GpuNativeDecodedFrameTextureFormat::P416
        | GpuNativeDecodedFrameTextureFormat::Y210
        | GpuNativeDecodedFrameTextureFormat::Y212
        | GpuNativeDecodedFrameTextureFormat::Xv30
        | GpuNativeDecodedFrameTextureFormat::Xv36
        | GpuNativeDecodedFrameTextureFormat::Rgba8Unorm
        | GpuNativeDecodedFrameTextureFormat::Bgra8Unorm
        | GpuNativeDecodedFrameTextureFormat::Rgba16Float
        | GpuNativeDecodedFrameTextureFormat::Rgba32Float => {
            return Err(D3D12NativeTextureError::UnsupportedNativeTextureFormat { format });
        }
    };
    if !device.features().contains(feature) {
        return Err(D3D12NativeTextureError::DeviceFeatureMissing { feature: name });
    }
    Ok(())
}

pub(super) fn validate_device_limits(
    device: &wgpu::Device,
    inspection: D3D12NativeDecodedFrameInspection,
) -> Result<(), D3D12NativeTextureError> {
    let limit = device.limits().max_texture_dimension_2d;
    if inspection.storage_width > limit || inspection.storage_height > limit {
        return Err(D3D12NativeTextureError::DeviceTextureLimitExceeded {
            width: inspection.storage_width,
            height: inspection.storage_height,
            limit,
        });
    }
    Ok(())
}

fn wgpu_texture_format(
    format: GpuNativeDecodedFrameTextureFormat,
) -> Result<wgpu::TextureFormat, D3D12NativeTextureError> {
    match format {
        GpuNativeDecodedFrameTextureFormat::Nv12 => Ok(wgpu::TextureFormat::NV12),
        GpuNativeDecodedFrameTextureFormat::P010 => Ok(wgpu::TextureFormat::P010),
        _ => Err(D3D12NativeTextureError::UnsupportedNativeTextureFormat { format }),
    }
}

fn plane_format(
    format: wgpu::TextureFormat,
    aspect: wgpu::TextureAspect,
) -> Result<wgpu::TextureFormat, D3D12NativeTextureError> {
    format.aspect_specific_format(aspect).ok_or(
        D3D12NativeTextureError::UnsupportedNativeTextureFormat {
            format: match format {
                wgpu::TextureFormat::P010 => GpuNativeDecodedFrameTextureFormat::P010,
                _ => GpuNativeDecodedFrameTextureFormat::Nv12,
            },
        },
    )
}

fn transition_barrier(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

fn release_barrier_resource_reference(barrier: &mut D3D12_RESOURCE_BARRIER) {
    debug_assert_eq!(barrier.Type, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION);
    unsafe {
        let transition = &mut *barrier.Anonymous.Transition;
        ManuallyDrop::drop(&mut transition.pResource);
    }
}

fn windows_error(operation: &'static str, error: windows::core::Error) -> D3D12NativeTextureError {
    D3D12NativeTextureError::WindowsApi { operation, hresult: error.code().0 }
}
