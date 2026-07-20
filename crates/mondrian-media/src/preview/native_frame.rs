use super::PreviewDecodeDiagnostics;
use crate::decoder::{
    DecodedFrameResidency, DecodedGpuFrameHandleKind, DecodedVideoChromaLocation,
    DecodedVideoRange, DecodedVideoSampling, DecodedVideoSurfaceFormat,
};
use ffmpeg_next as ffmpeg;
use std::any::Any;
use std::ffi::c_void;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU64;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// GPU-resident native decoded preview frame.
///
/// This is the media-layer payload contract for hardware decoders. The
/// concrete native handle stays behind backend-specific adapters; callers must
/// not reinterpret this as CPU RGBA.
#[derive(Debug, Clone)]
pub struct PreviewNativeDecodedFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Process-local native decoder handle token that owns the frame.
    pub handle: PreviewNativeDecodedFrameHandle,
    /// Decoder output surface format before renderer import.
    pub surface_format: DecodedVideoSurfaceFormat,
    /// Decode/cache diagnostics for this frame.
    pub diagnostics: PreviewDecodeDiagnostics,
}

impl PreviewNativeDecodedFrame {
    /// Create a GPU-resident native decoded frame payload.
    pub fn new(
        width: u32,
        height: u32,
        handle: PreviewNativeDecodedFrameHandle,
        surface_format: DecodedVideoSurfaceFormat,
        decoded_video_sampling: DecodedVideoSampling,
        mut diagnostics: PreviewDecodeDiagnostics,
    ) -> std::result::Result<Self, PreviewNativeDecodedFrameError> {
        if width == 0 || height == 0 {
            return Err(PreviewNativeDecodedFrameError::EmptyExtent { width, height });
        }
        if !surface_format.supports_native_gpu_payload() {
            return Err(PreviewNativeDecodedFrameError::UnsupportedSurfaceFormat {
                surface_format,
            });
        }
        validate_native_decoded_video_sampling(surface_format, decoded_video_sampling)?;
        diagnostics.cpu_resident = false;
        diagnostics.decoded_frame_residency = DecodedFrameResidency::GpuTexture;
        diagnostics.gpu_frame_handle_kind = Some(handle.kind());
        diagnostics.decoded_surface_format = surface_format;
        diagnostics.decoded_video_sampling = decoded_video_sampling;
        Ok(Self { width, height, handle, surface_format, diagnostics })
    }

    /// Native decoder handle family that owns the frame.
    pub fn handle_kind(&self) -> DecodedGpuFrameHandleKind {
        self.handle.kind()
    }
}

/// Backend-owned native decoder resource retained by a preview frame.
///
/// Implementations own the concrete decoder surface or registry lease. Dropping
/// the final handle clone must release that ownership according to the backend's
/// normal resource lifetime rules.
pub trait PreviewNativeDecodedFrameResource: fmt::Debug + Any + Send + Sync {
    /// Native decoder handle family exposed by this resource.
    fn handle_kind(&self) -> DecodedGpuFrameHandleKind;

    /// Process-local backend identity. This is never an OS handle.
    fn handle_id(&self) -> NonZeroU64;

    /// Type-erased access for the matching renderer import backend.
    fn as_any(&self) -> &dyn Any;
}

static NEXT_FFMPEG_NATIVE_FRAME_ID: AtomicU64 = AtomicU64::new(1);

/// FFmpeg-owned reference to one native hardware-decoded frame.
///
/// Construction retains the source frame with `av_frame_clone`, which in turn
/// retains its `AVBufferRef`-backed decoder surface. The final resource drop
/// releases that reference with `av_frame_free`.
pub struct FfmpegNativeDecodedFrameResource {
    frame: NonNull<ffmpeg::ffi::AVFrame>,
    pixel_format: ffmpeg::util::format::pixel::Pixel,
    kind: DecodedGpuFrameHandleKind,
    id: NonZeroU64,
}

