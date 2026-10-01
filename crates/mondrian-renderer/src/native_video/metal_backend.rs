//! VideoToolbox CVPixelBuffer to Metal native-video Adapter.

use std::ptr::NonNull;
use std::sync::Arc;

use mondrian_media::{DecodedGpuFrameHandleKind, PreviewNativeDecodedFrame};
use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    kCVPixelFormatType_420YpCbCr10BiPlanarFullRange as CV_420_YPCBCR10_BIPLANAR_FULL_RANGE,
    kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange as CV_420_YPCBCR10_BIPLANAR_VIDEO_RANGE,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange as CV_420_YPCBCR8_BIPLANAR_FULL_RANGE,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as CV_420_YPCBCR8_BIPLANAR_VIDEO_RANGE,
    kCVPixelFormatType_422YpCbCr10BiPlanarFullRange as CV_422_YPCBCR10_BIPLANAR_FULL_RANGE,
    kCVPixelFormatType_422YpCbCr10BiPlanarVideoRange as CV_422_YPCBCR10_BIPLANAR_VIDEO_RANGE,
    kCVPixelFormatType_422YpCbCr16BiPlanarVideoRange as CV_422_YPCBCR16_BIPLANAR_VIDEO_RANGE,
    kCVPixelFormatType_444YpCbCr10BiPlanarFullRange as CV_444_YPCBCR10_BIPLANAR_FULL_RANGE,
    kCVPixelFormatType_444YpCbCr10BiPlanarVideoRange as CV_444_YPCBCR10_BIPLANAR_VIDEO_RANGE,
    kCVPixelFormatType_444YpCbCr16BiPlanarVideoRange as CV_444_YPCBCR16_BIPLANAR_VIDEO_RANGE,
    CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBuffer,
    CVPixelBufferGetHeight, CVPixelBufferGetHeightOfPlane, CVPixelBufferGetPixelFormatType,
    CVPixelBufferGetPlaneCount, CVPixelBufferGetWidth, CVPixelBufferGetWidthOfPlane,
};
use objc2_metal::{MTLPixelFormat, MTLTextureType};

use super::direct_backend::{
    DirectNativeVideoImportBackend, DirectNativeYuvPlaneAdapter, DirectNativeYuvTextures,
};
use crate::{
    GpuColorFrameResource, GpuColorFrameWgpuResource, GpuColorFrameWgpuResourcePool,
    GpuNativeDecodedFrameImportBackend, GpuNativeDecodedFrameImportError,
    GpuNativeDecodedFrameImportPlan, GpuNativeDecodedFrameImportSupport,
    GpuNativeDecodedFrameTextureFormat, NativeVideoImportCandidateTimingReceipt,
    NativeVideoImportCandidateToken, NativeVideoImportCpuTimings,
    NativeVideoImportGpuTimingDiagnostics, NativeVideoImportGpuTimingPolicy,
    NativeVideoImportGpuTimingSample,
};

/// Failure to bind the VideoToolbox Adapter to one wgpu Metal device.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum MetalNativeVideoImportBackendCreateError {
    /// The selected renderer is not Metal.
    #[error("native VideoToolbox import requires the wgpu Metal backend, got {backend}")]
    WrongBackend {
        /// Actual wgpu backend.
        backend: String,
    },
    /// wgpu did not expose its underlying Metal device.
    #[error("wgpu did not expose the active Metal device")]
    MissingHalDevice,
    /// CoreVideo could not create a texture cache for the active Metal device.
    #[error("CVMetalTextureCacheCreate failed with CVReturn {code}")]
    TextureCacheCreateFailed {
        /// CoreVideo result code.
        code: i32,
    },
    /// CoreVideo returned success without a cache object.
    #[error("CVMetalTextureCacheCreate returned no texture cache")]
    MissingTextureCache,
    /// Shared color execution could not be created.
    #[error("could not create shared native-video execution: {reason}")]
    Direct {
        /// Concrete shared-runtime failure.
        reason: String,
    },
}

/// Production VideoToolbox native-video backend.
pub struct MetalNativeVideoImportBackend {
    inner: DirectNativeVideoImportBackend<MetalNativeYuvPlaneAdapter>,
}

