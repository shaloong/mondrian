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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::OnceLock;

/// Exact outstanding-native-output count for one decode ownership domain.
///
/// A worker family owns one shared tracker and every decoder Session owns one
/// local tracker. A native output lease charges both, so Session-slot reuse can
/// wait for its local count while family retirement waits for the total count.
/// The counter itself is strongly retained by every lease; dropping a Session
/// or context therefore cannot erase outstanding-output evidence.
#[derive(Clone, Debug, Default)]
pub(super) struct PreviewNativeOutputTracker {
    outstanding: Arc<AtomicUsize>,
}

impl PreviewNativeOutputTracker {
    pub(super) fn outstanding(&self) -> usize {
        self.outstanding.load(Ordering::Acquire)
    }

    pub(super) fn is_released(&self) -> bool {
        self.outstanding() == 0
    }

    fn try_increment(&self) -> Result<(), PreviewNativeOutputLeaseError> {
        self.outstanding
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map(|_| ())
            .map_err(|_| PreviewNativeOutputLeaseError::CounterExhausted)
    }

    fn decrement(&self) {
        let previous = self.outstanding.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "native-output tracker underflow");
    }
}

/// One logical native decoder output shared by all payload/renderer clones.
///
/// Cloning the lease shares one RAII owner and does not charge either tracker
/// again. Creating the next logical output always creates a new lease and adds
/// exactly one family charge plus one Session-local charge.
#[derive(Debug, Clone)]
pub(super) struct PreviewDecodeSessionOutputLease {
    _owner: Arc<PreviewDecodeSessionOutputLeaseInner>,
}

#[derive(Debug)]
struct PreviewDecodeSessionOutputLeaseInner {
    family: PreviewNativeOutputTracker,
    session: PreviewNativeOutputTracker,
}

#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq, Eq)]
pub(super) enum PreviewNativeOutputLeaseError {
    #[error("native decoded-output counter is exhausted")]
    CounterExhausted,
}

impl PreviewDecodeSessionOutputLease {
    pub(super) fn acquire(
        family: &PreviewNativeOutputTracker,
        session: &PreviewNativeOutputTracker,
    ) -> Result<Self, PreviewNativeOutputLeaseError> {
        family.try_increment()?;
        if let Err(error) = session.try_increment() {
            family.decrement();
            return Err(error);
        }
        Ok(Self {
            _owner: Arc::new(PreviewDecodeSessionOutputLeaseInner {
                family: family.clone(),
                session: session.clone(),
            }),
        })
    }
}

impl Drop for PreviewDecodeSessionOutputLeaseInner {
    fn drop(&mut self) {
        self.session.decrement();
        self.family.decrement();
    }
}

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
    _session_output_lease: Option<PreviewDecodeSessionOutputLease>,
    #[cfg(target_os = "linux")]
    drm_prime_frame:
        OnceLock<std::result::Result<FfmpegDrmPrimeFrame, FfmpegNativeDecodedFrameResourceError>>,
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
        Self::retain_with_session_output_lease(frame, None)
    }

    pub(super) fn retain_with_session_output_lease(
        frame: &ffmpeg::util::frame::video::Video,
        session_output_lease: Option<PreviewDecodeSessionOutputLease>,
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
        Ok(Self {
            frame,
            pixel_format,
            kind,
            id,
            _session_output_lease: session_output_lease,
            #[cfg(target_os = "linux")]
            drm_prime_frame: OnceLock::new(),
        })
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

    /// Borrow the `CVPixelBufferRef` retained by a VideoToolbox frame.
    ///
    /// The pointer remains valid only while this resource is retained. A Metal
    /// Adapter must retain its own CVMetalTexture/MTLTexture view before
    /// releasing the native decoded-frame handle.
    pub fn cv_pixel_buffer(
        &self,
    ) -> std::result::Result<NonNull<c_void>, FfmpegNativeDecodedFrameResourceError> {
        parse_ffmpeg_cv_pixel_buffer(self.frame, self.pixel_format)
    }

    /// Map one VA-API frame to an owned DRM PRIME descriptor without a CPU
    /// pixel transfer.
    #[cfg(target_os = "linux")]
    pub fn drm_prime_frame(
        &self,
    ) -> std::result::Result<&FfmpegDrmPrimeFrame, FfmpegNativeDecodedFrameResourceError> {
        self.drm_prime_frame
            .get_or_init(|| FfmpegDrmPrimeFrame::map(self.frame, self.pixel_format))
            .as_ref()
            .map_err(Clone::clone)
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
    /// Only VideoToolbox frames carry a `CVPixelBufferRef` in `data[3]`.
    #[error(
        "FFmpeg frame format {pixel_format:?} does not use the VideoToolbox CVPixelBuffer ABI"
    )]
    NotVideoToolboxFrame {
        /// Actual retained hardware pixel format.
        pixel_format: ffmpeg::util::format::pixel::Pixel,
    },
    /// A VideoToolbox frame did not carry its `CVPixelBufferRef`.
    #[error("FFmpeg VideoToolbox frame is missing its CVPixelBufferRef")]
    MissingCvPixelBuffer,
    /// Only VA-API frames can be mapped to the Linux DRM PRIME Adapter.
    #[error("FFmpeg frame format {pixel_format:?} cannot be mapped as a VA-API DRM PRIME frame")]
    NotVaapiFrame {
        /// Actual retained hardware pixel format.
        pixel_format: ffmpeg::util::format::pixel::Pixel,
    },
    /// FFmpeg failed to allocate a destination frame for DRM PRIME mapping.
    #[error("FFmpeg could not allocate a DRM PRIME mapping frame")]
    DrmPrimeFrameAllocationFailed,
    /// FFmpeg could not map the retained VA-API frame as DRM PRIME.
    #[error("FFmpeg av_hwframe_map to DRM PRIME failed with code {code}")]
    DrmPrimeMapFailed {
        /// Negative FFmpeg error code.
        code: i32,
    },
    /// FFmpeg returned an incomplete or internally inconsistent DRM descriptor.
    #[error("FFmpeg DRM PRIME descriptor is invalid: {reason}")]
    InvalidDrmPrimeDescriptor {
        /// Validation failure.
        reason: String,
    },
    /// A DMA-BUF descriptor could not be duplicated for Vulkan ownership.
    #[error("could not duplicate DRM PRIME object {object_index} file descriptor: {reason}")]
    DrmPrimeFileDescriptorDuplicationFailed {
        /// Object index in the FFmpeg descriptor.
        object_index: usize,
        /// Operating-system error.
        reason: String,
    },
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

