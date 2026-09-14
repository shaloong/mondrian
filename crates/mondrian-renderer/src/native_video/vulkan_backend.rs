//! VA-API DRM PRIME and CUDA storage-buffer Vulkan native-video Adapters.

use std::sync::Arc;

use mondrian_media::{
    DecodedGpuFrameHandleKind, FfmpegDrmPrimeFrame, FfmpegDrmPrimeLayer, FfmpegDrmPrimePlane,
    FfmpegNativeDecodedFrameResource, PreviewNativeDecodedFrame,
};

use super::direct_backend::{
    DirectNativeVideoImportBackend, DirectNativeYuvInput, DirectNativeYuvPlaneAdapter,
    DirectNativeYuvTextures,
};
use crate::{
    GpuColorFrameResource, GpuColorFrameWgpuResource, GpuColorFrameWgpuResourcePool,
    GpuNativeDecodedFrameImportBackend, GpuNativeDecodedFrameImportError,
    GpuNativeDecodedFrameImportPlan, GpuNativeDecodedFrameImportSupport,
    GpuNativeDecodedFrameTextureFormat, NativeVideoImportCpuTimings,
};

const DRM_FORMAT_NV12: u32 = fourcc(b'N', b'V', b'1', b'2');
const DRM_FORMAT_P010: u32 = fourcc(b'P', b'0', b'1', b'0');
const DRM_FORMAT_P012: u32 = fourcc(b'P', b'0', b'1', b'2');
const DRM_FORMAT_R8: u32 = fourcc(b'R', b'8', b' ', b' ');
const DRM_FORMAT_R16: u32 = fourcc(b'R', b'1', b'6', b' ');
const DRM_FORMAT_RG88: u32 = fourcc(b'R', b'G', b'8', b'8');
const DRM_FORMAT_GR88: u32 = fourcc(b'G', b'R', b'8', b'8');
const DRM_FORMAT_RG1616: u32 = fourcc(b'R', b'G', b'3', b'2');

const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    u32::from_le_bytes([a, b, c, d])
}

/// Failure to bind a native-video Adapter to one wgpu Vulkan device.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum VulkanNativeVideoImportBackendCreateError {
    /// CUDA driver or Vulkan external-memory admission failed on the NVIDIA device.
    #[error("CUDA Vulkan import is unavailable: {reason}")]
    Cuda {
        /// Exact driver, identity, or required-extension rejection.
        reason: String,
    },
    /// The selected renderer is not Vulkan.
    #[error("native video import requires the wgpu Vulkan backend, got {backend}")]
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
    /// The driver cannot bind this Vulkan device to a DRM render node.
    #[error("Vulkan device has no usable DRM render-node identity: {reason}")]
    DrmIdentity {
        /// Concrete missing extension, node, or mismatched identity.
        reason: String,
    },
    /// Shared color execution could not be created.
    #[error("could not create shared native-video execution: {reason}")]
    Direct {
        /// Concrete shared-runtime failure.
        reason: String,
    },
}