impl MetalNativeVideoImportBackend {
    /// Create a backend bound to one concrete wgpu Metal device.
    pub fn new_with_resource_pool(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
        gpu_timing_policy: NativeVideoImportGpuTimingPolicy,
    ) -> Result<Self, MetalNativeVideoImportBackendCreateError> {
        let backend = adapter.get_info().backend;
        if backend != wgpu::Backend::Metal {
            return Err(MetalNativeVideoImportBackendCreateError::WrongBackend {
                backend: format!("{backend:?}"),
            });
        }
        // SAFETY: The guard is used only while creating a CoreVideo cache bound
        // to this exact device; no raw handle escapes.
        let hal_device = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
            .ok_or(MetalNativeVideoImportBackendCreateError::MissingHalDevice)?;
        let mut cache_ptr = std::ptr::null_mut::<CVMetalTextureCache>();
        let cache_out = NonNull::from(&mut cache_ptr);
        // SAFETY: `cache_out` is a valid pointer-to-pointer and the raw Metal
        // device remains live for the complete call.
        let result = unsafe {
            CVMetalTextureCache::create(None, None, hal_device.raw_device(), None, cache_out)
        };
        if result != 0 {
            return Err(
                MetalNativeVideoImportBackendCreateError::TextureCacheCreateFailed { code: result },
            );
        }
        let cache = NonNull::new(cache_ptr)
            .ok_or(MetalNativeVideoImportBackendCreateError::MissingTextureCache)?;
        // SAFETY: CoreVideo returned a +1 cache reference on success.
        let cache = unsafe { CFRetained::from_raw(cache) };
        drop(hal_device);

        let mut formats = vec![GpuNativeDecodedFrameTextureFormat::Nv12];
        if device.features().contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM) {
            formats.extend([
                GpuNativeDecodedFrameTextureFormat::P010,
                GpuNativeDecodedFrameTextureFormat::P210,
                GpuNativeDecodedFrameTextureFormat::P216,
                GpuNativeDecodedFrameTextureFormat::P410,
                GpuNativeDecodedFrameTextureFormat::P416,
                GpuNativeDecodedFrameTextureFormat::Yuva444sp16,
            ]);
        }
        let support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
            vec![DecodedGpuFrameHandleKind::CVPixelBuffer],
            formats,
        )
        .with_renderer_backend_label("wgpu Metal VideoToolbox CVPixelBuffer + OCIO");
        let plane_adapter =
            MetalNativeYuvPlaneAdapter { cache: CvMetalTextureCacheHandle(cache), support };
        Ok(Self {
            inner: DirectNativeVideoImportBackend::new(
                plane_adapter,
                device,
                queue,
                resource_pool,
                gpu_timing_policy,
            )
            .map_err(|error| MetalNativeVideoImportBackendCreateError::Direct {
                reason: error.to_string(),
            })?,
        })
    }

    /// Actual native import capability for this Metal device.
    pub fn support(&self) -> &GpuNativeDecodedFrameImportSupport {
        self.inner.support()
    }

    /// CPU command-preparation attribution for the latest frame.
    pub fn frame_cpu_timings(&self) -> NativeVideoImportCpuTimings {
        self.inner.frame_cpu_timings()
    }

    pub(crate) fn begin_viewer_candidate(&mut self) -> Option<NativeVideoImportCandidateToken> {
        self.inner.begin_viewer_candidate()
    }

    pub(crate) fn end_viewer_candidate(
        &mut self,
        candidate: Option<NativeVideoImportCandidateToken>,
        succeeded: bool,
    ) -> Option<NativeVideoImportCandidateTimingReceipt> {
        self.inner.end_viewer_candidate(candidate, succeeded)
    }

    pub(crate) fn collect_gpu_timings_after_device_poll(&mut self) {
        self.inner.collect_gpu_timings_after_device_poll();
    }

    pub(crate) fn take_completed_gpu_timings(&mut self) -> Vec<NativeVideoImportGpuTimingSample> {
        self.inner.take_completed_gpu_timings()
    }

    pub(crate) fn gpu_timing_diagnostics(&self) -> NativeVideoImportGpuTimingDiagnostics {
        self.inner.gpu_timing_diagnostics()
    }

    pub(crate) fn take_input_backend_cache(
        &mut self,
    ) -> Option<crate::viewer_execution::NativeVideoInputBackendCache> {
        self.inner.take_input_backend_cache()
    }

    pub(crate) fn return_input_backend_cache(
        &mut self,
        cache: crate::viewer_execution::NativeVideoInputBackendCache,
    ) {
        self.inner.return_input_backend_cache(cache);
    }

    pub(crate) fn prepare_import_plan(
        &mut self,
        plan: &GpuNativeDecodedFrameImportPlan,
    ) -> Result<(), GpuNativeDecodedFrameImportError> {
        self.inner.prepare_import_plan(plan)
    }

    /// Decoder surfaces retained until the import submission completes.
    pub fn retained_source_count(&self) -> usize {
        self.inner.retained_source_count()
    }
}