fn parse_ffmpeg_cv_pixel_buffer(
    frame: NonNull<ffmpeg::ffi::AVFrame>,
    pixel_format: ffmpeg::util::format::pixel::Pixel,
) -> std::result::Result<NonNull<c_void>, FfmpegNativeDecodedFrameResourceError> {
    if pixel_format != ffmpeg::util::format::pixel::Pixel::VIDEOTOOLBOX {
        return Err(FfmpegNativeDecodedFrameResourceError::NotVideoToolboxFrame { pixel_format });
    }
    // SAFETY: The retained AVFrame is alive for this borrow. FFmpeg documents
    // AV_PIX_FMT_VIDEOTOOLBOX data[3] as its retained CVPixelBufferRef.
    let frame = unsafe { frame.as_ref() };
    NonNull::new(frame.data[3].cast::<c_void>())
        .ok_or(FfmpegNativeDecodedFrameResourceError::MissingCvPixelBuffer)
}

/// One DRM PRIME object retained by an FFmpeg mapping.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegDrmPrimeObject {
    fd: std::os::fd::RawFd,
    size: usize,
    format_modifier: u64,
}

#[cfg(target_os = "linux")]
impl FfmpegDrmPrimeObject {
    /// Total bytes in the DMA-BUF object.
    pub fn size(self) -> usize {
        self.size
    }

    /// DRM format modifier for this object.
    pub fn format_modifier(self) -> u64 {
        self.format_modifier
    }
}

/// One plane within a DRM PRIME layer.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegDrmPrimePlane {
    /// Index into [`FfmpegDrmPrimeFrame::objects`].
    pub object_index: usize,
    /// Byte offset within the selected object.
    pub offset: u64,
    /// Row pitch in bytes.
    pub pitch: u64,
}

/// One DRM format layer and its ordered planes.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegDrmPrimeLayer {
    /// DRM FourCC format.
    pub format: u32,
    /// Ordered plane descriptors.
    pub planes: Vec<FfmpegDrmPrimePlane>,
}

/// Owned FFmpeg VA-API to DRM PRIME mapping.
#[cfg(target_os = "linux")]
pub struct FfmpegDrmPrimeFrame {
    frame: NonNull<ffmpeg::ffi::AVFrame>,
    objects: Vec<FfmpegDrmPrimeObject>,
    layers: Vec<FfmpegDrmPrimeLayer>,
}