// SAFETY: This resource has the same ownership and synchronization contract as
// ffmpeg-next's Frame, which explicitly implements Send and Sync. The AVFrame
// is immutable after retention and is released only when the final Arc drops.
unsafe impl Send for FfmpegNativeDecodedFrameResource {}
// SAFETY: See the Send implementation. Accessors expose immutable metadata and
// borrowed native handles; mutation remains owned by the decoder/backend.
unsafe impl Sync for FfmpegNativeDecodedFrameResource {}

impl FfmpegNativeDecodedFrameResource {
    /// Retain a hardware-decoded FFmpeg frame without copying its surface.
    pub fn retain(
        frame: &ffmpeg::util::frame::video::Video,
    ) -> std::result::Result<Self, FfmpegNativeDecodedFrameResourceError> {
        let pixel_format = frame.format();
        let kind = decoded_handle_kind_from_hardware_pixel(pixel_format).ok_or(
            FfmpegNativeDecodedFrameResourceError::UnsupportedPixelFormat { pixel_format },
        )?;
        let id = next_ffmpeg_native_frame_id()?;
        // SAFETY: frame.as_ptr() is valid for this borrow. av_frame_clone creates
        // an independently owned AVFrame whose buffer references are retained.
        let retained = unsafe { ffmpeg::ffi::av_frame_clone(frame.as_ptr()) };
        let frame = NonNull::new(retained)
            .ok_or(FfmpegNativeDecodedFrameResourceError::FrameReferenceAllocationFailed)?;
        Ok(Self { frame, pixel_format, kind, id })
    }

    /// Borrow the preferred FFmpeg D3D11 texture ABI view.
    pub fn d3d11_texture(
        &self,
    ) -> std::result::Result<FfmpegD3D11TextureView, FfmpegNativeDecodedFrameResourceError> {
        parse_ffmpeg_d3d11_texture(self.frame, self.pixel_format)
    }

    /// Borrow FFmpeg's D3D12 resource and decode-completion fence ABI.
    pub fn d3d12_texture(
        &self,
    ) -> std::result::Result<FfmpegD3D12TextureView, FfmpegNativeDecodedFrameResourceError> {
        parse_ffmpeg_d3d12_texture(self.frame, self.pixel_format)
    }

    /// Hardware pixel format retained by this frame.
    pub fn pixel_format(&self) -> ffmpeg::util::format::pixel::Pixel {
        self.pixel_format
    }

    #[cfg(test)]
    pub(super) fn retained_frame_ptr(&self) -> *mut ffmpeg::ffi::AVFrame {
        self.frame.as_ptr()
    }
}

impl fmt::Debug for FfmpegNativeDecodedFrameResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FfmpegNativeDecodedFrameResource")
            .field("pixel_format", &self.pixel_format())
            .field("kind", &self.kind)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Drop for FfmpegNativeDecodedFrameResource {
    fn drop(&mut self) {
        let mut frame = self.frame.as_ptr();
        // SAFETY: retain obtained sole ownership of this AVFrame allocation from
        // av_frame_clone. Drop runs exactly once and av_frame_free accepts &mut.
        unsafe { ffmpeg::ffi::av_frame_free(&mut frame) };
    }
}

impl PreviewNativeDecodedFrameResource for FfmpegNativeDecodedFrameResource {
    fn handle_kind(&self) -> DecodedGpuFrameHandleKind {
        self.kind
    }

    fn handle_id(&self) -> NonZeroU64 {
        self.id
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Borrowed view of FFmpeg's preferred D3D11 hardware-frame ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegD3D11TextureView {
    texture: NonNull<c_void>,
    array_slice: u32,
}

/// Borrowed view of FFmpeg's `AVD3D12VAFrame` resource and sync contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegD3D12TextureView {
    texture: NonNull<c_void>,
    fence: NonNull<c_void>,
    fence_value: u64,
}

impl FfmpegD3D12TextureView {
    /// Borrowed `ID3D12Resource` pointer owned by the retained AVFrame.
    pub fn texture_ptr(self) -> *mut c_void {
        self.texture.as_ptr()
    }

    /// Borrowed `ID3D12Fence` pointer signaling decode completion.
    pub fn fence_ptr(self) -> *mut c_void {
        self.fence.as_ptr()
    }