impl GpuNativeDecodedFrameImportBackend for MetalNativeVideoImportBackend {
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

struct MetalNativeYuvPlaneAdapter {
    cache: CvMetalTextureCacheHandle,
    support: GpuNativeDecodedFrameImportSupport,
}

/// One retained CoreVideo texture cache that may cross execution threads.
///
/// CoreVideo objects are reference-counted with thread-safe retain/release,
/// and `CVMetalTextureCacheCreateTextureFromImage` may be called from any
/// thread. The wrapper exists only because objc2-core-video conservatively
/// marks `CVMetalTextureCache` `!Send`/`!Sync`; the operations Mondrian
/// performs through it are thread-safe by CoreVideo contract.
struct CvMetalTextureCacheHandle(CFRetained<CVMetalTextureCache>);

// SAFETY: retain/release and cache-to-texture creation are thread-safe
// CoreVideo operations; no other access is reachable through this type.
unsafe impl Send for CvMetalTextureCacheHandle {}
// SAFETY: See the Send implementation; access always goes through the
// thread-safe CoreVideo entry points.
unsafe impl Sync for CvMetalTextureCacheHandle {}

impl std::ops::Deref for CvMetalTextureCacheHandle {
    type Target = CVMetalTextureCache;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Carries one retained CoreVideo texture into the wgpu HAL drop callback.
///
/// The HAL may invoke its drop callback on the thread that destroys the
/// texture, which is not necessarily the thread that created the CoreVideo
/// object. CoreVideo reference counting is thread-safe and this wrapper's only
/// reachable operation is the final release, so transferring it is sound.
struct CvMetalTextureReleaseGuard {
    texture: CFRetained<CVMetalTexture>,
}

// SAFETY: The wrapper exposes no operation other than dropping the retained
// CoreVideo object, and CoreVideo retain/release is thread-safe.
unsafe impl Send for CvMetalTextureReleaseGuard {}
// SAFETY: See the Send implementation; no shared access is ever exposed.
unsafe impl Sync for CvMetalTextureReleaseGuard {}

impl CvMetalTextureReleaseGuard {
    /// Release the retained CoreVideo texture reference.
    ///
    /// A by-value method keeps closure capture on the whole guard, so the
    /// thread-safety wrapper cannot be bypassed by precise field capture.
    fn release(self) {
        drop(self.texture);
    }
}

impl DirectNativeYuvPlaneAdapter for MetalNativeYuvPlaneAdapter {
    fn support(&self) -> &GpuNativeDecodedFrameImportSupport {
        &self.support
    }