#[cfg(target_os = "linux")]
// SAFETY: The mapped AVFrame is immutable after construction, its AVBufferRef
// ownership is released only by Drop, and accessors expose copied metadata or
// newly duplicated file descriptors.
unsafe impl Send for FfmpegDrmPrimeFrame {}
#[cfg(target_os = "linux")]
// SAFETY: See the Send implementation.
unsafe impl Sync for FfmpegDrmPrimeFrame {}

#[cfg(target_os = "linux")]
impl fmt::Debug for FfmpegDrmPrimeFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FfmpegDrmPrimeFrame")
            .field("objects", &self.objects)
            .field("layers", &self.layers)
            .finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
impl FfmpegDrmPrimeFrame {
    fn map(
        source: NonNull<ffmpeg::ffi::AVFrame>,
        pixel_format: ffmpeg::util::format::pixel::Pixel,
    ) -> std::result::Result<Self, FfmpegNativeDecodedFrameResourceError> {
        if pixel_format != ffmpeg::util::format::pixel::Pixel::VAAPI {
            return Err(FfmpegNativeDecodedFrameResourceError::NotVaapiFrame { pixel_format });
        }
        // SAFETY: FFmpeg returns a fresh AVFrame allocation or null. Ownership
        // transfers immediately into `mapped`.
        let mapped = NonNull::new(unsafe { ffmpeg::ffi::av_frame_alloc() })
            .ok_or(FfmpegNativeDecodedFrameResourceError::DrmPrimeFrameAllocationFailed)?;
        // SAFETY: `mapped` is uniquely owned. This is the documented requested
        // destination format for a VA-API DRM PRIME mapping.
        unsafe {
            (*mapped.as_ptr()).format = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
        }
        // SAFETY: Both frames are live, the destination is uniquely owned, and
        // FFmpeg retains all required buffer references on success.
        let result = unsafe {
            ffmpeg::ffi::av_hwframe_map(
                mapped.as_ptr(),
                source.as_ptr(),
                ffmpeg::ffi::AV_HWFRAME_MAP_READ as i32,
            )
        };
        if result < 0 {
            let mut raw = mapped.as_ptr();
            // SAFETY: `mapped` is the sole owner after the failed map.
            unsafe { ffmpeg::ffi::av_frame_free(&mut raw) };
            return Err(FfmpegNativeDecodedFrameResourceError::DrmPrimeMapFailed { code: result });
        }
        match Self::from_mapped_frame(mapped) {
            Ok(frame) => Ok(frame),
            Err(error) => {
                let mut raw = mapped.as_ptr();
                // SAFETY: validation failed before ownership escaped.
                unsafe { ffmpeg::ffi::av_frame_free(&mut raw) };
                Err(error)
            }
        }
    }

    fn from_mapped_frame(
        frame: NonNull<ffmpeg::ffi::AVFrame>,
    ) -> std::result::Result<Self, FfmpegNativeDecodedFrameResourceError> {
        // SAFETY: The caller owns a successfully mapped DRM PRIME AVFrame.
        let raw_frame = unsafe { frame.as_ref() };
        let descriptor =
            NonNull::new(raw_frame.data[0].cast::<ffmpeg::ffi::AVDRMFrameDescriptor>())
                .ok_or_else(|| invalid_drm_descriptor("data[0] is null"))?;
        // SAFETY: AV_PIX_FMT_DRM_PRIME data[0] has this documented ABI and is
        // retained by `frame`.
        let descriptor = unsafe { descriptor.as_ref() };
        let object_count = descriptor_count(descriptor.nb_objects, "object")?;
        let layer_count = descriptor_count(descriptor.nb_layers, "layer")?;
        let mut objects = Vec::with_capacity(object_count);
        for object in descriptor.objects.iter().take(object_count) {
            if object.fd < 0 {
                return Err(invalid_drm_descriptor("object file descriptor is negative"));
            }
            if object.size == 0 {
                return Err(invalid_drm_descriptor("object size is zero"));
            }
            objects.push(FfmpegDrmPrimeObject {
                fd: object.fd,
                size: object.size,
                format_modifier: object.format_modifier,
            });
        }
        let mut layers = Vec::with_capacity(layer_count);
        for layer in descriptor.layers.iter().take(layer_count) {
            let plane_count = descriptor_count(layer.nb_planes, "plane")?;
            let mut planes = Vec::with_capacity(plane_count);
            for plane in layer.planes.iter().take(plane_count) {
                let object_index = usize::try_from(plane.object_index)
                    .map_err(|_| invalid_drm_descriptor("plane object index is negative"))?;
                if object_index >= object_count {
                    return Err(invalid_drm_descriptor("plane object index is out of range"));
                }
                let offset = u64::try_from(plane.offset)
                    .map_err(|_| invalid_drm_descriptor("plane offset is negative"))?;
                let pitch = u64::try_from(plane.pitch)
                    .map_err(|_| invalid_drm_descriptor("plane pitch is negative"))?;
                if pitch == 0 {
                    return Err(invalid_drm_descriptor("plane pitch is zero"));
                }
                planes.push(FfmpegDrmPrimePlane { object_index, offset, pitch });
            }
            layers.push(FfmpegDrmPrimeLayer { format: layer.format, planes });
        }
        Ok(Self { frame, objects, layers })
    }