/// Production VA-API DRM PRIME or NVIDIA CUDA native-video backend.
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
        if adapter.get_info().vendor == 0x10de {
            let cuda = super::vulkan_cuda::CudaPlaneAdapter::new(device).map_err(|error| {
                VulkanNativeVideoImportBackendCreateError::Cuda { reason: error.to_string() }
            })?;
            let plane_adapter =
                VulkanNativeYuvPlaneAdapter { support: cuda.support.clone(), cuda: Some(cuda) };
            return Ok(Self {
                inner: DirectNativeVideoImportBackend::new(
                    plane_adapter,
                    device,
                    queue,
                    resource_pool,
                )
                .map_err(|error| {
                    VulkanNativeVideoImportBackendCreateError::Direct { reason: error.to_string() }
                })?,
            });
        }
        if !device.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF) {
            return Err(VulkanNativeVideoImportBackendCreateError::MissingDmaBufFeature);
        }
        let selector = renderer_drm_selector(device)?;

        let mut formats = vec![GpuNativeDecodedFrameTextureFormat::Nv12];
        if device.features().contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM) {
            formats.extend([
                GpuNativeDecodedFrameTextureFormat::P010,
                GpuNativeDecodedFrameTextureFormat::P012,
            ]);
        }
        let support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
            vec![DecodedGpuFrameHandleKind::VaapiSurface],
            formats,
        )
        .with_renderer_backend_label("wgpu Vulkan VA-API DRM PRIME + OCIO")
        .with_hardware_decode_device_selector(selector);
        let plane_adapter = VulkanNativeYuvPlaneAdapter { support, cuda: None };
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

    pub(crate) fn prepare_import_plan(
        &mut self,
        plan: &GpuNativeDecodedFrameImportPlan,
    ) -> Result<(), GpuNativeDecodedFrameImportError> {
        self.inner.prepare_import_plan(plan)
    }

    /// Decoder surfaces retained until the import submission completes.
    /// Close native release admission and consume its worker after all owners return.
    pub fn poll_retirement(&mut self) -> Result<bool, GpuNativeDecodedFrameImportError> {
        self.inner.poll_retirement()
    }

    pub fn retained_source_count(&self) -> usize {
        self.inner.retained_source_count()
    }

    /// Wait until already released Linux native owners are consumed, without
    /// closing this runtime to subsequent frame admission.
    pub fn wait_for_released_sources_until(
        &self,
        deadline: std::time::Instant,
    ) -> Result<usize, GpuNativeDecodedFrameImportError> {
        self.inner.wait_for_released_sources_until(deadline)
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

fn renderer_drm_selector(
    device: &wgpu::Device,
) -> Result<mondrian_media::HwAccelDeviceSelector, VulkanNativeVideoImportBackendCreateError> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let unavailable =
        |reason: String| VulkanNativeVideoImportBackendCreateError::DrmIdentity { reason };
    // SAFETY: The guard retains the exact renderer device; only immutable
    // physical-device properties are queried and no native handle escapes.
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }
        .ok_or(VulkanNativeVideoImportBackendCreateError::MissingHalDevice)?;
    let instance = hal.shared_instance().raw_instance();
    let physical = hal.raw_physical_device();
    // SAFETY: physical belongs to this live instance. Vulkan permits these
    // read-only capability queries independently of queue submissions.
    let extensions = unsafe { instance.enumerate_device_extension_properties(physical) }
        .map_err(|error| unavailable(format!("device extension query failed: {error:?}")))?;
    let drm_supported = extensions.iter().any(|extension| {
        // SAFETY: Vulkan extensionName is a NUL-terminated fixed-size string.
        (unsafe { std::ffi::CStr::from_ptr(extension.extension_name.as_ptr()) })
            == ash::ext::physical_device_drm::NAME
    });
    if !drm_supported {
        return Err(unavailable(
            "VK_EXT_physical_device_drm is unavailable".to_owned(),
        ));
    }
    let mut drm = ash::vk::PhysicalDeviceDrmPropertiesEXT::default();
    let mut properties = ash::vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
    // SAFETY: The extension was advertised, the output chain is correctly
    // typed, and both output structures remain live for the entire query.
    unsafe { instance.get_physical_device_properties2(physical, &mut properties) };
    let minor = u32::try_from(drm.render_minor).ok().filter(|minor| *minor >= 128);
    let Some(minor) = minor.filter(|_| drm.has_render != 0 && drm.render_major == 226) else {
        return Err(unavailable(format!(
            "no supported DRM render node: present={}, major={}, minor={}",
            drm.has_render, drm.render_major, drm.render_minor,
        )));
    };
    let path = format!("/dev/dri/renderD{minor}");
    let metadata = std::fs::metadata(&path)
        .map_err(|error| unavailable(format!("cannot inspect {path}: {error}")))?;
    if !metadata.file_type().is_char_device()
        || libc::major(metadata.rdev()) != 226
        || libc::minor(metadata.rdev()) != minor
    {
        return Err(unavailable(format!(
            "{path} does not match the renderer DRM identity"
        )));
    }
    Ok(mondrian_media::HwAccelDeviceSelector::VaapiDrmRenderNode(
        minor,
    ))
}

struct VulkanNativeYuvPlaneAdapter {
    cuda: Option<super::vulkan_cuda::CudaPlaneAdapter>,
    support: GpuNativeDecodedFrameImportSupport,
}

