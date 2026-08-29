//! VA-API DRM PRIME to Vulkan native-video Adapter.

use std::sync::Arc;

use mondrian_media::{
    DecodedGpuFrameHandleKind, FfmpegDrmPrimeFrame, FfmpegDrmPrimePlane,
    FfmpegNativeDecodedFrameResource, PreviewNativeDecodedFrame,
};

use super::direct_backend::{
    DirectNativeVideoImportBackend, DirectNativeYuvPlaneAdapter, DirectNativeYuvTextures,
};
use crate::{
    GpuColorFrameResource, GpuColorFrameWgpuResource, GpuColorFrameWgpuResourcePool,
    GpuNativeDecodedFrameImportBackend, GpuNativeDecodedFrameImportError,
    GpuNativeDecodedFrameImportPlan, GpuNativeDecodedFrameImportSupport,
    GpuNativeDecodedFrameTextureFormat, NativeVideoImportCpuTimings,
};

const DRM_FORMAT_NV12: u32 = fourcc(b'N', b'V', b'1', b'2');
const DRM_FORMAT_P010: u32 = fourcc(b'P', b'0', b'1', b'0');

const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    u32::from_le_bytes([a, b, c, d])
}

/// Failure to bind the VA-API Adapter to one wgpu Vulkan device.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum VulkanNativeVideoImportBackendCreateError {
    /// The selected renderer is not Vulkan.
    #[error("native VA-API import requires the wgpu Vulkan backend, got {backend}")]
    WrongBackend {
        /// Actual wgpu backend.
        backend: String,
    },
    /// The Vulkan device was not created with DMA-BUF external-memory support.
    #[error("wgpu Vulkan device has no VULKAN_EXTERNAL_MEMORY_DMA_BUF feature")]
    MissingDmaBufFeature,
    /// wgpu did not expose its underlying Vulkan device.
    #[error("wgpu did not expose the active Vulkan device")]
    MissingHalDevice,
    /// Shared color execution could not be created.
    #[error("could not create shared native-video execution: {reason}")]
    Direct {
        /// Concrete shared-runtime failure.
        reason: String,
    },
}

/// Production VA-API DRM PRIME native-video backend.
pub struct VulkanNativeVideoImportBackend {
    inner: DirectNativeVideoImportBackend<VulkanNativeYuvPlaneAdapter>,
}

impl VulkanNativeVideoImportBackend {
    /// Create a backend bound to one concrete wgpu Vulkan device.
    pub fn new_with_resource_pool(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    ) -> Result<Self, VulkanNativeVideoImportBackendCreateError> {
        let backend = adapter.get_info().backend;
        if backend != wgpu::Backend::Vulkan {
            return Err(VulkanNativeVideoImportBackendCreateError::WrongBackend {
                backend: format!("{backend:?}"),
            });
        }
        if !device.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF) {
            return Err(VulkanNativeVideoImportBackendCreateError::MissingDmaBufFeature);
        }
        // SAFETY: The guard is used only to prove this exact device exposes its
        // Vulkan HAL implementation; no raw handle escapes.
        if unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }.is_none() {
            return Err(VulkanNativeVideoImportBackendCreateError::MissingHalDevice);
        }

        let mut formats = vec![GpuNativeDecodedFrameTextureFormat::Nv12];
        if device.features().contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM) {
            formats.push(GpuNativeDecodedFrameTextureFormat::P010);
        }
        let support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
            vec![DecodedGpuFrameHandleKind::VaapiSurface],
            formats,
        )
        .with_renderer_backend_label("wgpu Vulkan VA-API DRM PRIME + OCIO");
        let plane_adapter = VulkanNativeYuvPlaneAdapter { support };
        Ok(Self {
            inner: DirectNativeVideoImportBackend::new(plane_adapter, device, queue, resource_pool)
                .map_err(|error| VulkanNativeVideoImportBackendCreateError::Direct {
                    reason: error.to_string(),
                })?,
        })
    }

    /// Actual native import capability for this Vulkan device.
    pub fn support(&self) -> &GpuNativeDecodedFrameImportSupport {
        self.inner.support()
    }

    /// CPU command-preparation attribution for the latest frame.
    pub fn frame_cpu_timings(&self) -> NativeVideoImportCpuTimings {
        self.inner.frame_cpu_timings()
    }

    /// Decoder surfaces retained until the import submission completes.
    pub fn retained_source_count(&self) -> usize {
        self.inner.retained_source_count()
    }
}

impl GpuNativeDecodedFrameImportBackend for VulkanNativeVideoImportBackend {
    type NativeFrame = PreviewNativeDecodedFrame;
    type Resource = GpuColorFrameWgpuResource;

    fn support(&self) -> &GpuNativeDecodedFrameImportSupport {
        self.support()
    }

    fn import_native_decoded_frame(
        &mut self,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &Self::NativeFrame,
    ) -> Result<GpuColorFrameResource<Self::Resource>, GpuNativeDecodedFrameImportError> {
        self.inner.import_native_decoded_frame(plan, native_frame)
    }
}

struct VulkanNativeYuvPlaneAdapter {
    support: GpuNativeDecodedFrameImportSupport,
}

impl DirectNativeYuvPlaneAdapter for VulkanNativeYuvPlaneAdapter {
    fn support(&self) -> &GpuNativeDecodedFrameImportSupport {
        &self.support
    }