    /// Ordered DRM objects retained by this mapping.
    pub fn objects(&self) -> &[FfmpegDrmPrimeObject] {
        &self.objects
    }

    /// Ordered DRM layers retained by this mapping.
    pub fn layers(&self) -> &[FfmpegDrmPrimeLayer] {
        &self.layers
    }

    /// Duplicate one object FD for ownership transfer to Vulkan.
    pub fn duplicate_object_fd(
        &self,
        object_index: usize,
    ) -> std::result::Result<std::os::fd::OwnedFd, FfmpegNativeDecodedFrameResourceError> {
        use std::os::fd::FromRawFd;

        let object = self.objects.get(object_index).ok_or_else(|| {
            invalid_drm_descriptor(format!("object index {object_index} is out of range"))
        })?;
        // SAFETY: `object.fd` remains live through `self`; dup returns a new
        // independently owned descriptor on success.
        let duplicated = unsafe { libc::dup(object.fd) };
        if duplicated < 0 {
            return Err(
                FfmpegNativeDecodedFrameResourceError::DrmPrimeFileDescriptorDuplicationFailed {
                    object_index,
                    reason: std::io::Error::last_os_error().to_string(),
                },
            );
        }
        // SAFETY: dup returned a fresh owned descriptor.
        Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicated) })
    }
}

#[cfg(target_os = "linux")]
impl Drop for FfmpegDrmPrimeFrame {
    fn drop(&mut self) {
        let mut frame = self.frame.as_ptr();
        // SAFETY: this object solely owns the mapped AVFrame allocation.
        unsafe { ffmpeg::ffi::av_frame_free(&mut frame) };
    }
}

#[cfg(target_os = "linux")]
fn descriptor_count(
    raw: i32,
    kind: &str,
) -> std::result::Result<usize, FfmpegNativeDecodedFrameResourceError> {
    let count = usize::try_from(raw)
        .map_err(|_| invalid_drm_descriptor(format!("{kind} count is negative")))?;
    if !(1..=ffmpeg::ffi::AV_DRM_MAX_PLANES as usize).contains(&count) {
        return Err(invalid_drm_descriptor(format!(
            "{kind} count {count} exceeds the DRM descriptor extent"
        )));
    }
    Ok(count)
}

#[cfg(target_os = "linux")]
fn invalid_drm_descriptor(reason: impl Into<String>) -> FfmpegNativeDecodedFrameResourceError {
    FfmpegNativeDecodedFrameResourceError::InvalidDrmPrimeDescriptor { reason: reason.into() }
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

#[cfg(test)]
mod session_output_lease_tests {
    use super::*;

    #[test]
    fn logical_outputs_are_counted_once_while_clones_share_their_token() {
        let family = PreviewNativeOutputTracker::default();
        let session = PreviewNativeOutputTracker::default();
        let first = PreviewDecodeSessionOutputLease::acquire(&family, &session)
            .expect("first logical native output");
        let first_renderer_clone = first.clone();
        let second = PreviewDecodeSessionOutputLease::acquire(&family, &session)
            .expect("second logical native output");

        assert_eq!(family.outstanding(), 2);
        assert_eq!(session.outstanding(), 2);

        drop(first);
        assert_eq!(family.outstanding(), 2);
        assert_eq!(session.outstanding(), 2);

        drop(first_renderer_clone);
        assert_eq!(family.outstanding(), 1);
        assert_eq!(session.outstanding(), 1);

        drop(second);
        assert!(family.is_released());
        assert!(session.is_released());
    }

    #[test]
    fn family_evidence_survives_dropped_session_tracker_owner() {
        let family = PreviewNativeOutputTracker::default();
        let session = PreviewNativeOutputTracker::default();
        let output = PreviewDecodeSessionOutputLease::acquire(&family, &session)
            .expect("logical native output");

        drop(session);
        assert_eq!(family.outstanding(), 1);

        drop(output);
        assert!(family.is_released());
    }
}