    fn import_textures(
        &mut self,
        device: &wgpu::Device,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &PreviewNativeDecodedFrame,
    ) -> Result<DirectNativeYuvTextures, GpuNativeDecodedFrameImportError> {
        if native_frame.handle_kind() != DecodedGpuFrameHandleKind::CVPixelBuffer {
            return Err(rejected(format!(
                "Metal import requires CVPixelBuffer, got {:?}",
                native_frame.handle_kind()
            )));
        }
        let surface = native_frame
            .handle
            .cv_pixel_buffer()
            .ok_or_else(|| rejected("CVPixelBuffer frame has no retained surface".to_owned()))?;
        let pixel_buffer_ptr = surface.as_ptr().cast::<CVPixelBuffer>();
        // SAFETY: The owner-bound borrow retains this immutable CVPixelBuffer
        // for the entire import call, regardless of the decoder implementation.
        let pixel_buffer = unsafe { pixel_buffer_ptr.as_ref() };
        let has_alpha =
            plan.source_texture_format == GpuNativeDecodedFrameTextureFormat::Yuva444sp16;
        if CVPixelBufferGetPlaneCount(pixel_buffer) != if has_alpha { 3 } else { 2 } {
            return Err(rejected(
                "CVPixelBuffer plane count does not match its YCbCr format".to_owned(),
            ));
        }
        let actual_format = CVPixelBufferGetPixelFormatType(pixel_buffer);
        let plane_formats = metal_plane_formats(plan.source_texture_format, actual_format)?;
        let plane_count = CVPixelBufferGetPlaneCount(pixel_buffer);
        let mut extents = [(0, 0); 3];
        for (plane, extent) in extents[..plane_count].iter_mut().enumerate() {
            *extent = (
                CVPixelBufferGetWidthOfPlane(pixel_buffer, plane),
                CVPixelBufferGetHeightOfPlane(pixel_buffer, plane),
            );
        }
        validate_plane_extents(
            plan.source_texture_format,
            (
                CVPixelBufferGetWidth(pixel_buffer),
                CVPixelBufferGetHeight(pixel_buffer),
            ),
            &extents[..plane_count],
        )?;
        let luma = wrap_plane(device, &self.cache, pixel_buffer, 0, plane_formats.0)?;
        let chroma = wrap_plane(device, &self.cache, pixel_buffer, 1, plane_formats.1)?;
        let alpha = if has_alpha {
            Some(wrap_plane(
                device,
                &self.cache,
                pixel_buffer,
                2,
                plane_formats.0,
            )?)
        } else {
            None
        };
        Ok(DirectNativeYuvTextures { luma, chroma, alpha })
    }
}

/// Validate the storage grid before wrapping textures. Visible cropping is
/// handled by the shared sampling plan; these dimensions describe the actual
/// CVPixelBuffer allocation, including its full-resolution alpha plane.
fn validate_plane_extents(
    format: GpuNativeDecodedFrameTextureFormat,
    storage: (usize, usize),
    planes: &[(usize, usize)],
) -> Result<(), GpuNativeDecodedFrameImportError> {
    use mondrian_media::DecodedVideoSurfaceChromaSubsampling::{Cs420, Cs422, Cs444};
    let descriptor = format
        .physical_descriptor()
        .ok_or_else(|| rejected("native YCbCr surface has no physical descriptor".to_owned()))?;
    let expected_chroma = match descriptor.chroma_subsampling {
        Some(Cs420) => (storage.0.div_ceil(2), storage.1.div_ceil(2)),
        Some(Cs422) => (storage.0.div_ceil(2), storage.1),
        Some(Cs444) => storage,
        None => {
            return Err(rejected(
                "native YCbCr surface has no chroma grid".to_owned(),
            ))
        }
    };
    let expected_count = if descriptor.has_alpha { 3 } else { 2 };
    if storage.0 == 0
        || storage.1 == 0
        || planes.len() != expected_count
        || planes[0] != storage
        || planes[1] != expected_chroma
        || (descriptor.has_alpha && planes[2] != storage)
    {
        return Err(rejected(format!(
            "CVPixelBuffer {format:?} plane extents {planes:?} do not match storage {storage:?}"
        )));
    }
    Ok(())
}

type MetalPlaneFormat = (MTLPixelFormat, wgpu::TextureFormat);
type MetalPlaneFormats = (MetalPlaneFormat, MetalPlaneFormat);

fn metal_plane_formats(
    source: GpuNativeDecodedFrameTextureFormat,
    actual: u32,
) -> Result<MetalPlaneFormats, GpuNativeDecodedFrameImportError> {
    match source {
        GpuNativeDecodedFrameTextureFormat::Nv12
            if matches!(
                actual,
                CV_420_YPCBCR8_BIPLANAR_VIDEO_RANGE | CV_420_YPCBCR8_BIPLANAR_FULL_RANGE
            ) =>
        {
            Ok((
                (MTLPixelFormat::R8Unorm, wgpu::TextureFormat::R8Unorm),
                (MTLPixelFormat::RG8Unorm, wgpu::TextureFormat::Rg8Unorm),
            ))
        }
        GpuNativeDecodedFrameTextureFormat::P010
            if matches!(
                actual,
                CV_420_YPCBCR10_BIPLANAR_VIDEO_RANGE | CV_420_YPCBCR10_BIPLANAR_FULL_RANGE
            ) =>
        {
            Ok((
                (MTLPixelFormat::R16Unorm, wgpu::TextureFormat::R16Unorm),
                (MTLPixelFormat::RG16Unorm, wgpu::TextureFormat::Rg16Unorm),
            ))
        }
        GpuNativeDecodedFrameTextureFormat::P210
            if matches!(
                actual,
                CV_422_YPCBCR10_BIPLANAR_VIDEO_RANGE | CV_422_YPCBCR10_BIPLANAR_FULL_RANGE
            ) =>
        {
            Ok((
                (MTLPixelFormat::R16Unorm, wgpu::TextureFormat::R16Unorm),
                (MTLPixelFormat::RG16Unorm, wgpu::TextureFormat::Rg16Unorm),
            ))
        }
        GpuNativeDecodedFrameTextureFormat::P216
            if actual == CV_422_YPCBCR16_BIPLANAR_VIDEO_RANGE =>
        {
            Ok((
                (MTLPixelFormat::R16Unorm, wgpu::TextureFormat::R16Unorm),
                (MTLPixelFormat::RG16Unorm, wgpu::TextureFormat::Rg16Unorm),
            ))
        }
        GpuNativeDecodedFrameTextureFormat::P410
            if matches!(
                actual,
                CV_444_YPCBCR10_BIPLANAR_VIDEO_RANGE | CV_444_YPCBCR10_BIPLANAR_FULL_RANGE
            ) =>
        {
            Ok((
                (MTLPixelFormat::R16Unorm, wgpu::TextureFormat::R16Unorm),
                (MTLPixelFormat::RG16Unorm, wgpu::TextureFormat::Rg16Unorm),
            ))
        }
        GpuNativeDecodedFrameTextureFormat::P416
            if actual == CV_444_YPCBCR16_BIPLANAR_VIDEO_RANGE =>
        {
            Ok((
                (MTLPixelFormat::R16Unorm, wgpu::TextureFormat::R16Unorm),
                (MTLPixelFormat::RG16Unorm, wgpu::TextureFormat::Rg16Unorm),
            ))
        }
        GpuNativeDecodedFrameTextureFormat::Yuva444sp16
            if actual == u32::from_be_bytes(*b"s4as") =>
        {
            Ok((
                (MTLPixelFormat::R16Unorm, wgpu::TextureFormat::R16Unorm),
                (MTLPixelFormat::RG16Unorm, wgpu::TextureFormat::Rg16Unorm),
            ))
        }
        _ => Err(rejected(format!(
            "CVPixelBuffer format 0x{actual:08x} does not match {source:?}"
        ))),
    }
}

fn wrap_plane(
    device: &wgpu::Device,
    cache: &CVMetalTextureCache,
    pixel_buffer: &CVPixelBuffer,
    plane: usize,
    formats: (MTLPixelFormat, wgpu::TextureFormat),
) -> Result<wgpu::Texture, GpuNativeDecodedFrameImportError> {
    let width = CVPixelBufferGetWidthOfPlane(pixel_buffer, plane);
    let height = CVPixelBufferGetHeightOfPlane(pixel_buffer, plane);
    if width == 0 || height == 0 {
        return Err(rejected(format!(
            "CVPixelBuffer plane {plane} has an empty extent"
        )));
    }
    let width_u32 = u32::try_from(width)
        .map_err(|_| rejected(format!("CVPixelBuffer plane {plane} width exceeds u32")))?;
    let height_u32 = u32::try_from(height)
        .map_err(|_| rejected(format!("CVPixelBuffer plane {plane} height exceeds u32")))?;
    let mut cv_texture_ptr = std::ptr::null_mut::<CVMetalTexture>();
    // SAFETY: All references remain live, and `cv_texture_ptr` is a valid
    // pointer-to-pointer receiving a +1 CoreVideo object.
    let result = unsafe {
        CVMetalTextureCache::create_texture_from_image(
            None,
            cache,
            pixel_buffer,
            None,
            formats.0,
            width,
            height,
            plane,
            NonNull::from(&mut cv_texture_ptr),
        )
    };
    if result != 0 {
        return Err(rejected(format!(
            "CVMetalTextureCache plane {plane} creation failed with CVReturn {result}"
        )));
    }
    let cv_texture_ptr = NonNull::new(cv_texture_ptr)
        .ok_or_else(|| rejected(format!("CoreVideo returned no plane {plane} texture")))?;
    // SAFETY: CoreVideo returned a +1 reference.
    let cv_texture = unsafe { CFRetained::from_raw(cv_texture_ptr) };
    let metal_texture = CVMetalTextureGetTexture(&cv_texture)
        .ok_or_else(|| rejected(format!("CVMetalTexture plane {plane} has no MTLTexture")))?;
    // SAFETY: The raw texture belongs to this exact wgpu Metal device. The
    // drop callback retains the CVMetalTexture until wgpu releases its final
    // reference; wgpu never destroys the external MTLTexture itself.
    let hal_device = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
        .ok_or_else(|| rejected("wgpu Metal HAL device disappeared".to_owned()))?;
    let release_guard = CvMetalTextureReleaseGuard { texture: cv_texture };
    let hal_texture = unsafe {
        wgpu::hal::metal::Device::texture_from_raw(
            metal_texture,
            formats.1,
            MTLTextureType::Type2D,
            1,
            1,
            wgpu::hal::CopyExtent { width: width_u32, height: height_u32, depth: 1 },
            Some(Box::new(move || release_guard.release())),
        )
    };
    drop(hal_device);
    let descriptor = wgpu::TextureDescriptor {
        label: Some("mondrian.native-video.metal-plane"),
        size: wgpu::Extent3d {
            width: width_u32,
            height: height_u32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: formats.1,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    };
    // SAFETY: The HAL texture was created from this exact device, matches the
    // descriptor, and VideoToolbox/CoreVideo published initialized contents.
    Ok(unsafe {
        device.create_texture_from_hal::<wgpu::hal::api::Metal>(
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
mod tests {
    use super::*;

    #[test]
    fn metal_plane_extents_follow_sampling_and_alpha_grid() {
        let storage = (1921, 1081);
        for (format, planes) in [
            (
                GpuNativeDecodedFrameTextureFormat::P010,
                vec![storage, (961, 541)],
            ),
            (
                GpuNativeDecodedFrameTextureFormat::P210,
                vec![storage, (961, 1081)],
            ),
            (
                GpuNativeDecodedFrameTextureFormat::P416,
                vec![storage, storage],
            ),
            (
                GpuNativeDecodedFrameTextureFormat::Yuva444sp16,
                vec![storage; 3],
            ),
        ] {
            validate_plane_extents(format, storage, &planes).expect("correct storage grid");
        }
        let format = GpuNativeDecodedFrameTextureFormat::Yuva444sp16;
        for planes in [
            vec![storage, storage],
            vec![storage, (961, 1081), storage],
            vec![storage, storage, (961, 541)],
            vec![(1920, 1080), storage, storage],
        ] {
            assert!(validate_plane_extents(format, storage, &planes).is_err());
        }
        assert!(validate_plane_extents(format, (0, 1081), &[(0, 1081); 3]).is_err());
    }

    #[test]
    fn metal_s4as_cannot_be_imported_as_packed_or_alpha_free_yuv() {
        let s4as = u32::from_be_bytes(*b"s4as");
        assert!(metal_plane_formats(GpuNativeDecodedFrameTextureFormat::Yuva444sp16, s4as).is_ok());
        assert!(metal_plane_formats(GpuNativeDecodedFrameTextureFormat::P416, s4as).is_err());
        assert!(metal_plane_formats(
            GpuNativeDecodedFrameTextureFormat::Yuva444sp16,
            u32::from_be_bytes(*b"y416"),
        )
        .is_err());
    }
}