    /// Fence value that must complete before the resource is read.
    pub fn fence_value(self) -> u64 {
        self.fence_value
    }
}

impl FfmpegD3D11TextureView {
    /// Borrowed `ID3D11Texture2D` pointer stored in `AVFrame::data[0]`.
    pub fn texture_ptr(self) -> *mut c_void {
        self.texture.as_ptr()
    }

    /// Array-texture slice stored as `intptr_t` in `AVFrame::data[1]`.
    pub fn array_slice(self) -> u32 {
        self.array_slice
    }
}

/// Error retaining or interpreting an FFmpeg native decoded frame.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum FfmpegNativeDecodedFrameResourceError {
    /// The frame is not backed by a supported FFmpeg hardware pixel format.
    #[error("FFmpeg pixel format {pixel_format:?} is not a supported native decode surface")]
    UnsupportedPixelFormat {
        /// Unsupported source pixel format.
        pixel_format: ffmpeg::util::format::pixel::Pixel,
    },
    /// FFmpeg could not allocate a retained AVFrame reference.
    #[error("FFmpeg could not retain the native decoded frame reference")]
    FrameReferenceAllocationFailed,
    /// Process-local diagnostic handle identifiers were exhausted.
    #[error("process-local FFmpeg native frame identifiers are exhausted")]
    HandleIdExhausted,
    /// Only AV_PIX_FMT_D3D11 uses the preferred texture-plus-slice ABI.
    #[error("FFmpeg frame format {pixel_format:?} does not use the preferred D3D11 texture ABI")]
    NotPreferredD3D11Frame {
        /// Actual retained hardware pixel format.
        pixel_format: ffmpeg::util::format::pixel::Pixel,
    },
    /// A preferred D3D11 frame did not carry an ID3D11Texture2D pointer.
    #[error("FFmpeg D3D11 frame is missing its ID3D11Texture2D pointer")]
    MissingD3D11Texture,
    /// FFmpeg reported a D3D11 array slice that cannot fit Mondrian's contract.
    #[error("FFmpeg D3D11 array slice {array_slice} exceeds u32")]
    D3D11ArraySliceOverflow {
        /// FFmpeg `intptr_t` value interpreted as an unsigned index.
        array_slice: usize,
    },
    /// Only AV_PIX_FMT_D3D12 uses the `AVD3D12VAFrame` ABI.
    #[error("FFmpeg frame format {pixel_format:?} does not use the D3D12 resource ABI")]
    NotD3D12Frame {
        /// Actual retained hardware pixel format.
        pixel_format: ffmpeg::util::format::pixel::Pixel,
    },
    /// A D3D12 hardware frame did not carry its decoded texture.
    #[error("FFmpeg D3D12 frame is missing its ID3D12Resource pointer")]
    MissingD3D12Texture,
    /// A D3D12 hardware frame did not carry its decode-completion fence.
    #[error("FFmpeg D3D12 frame is missing its ID3D12Fence pointer")]
    MissingD3D12Fence,
}

fn next_ffmpeg_native_frame_id(
) -> std::result::Result<NonZeroU64, FfmpegNativeDecodedFrameResourceError> {
    let raw = NEXT_FFMPEG_NATIVE_FRAME_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| FfmpegNativeDecodedFrameResourceError::HandleIdExhausted)?;
    NonZeroU64::new(raw).ok_or(FfmpegNativeDecodedFrameResourceError::HandleIdExhausted)
}

fn decoded_handle_kind_from_hardware_pixel(
    pixel_format: ffmpeg::util::format::pixel::Pixel,
) -> Option<DecodedGpuFrameHandleKind> {
    use ffmpeg::util::format::pixel::Pixel;

    match pixel_format {
        Pixel::D3D12 => Some(DecodedGpuFrameHandleKind::D3D12Resource),
        Pixel::D3D11 | Pixel::D3D11VA_VLD => Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
        Pixel::DXVA2_VLD => Some(DecodedGpuFrameHandleKind::Dxva2Surface),
        Pixel::VIDEOTOOLBOX => Some(DecodedGpuFrameHandleKind::CVPixelBuffer),
        Pixel::VAAPI => Some(DecodedGpuFrameHandleKind::VaapiSurface),
        Pixel::VDPAU => Some(DecodedGpuFrameHandleKind::VdpauVideoSurface),
        Pixel::CUDA => Some(DecodedGpuFrameHandleKind::CudaDeviceMemory),
        _ => None,
    }
}