    fn import_textures(
        &mut self,
        device: &wgpu::Device,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &PreviewNativeDecodedFrame,
    ) -> Result<DirectNativeYuvTextures, GpuNativeDecodedFrameImportError> {
        if native_frame.handle_kind() != DecodedGpuFrameHandleKind::VaapiSurface {
            return Err(rejected(format!(
                "Vulkan import requires a VA-API surface, got {:?}",
                native_frame.handle_kind()
            )));
        }
        let resource = native_frame
            .handle
            .resource::<FfmpegNativeDecodedFrameResource>()
            .ok_or_else(|| rejected("VA-API frame has no retained FFmpeg resource".to_owned()))?;
        let drm_frame = resource.drm_prime_frame().map_err(|error| rejected(error.to_string()))?;
        let planes = validate_drm_layout(drm_frame, plan.source_texture_format)?;
        let (luma_format, chroma_format) = plane_formats(plan.source_texture_format);
        let luma = import_plane(
            device,
            drm_frame,
            planes[0],
            native_frame.width,
            native_frame.height,
            luma_format,
            "mondrian.native-video.vulkan-luma",
        )?;
        let chroma = import_plane(
            device,
            drm_frame,
            planes[1],
            native_frame.width.div_ceil(2),
            native_frame.height.div_ceil(2),
            chroma_format,
            "mondrian.native-video.vulkan-chroma",
        )?;
        Ok(DirectNativeYuvTextures { luma, chroma })
    }
}

fn validate_drm_layout(
    frame: &FfmpegDrmPrimeFrame,
    source: GpuNativeDecodedFrameTextureFormat,
) -> Result<[FfmpegDrmPrimePlane; 2], GpuNativeDecodedFrameImportError> {
    let expected_fourcc = match source {
        GpuNativeDecodedFrameTextureFormat::Nv12 => DRM_FORMAT_NV12,
        GpuNativeDecodedFrameTextureFormat::P010 => DRM_FORMAT_P010,
        other => {
            return Err(rejected(format!(
                "DRM PRIME Adapter does not support {other:?}"
            )))
        }
    };
    if frame.layers().len() != 1 {
        return Err(rejected(format!(
            "DRM PRIME frame must expose one typed NV12/P010 layer, got {}",
            frame.layers().len()
        )));
    }
    let layer = &frame.layers()[0];
    if layer.format != expected_fourcc {
        return Err(rejected(format!(
            "DRM PRIME FourCC 0x{:08x} does not match {source:?}",
            layer.format
        )));
    }
    let [luma, chroma] = layer.planes.as_slice() else {
        return Err(rejected(format!(
            "DRM PRIME {source:?} layer must have exactly two planes, got {}",
            layer.planes.len()
        )));
    };
    Ok([*luma, *chroma])
}

fn plane_formats(
    source: GpuNativeDecodedFrameTextureFormat,
) -> (wgpu::TextureFormat, wgpu::TextureFormat) {
    match source {
        GpuNativeDecodedFrameTextureFormat::P010 => (
            wgpu::TextureFormat::R16Unorm,
            wgpu::TextureFormat::Rg16Unorm,
        ),
        _ => (wgpu::TextureFormat::R8Unorm, wgpu::TextureFormat::Rg8Unorm),
    }
}

#[allow(clippy::too_many_arguments)]
fn import_plane(
    device: &wgpu::Device,
    frame: &FfmpegDrmPrimeFrame,
    plane: FfmpegDrmPrimePlane,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    label: &'static str,
) -> Result<wgpu::Texture, GpuNativeDecodedFrameImportError> {
    let object = frame
        .objects()
        .get(plane.object_index)
        .ok_or_else(|| rejected("DRM PRIME plane object index is out of range".to_owned()))?;
    let rows_bytes = plane
        .pitch
        .checked_mul(u64::from(height))
        .and_then(|bytes| plane.offset.checked_add(bytes))
        .ok_or_else(|| rejected("DRM PRIME plane byte range overflowed".to_owned()))?;
    if rows_bytes > object.size() as u64 {
        return Err(rejected(format!(
            "DRM PRIME plane range {rows_bytes} exceeds object size {}",
            object.size()
        )));
    }
    let fd = frame
        .duplicate_object_fd(plane.object_index)
        .map_err(|error| rejected(error.to_string()))?;
    let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
    let hal_descriptor = wgpu::hal::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::wgt::TextureUses::RESOURCE,
        memory_flags: wgpu::hal::MemoryFlags::empty(),
        view_formats: Vec::new(),
    };
    // SAFETY: The Adapter validated the exact FFmpeg object, byte range,
    // modifier, pitch, and plane extent. The duplicated descriptor transfers
    // ownership to Vulkan on success and is closed by wgpu-hal on failure.
    let hal_device = unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }
        .ok_or_else(|| rejected("wgpu Vulkan HAL device disappeared".to_owned()))?;
    let hal_texture = unsafe {
        hal_device.texture_from_dmabuf_fd(
            fd,
            &hal_descriptor,
            object.format_modifier(),
            plane.pitch,
            plane.offset,
        )
    }
    .map_err(|error| rejected(format!("Vulkan DMA-BUF plane import failed: {error:?}")))?;
    drop(hal_device);
    let descriptor = wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    };
    // SAFETY: The HAL texture belongs to this exact device and matches the
    // public descriptor. AV_HWFRAME_MAP_READ publishes decoder completion and
    // wgpu owns subsequent Vulkan layout transitions.
    Ok(unsafe {
        device.create_texture_from_hal::<wgpu::hal::api::Vulkan>(
            hal_texture,
            &descriptor,
            wgpu::wgt::TextureUses::RESOURCE,
        )
    })
}

fn rejected(reason: String) -> GpuNativeDecodedFrameImportError {
    GpuNativeDecodedFrameImportError::BackendRejected { reason }
}