impl DirectNativeYuvPlaneAdapter for VulkanNativeYuvPlaneAdapter {
    fn supports_buffer_source(&self) -> bool {
        self.cuda.is_some()
    }
    fn poll_retirement(&mut self) -> Result<bool, GpuNativeDecodedFrameImportError> {
        self.cuda.as_mut().map_or(Ok(true), |cuda| cuda.poll_retirement())
    }
    fn retained_owner_count(&self) -> usize {
        self.cuda.as_ref().map_or(0, |cuda| cuda.retained_owners())
    }
    fn wait_for_released_owners_until(
        &self,
        deadline: std::time::Instant,
    ) -> Result<(), GpuNativeDecodedFrameImportError> {
        self.cuda
            .as_ref()
            .map_or(Ok(()), |cuda| cuda.wait_for_released_owners_until(deadline))
    }
    fn import_input(
        &mut self,
        device: &wgpu::Device,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &PreviewNativeDecodedFrame,
    ) -> Result<DirectNativeYuvInput, GpuNativeDecodedFrameImportError> {
        if let Some(cuda) = &self.cuda {
            cuda.import(device, native_frame).map(DirectNativeYuvInput::Buffer)
        } else {
            self.import_textures(device, plan, native_frame)
                .map(DirectNativeYuvInput::Textures)
        }
    }

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
        let planes = validate_drm_layout(drm_frame.layers(), plan.source_texture_format)?;
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
        let chroma_height = match plan
            .source_texture_format
            .physical_descriptor()
            .and_then(|descriptor| descriptor.chroma_subsampling)
        {
            Some(mondrian_media::DecodedVideoSurfaceChromaSubsampling::Cs420) => {
                native_frame.height.div_ceil(2)
            }
            Some(mondrian_media::DecodedVideoSurfaceChromaSubsampling::Cs422) => {
                native_frame.height
            }
            _ => {
                return Err(rejected(format!(
                    "DRM PRIME Adapter cannot derive chroma extent for {:?}",
                    plan.source_texture_format
                )))
            }
        };
        let chroma = import_plane(
            device,
            drm_frame,
            planes[1],
            native_frame.width.div_ceil(2),
            chroma_height,
            chroma_format,
            "mondrian.native-video.vulkan-chroma",
        )?;
        Ok(DirectNativeYuvTextures { luma, chroma })
    }
}