pub(super) fn parse_ffmpeg_d3d11_texture(
    frame: NonNull<ffmpeg::ffi::AVFrame>,
    pixel_format: ffmpeg::util::format::pixel::Pixel,
) -> std::result::Result<FfmpegD3D11TextureView, FfmpegNativeDecodedFrameResourceError> {
    // SAFETY: callers retain ownership of the AVFrame for the duration of this
    // function. FFmpeg documents data[0]/data[1] for AV_PIX_FMT_D3D11.
    let frame = unsafe { frame.as_ref() };
    if pixel_format != ffmpeg::util::format::pixel::Pixel::D3D11 {
        return Err(FfmpegNativeDecodedFrameResourceError::NotPreferredD3D11Frame { pixel_format });
    }
    let texture = NonNull::new(frame.data[0].cast::<c_void>())
        .ok_or(FfmpegNativeDecodedFrameResourceError::MissingD3D11Texture)?;
    let array_slice = frame.data[1] as usize;
    let array_slice = u32::try_from(array_slice).map_err(|_| {
        FfmpegNativeDecodedFrameResourceError::D3D11ArraySliceOverflow { array_slice }
    })?;
    Ok(FfmpegD3D11TextureView { texture, array_slice })
}

#[repr(C)]
pub(super) struct FfmpegAvD3D12VaSyncContext {
    pub(super) fence: *mut c_void,
    pub(super) event: *mut c_void,
    pub(super) fence_value: u64,
}

#[repr(C)]
pub(super) struct FfmpegAvD3D12VaFrame {
    pub(super) texture: *mut c_void,
    pub(super) sync_ctx: FfmpegAvD3D12VaSyncContext,
}

fn parse_ffmpeg_d3d12_texture(
    frame: NonNull<ffmpeg::ffi::AVFrame>,
    pixel_format: ffmpeg::util::format::pixel::Pixel,
) -> std::result::Result<FfmpegD3D12TextureView, FfmpegNativeDecodedFrameResourceError> {
    if pixel_format != ffmpeg::util::format::pixel::Pixel::D3D12 {
        return Err(FfmpegNativeDecodedFrameResourceError::NotD3D12Frame { pixel_format });
    }
    // SAFETY: the retained AVFrame owns data[0] for this borrow. FFmpeg 7.x+
    // defines AV_PIX_FMT_D3D12 data[0] as `AVD3D12VAFrame*`.
    let raw_frame = unsafe { frame.as_ref() };
    let native = NonNull::new(raw_frame.data[0].cast::<FfmpegAvD3D12VaFrame>())
        .ok_or(FfmpegNativeDecodedFrameResourceError::MissingD3D12Texture)?;
    // SAFETY: the data[0] ABI was validated above and remains AVFrame-owned.
    let native = unsafe { native.as_ref() };
    let texture = NonNull::new(native.texture)
        .ok_or(FfmpegNativeDecodedFrameResourceError::MissingD3D12Texture)?;
    let fence = NonNull::new(native.sync_ctx.fence)
        .ok_or(FfmpegNativeDecodedFrameResourceError::MissingD3D12Fence)?;
    Ok(FfmpegD3D12TextureView {
        texture,
        fence,
        fence_value: native.sync_ctx.fence_value,
    })
}

/// Shared lease for one backend-owned native decoder resource.
#[derive(Clone)]
pub struct PreviewNativeDecodedFrameHandle {
    resource: Arc<dyn PreviewNativeDecodedFrameResource>,
}

impl PreviewNativeDecodedFrameHandle {
    /// Retain a backend-owned native decoder resource.
    pub fn new<R>(resource: R) -> Self
    where
        R: PreviewNativeDecodedFrameResource,
    {
        Self { resource: Arc::new(resource) }
    }

    /// Native decoder handle family for this token.
    pub fn kind(&self) -> DecodedGpuFrameHandleKind {
        self.resource.handle_kind()
    }

    /// Process-local backend handle id.
    pub fn id(&self) -> NonZeroU64 {
        self.resource.handle_id()
    }

    /// Access the concrete resource only from its matching import backend.
    pub fn resource<R>(&self) -> Option<&R>
    where
        R: PreviewNativeDecodedFrameResource,
    {
        self.resource.as_any().downcast_ref()
    }
}

impl fmt::Debug for PreviewNativeDecodedFrameHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreviewNativeDecodedFrameHandle")
            .field("kind", &self.kind())
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}

impl PartialEq for PreviewNativeDecodedFrameHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.resource, &other.resource)
    }
}

impl Eq for PreviewNativeDecodedFrameHandle {}

impl Hash for PreviewNativeDecodedFrameHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let resource_identity = Arc::as_ptr(&self.resource) as *const ();
        resource_identity.hash(state);
    }
}

fn validate_native_decoded_video_sampling(
    surface_format: DecodedVideoSurfaceFormat,
    sampling: DecodedVideoSampling,
) -> std::result::Result<(), PreviewNativeDecodedFrameError> {
    if sampling.range == DecodedVideoRange::Unknown {
        return Err(PreviewNativeDecodedFrameError::MissingVideoRange { surface_format });
    }
    let expected_bit_depth = surface_format
        .fixed_bit_depth()
        .ok_or(PreviewNativeDecodedFrameError::UnsupportedSurfaceFormat { surface_format })?;
    if sampling.bit_depth != expected_bit_depth {
        return Err(PreviewNativeDecodedFrameError::BitDepthMismatch {
            surface_format,
            expected: expected_bit_depth,
            actual: sampling.bit_depth,
        });
    }
    if matches!(
        surface_format,
        DecodedVideoSurfaceFormat::Nv12 | DecodedVideoSurfaceFormat::P010
    ) && sampling.chroma_location == DecodedVideoChromaLocation::Unknown
    {
        return Err(PreviewNativeDecodedFrameError::MissingVideoChromaLocation { surface_format });
    }
    Ok(())
}

/// Error returned when constructing a native decoded preview frame payload.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum PreviewNativeDecodedFrameError {
    /// Native GPU preview frames require a non-empty extent.
    #[error("native decoded preview frame requires a non-empty extent, got {width}x{height}")]
    EmptyExtent {
        /// Frame width.
        width: u32,
        /// Frame height.
        height: u32,
    },
    /// The decoded surface format cannot be carried as a native GPU payload.
    #[error("decoded surface format {surface_format:?} cannot be carried as a native GPU payload")]
    UnsupportedSurfaceFormat {
        /// Unsupported decoded surface format.
        surface_format: DecodedVideoSurfaceFormat,
    },
    /// Native GPU preview payloads require explicit video range metadata.
    #[error(
        "native decoded preview frame {surface_format:?} requires explicit video range metadata"
    )]
    MissingVideoRange {
        /// Decoded surface format whose range metadata was missing.
        surface_format: DecodedVideoSurfaceFormat,
    },
    /// Subsampled native GPU preview payloads require explicit chroma siting.
    #[error("native decoded preview frame {surface_format:?} requires explicit chroma location metadata")]
    MissingVideoChromaLocation {
        /// Decoded surface format whose chroma metadata was missing.
        surface_format: DecodedVideoSurfaceFormat,
    },
    /// Native GPU preview payload bit depth must match its surface format.
    #[error("native decoded preview frame {surface_format:?} requires {expected}-bit sampling metadata, got {actual}")]
    BitDepthMismatch {
        /// Decoded surface format whose bit-depth metadata mismatched.
        surface_format: DecodedVideoSurfaceFormat,
        /// Required bit depth for the surface format.
        expected: u8,
        /// Reported bit depth.
        actual: u8,
    },
}