fn validate_drm_layout(
    layers: &[FfmpegDrmPrimeLayer],
    source: GpuNativeDecodedFrameTextureFormat,
) -> Result<[FfmpegDrmPrimePlane; 2], GpuNativeDecodedFrameImportError> {
    let expected_fourcc = match source {
        GpuNativeDecodedFrameTextureFormat::Nv12 => DRM_FORMAT_NV12,
        GpuNativeDecodedFrameTextureFormat::P010 => DRM_FORMAT_P010,
        GpuNativeDecodedFrameTextureFormat::P012 => DRM_FORMAT_P012,
        other => {
            return Err(rejected(format!(
                "DRM PRIME Adapter does not support {other:?}"
            )))
        }
    };
    if let [luma, chroma] = layers {
        // FFmpeg requests VA_EXPORT_SURFACE_SEPARATE_LAYERS. Its NV12
        // mapping accepts R8 + GR88/RG88, and P010/P012 use R16 + RG1616.
        // These are plane-storage formats; UV order and effective bit depth
        // remain the admitted decoder surface contract, not RGB semantics.
        let formats_match = match source {
            GpuNativeDecodedFrameTextureFormat::Nv12 => {
                luma.format == DRM_FORMAT_R8
                    && matches!(chroma.format, DRM_FORMAT_GR88 | DRM_FORMAT_RG88)
            }
            GpuNativeDecodedFrameTextureFormat::P010 | GpuNativeDecodedFrameTextureFormat::P012 => {
                luma.format == DRM_FORMAT_R16 && chroma.format == DRM_FORMAT_RG1616
            }
            _ => false,
        };
        if !formats_match {
            return Err(rejected(format!(
                "DRM PRIME separate layer formats 0x{:08x}/0x{:08x} do not match {source:?}",
                luma.format, chroma.format
            )));
        }
        let ([luma], [chroma]) = (luma.planes.as_slice(), chroma.planes.as_slice()) else {
            return Err(rejected(format!(
                "DRM PRIME {source:?} separate layers must each contain exactly one plane"
            )));
        };
        return Ok([*luma, *chroma]);
    }
    if layers.len() != 1 {
        return Err(rejected(format!(
            "DRM PRIME frame requires one composed or two separate layers, got {}",
            layers.len()
        )));
    }
    let layer = &layers[0];
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
        GpuNativeDecodedFrameTextureFormat::P010 | GpuNativeDecodedFrameTextureFormat::P012 => (
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

#[cfg(test)]
mod drm_layout_tests {
    use super::*;

    fn planes() -> [FfmpegDrmPrimePlane; 2] {
        [
            FfmpegDrmPrimePlane { object_index: 0, offset: 128, pitch: 512 },
            FfmpegDrmPrimePlane { object_index: 1, offset: 256, pitch: 512 },
        ]
    }

    #[test]
    fn ffmpeg_vaapi_separate_layers_preserve_exact_plane_identity() {
        // libva's normal separate-layer NV12 export and FFmpeg's P010/P012
        // DRM mappings. These are descriptor contracts, not device qualification.
        for (source, luma, chroma) in [
            (
                GpuNativeDecodedFrameTextureFormat::Nv12,
                fourcc(b'R', b'8', b' ', b' '),
                fourcc(b'G', b'R', b'8', b'8'),
            ),
            (
                GpuNativeDecodedFrameTextureFormat::Nv12,
                fourcc(b'R', b'8', b' ', b' '),
                fourcc(b'R', b'G', b'8', b'8'),
            ),
            (
                GpuNativeDecodedFrameTextureFormat::P010,
                fourcc(b'R', b'1', b'6', b' '),
                fourcc(b'R', b'G', b'3', b'2'),
            ),
            (
                GpuNativeDecodedFrameTextureFormat::P012,
                fourcc(b'R', b'1', b'6', b' '),
                fourcc(b'R', b'G', b'3', b'2'),
            ),
        ] {
            let expected = planes();
            let layers = [
                FfmpegDrmPrimeLayer { format: luma, planes: vec![expected[0]] },
                FfmpegDrmPrimeLayer { format: chroma, planes: vec![expected[1]] },
            ];
            assert_eq!(
                validate_drm_layout(&layers, source).expect("valid FFmpeg split layers"),
                expected
            );
        }
    }

    #[test]
    fn composed_layers_remain_exact_and_malformed_layers_are_rejected() {
        let expected = planes();
        for (source, format) in [
            (GpuNativeDecodedFrameTextureFormat::Nv12, DRM_FORMAT_NV12),
            (GpuNativeDecodedFrameTextureFormat::P010, DRM_FORMAT_P010),
            (GpuNativeDecodedFrameTextureFormat::P012, DRM_FORMAT_P012),
        ] {
            let layers = [FfmpegDrmPrimeLayer { format, planes: expected.to_vec() }];
            assert_eq!(
                validate_drm_layout(&layers, source).expect("composed layer"),
                expected
            );
            assert!(validate_drm_layout(
                &[FfmpegDrmPrimeLayer { format, planes: vec![expected[0]] }],
                source
            )
            .is_err());
        }
        let source = GpuNativeDecodedFrameTextureFormat::Nv12;
        let luma = FfmpegDrmPrimeLayer {
            format: fourcc(b'R', b'8', b' ', b' '),
            planes: vec![expected[0]],
        };
        let chroma = FfmpegDrmPrimeLayer {
            format: fourcc(b'G', b'R', b'8', b'8'),
            planes: vec![expected[1]],
        };
        assert!(validate_drm_layout(&[], source).is_err());
        assert!(validate_drm_layout(&[chroma.clone(), luma.clone()], source).is_err());
        assert!(validate_drm_layout(
            &[luma.clone(), chroma.clone()],
            GpuNativeDecodedFrameTextureFormat::P010
        )
        .is_err());
        assert!(
            validate_drm_layout(&[luma.clone(), chroma.clone(), chroma.clone()], source).is_err()
        );
        let mut extra_plane = chroma.clone();
        extra_plane.planes.push(expected[0]);
        assert!(validate_drm_layout(&[luma.clone(), extra_plane], source).is_err());
        let mut missing_plane = chroma;
        missing_plane.planes.clear();
        assert!(validate_drm_layout(&[luma, missing_plane], source).is_err());
        assert!(validate_drm_layout(
            &[FfmpegDrmPrimeLayer { format: DRM_FORMAT_P010, planes: expected.to_vec() }],
            source
        )
        .is_err());
    }
}
