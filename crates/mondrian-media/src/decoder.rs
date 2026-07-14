//! Decode residency and hardware-frame import diagnostics.
//!
//! Preview frame scheduling and access-mode FFmpeg session ownership live in
//! `preview.rs` plus the app preview worker. This module intentionally does not
//! expose a second preview decode pool.

use std::collections::HashMap;
use std::ffi::CString;
use std::ptr;
use std::sync::{Mutex, OnceLock};

use ffmpeg_next as ffmpeg;

/// GPU hardware acceleration backend family.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
pub enum HwAccelBackend {
    /// CPU software decode.
    #[default]
    None,
    /// NVIDIA NVDEC.
    Cuda,
    /// Windows Direct3D 12 Video Acceleration.
    D3D12VA,
    /// Windows DirectX 11 Video Acceleration.
    D3D11VA,
    /// Legacy Windows DirectX Video Acceleration 2.
    Dxva2,
    /// macOS/iOS VideoToolbox.
    VideoToolbox,
    /// Linux VA-API.
    Vaapi,
    /// Legacy Linux VDPAU.
    Vdpau,
}

/// Backend-specific device selection supplied by the renderer admission path.
///
/// The selector is an explicit cross-layer contract, not a claim that the
/// selected decoder can be imported. Renderer admission still validates the
/// resulting native frame's physical adapter identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum HwAccelDeviceSelector {
    /// DXGI adapter index passed to FFmpeg's D3D11VA device creator.
    D3D11VaAdapterIndex(u32),
}

impl HwAccelDeviceSelector {
    fn device_name_for(self, backend: HwAccelBackend) -> Option<CString> {
        match (self, backend) {
            (Self::D3D11VaAdapterIndex(index), HwAccelBackend::D3D11VA) => {
                CString::new(index.to_string()).ok()
            }
            _ => None,
        }
    }
}

type HwAccelDeviceProbeKey = (HwAccelBackend, Option<HwAccelDeviceSelector>);
type HwAccelDeviceProbeCache = Mutex<HashMap<HwAccelDeviceProbeKey, HwAccelDeviceContextProbe>>;

/// Residency of frames produced by the media decode boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum DecodedFrameResidency {
    /// Decoder output is CPU RGBA memory.
    #[default]
    CpuRgba,
    /// Decoder output is CPU RGBA f32 memory.
    CpuFloat,
    /// Decoder output is a GPU texture or hardware frame.
    GpuTexture,
}

/// Decoder output surface format before Mondrian's preview CPU RGBA boundary.
///
/// This is a media-layer fact. Renderer-native import formats are modeled by
/// `mondrian-renderer` and must be mapped at the app/readiness boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum DecodedVideoSurfaceFormat {
    /// The decoder surface format is unknown or not yet reported.
    #[default]
    Unknown,
    /// 8-bit NV12 two-plane YUV 4:2:0 surface.
    Nv12,
    /// 10-bit P010 two-plane YUV 4:2:0 surface.
    P010,
    /// Planar 8-bit YUV 4:2:0.
    Yuv420p,
    /// Planar 10-bit YUV 4:2:0.
    Yuv420p10le,
    /// Packed RGBA8.
    Rgba8,
    /// Packed BGRA8.
    Bgra8,
    /// A known but currently non-native preview surface format.
    Other,
}

/// Encoded quantization range reported by the decoder for a video frame.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
pub enum DecodedVideoRange {
    /// No reliable range metadata was reported.
    #[default]
    Unknown,
    /// Studio/legal range, reported by FFmpeg as MPEG range.
    Limited,
    /// Full range, reported by FFmpeg as JPEG range.
    Full,
}

pub(crate) fn decoded_video_range_from_ffmpeg(
    range: ffmpeg_next::util::color::Range,
) -> DecodedVideoRange {
    match range {
        ffmpeg_next::util::color::Range::MPEG => DecodedVideoRange::Limited,
        ffmpeg_next::util::color::Range::JPEG => DecodedVideoRange::Full,
        ffmpeg_next::util::color::Range::Unspecified => DecodedVideoRange::Unknown,
    }
}

/// Chroma sample location reported by the decoder for a video frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum DecodedVideoChromaLocation {
    /// No reliable chroma-location metadata was reported.
    #[default]
    Unknown,
    /// Left chroma siting.
    Left,
    /// Center chroma siting.
    Center,
    /// Top-left chroma siting.
    TopLeft,
    /// Top chroma siting.
    Top,
    /// Bottom-left chroma siting.
    BottomLeft,
    /// Bottom chroma siting.
    Bottom,
}

/// Decoder-reported sampling facts for a decoded video frame.
///
/// These are media payload facts, not color-interpretation decisions. The app
/// combines them with the resolved source color space before asking the renderer
/// to import a native video surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct DecodedVideoSampling {
    /// Decoder-reported YCbCr-to-RGB matrix.
    pub matrix: DecodedVideoMatrix,
    /// Encoded quantization range.
    pub range: DecodedVideoRange,
    /// Chroma sample location.
    pub chroma_location: DecodedVideoChromaLocation,
    /// Effective coded bit depth. Zero means unknown.
    pub bit_depth: u8,
}

/// YUV matrix applied while converting a decoded CPU frame to source-encoded RGB.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
pub enum DecodedVideoMatrix {
    /// No reliable matrix was reported.
    #[default]
    Unknown,
    /// The decoder reported a matrix that requires a conversion not implemented
    /// by Mondrian. This is distinct from absent metadata so callers fail closed
    /// instead of substituting the project color-space matrix.
    Unsupported,
    /// BT.709 non-constant luminance coefficients.
    Bt709,
    /// BT.2020 non-constant luminance coefficients.
    Bt2020NonConstant,
    /// FCC coefficients.
    Fcc,
    /// BT.470BG / BT.601 coefficients.
    Bt470Bg,
    /// SMPTE 170M / BT.601 coefficients.
    Smpte170M,
    /// SMPTE 240M coefficients.
    Smpte240M,
    /// Source pixels were already RGB, so no YUV matrix was applied.
    Rgb,
}

/// Native hardware-frame handle family produced by a decoder.
///
/// This enum names the cross-crate contract only. It does not claim that
/// Mondrian can import the handle into the renderer; that requires a separate
/// renderer/platform import probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum DecodedGpuFrameHandleKind {
    /// Windows D3D12 `ID3D12Resource` hardware decode surface.
    D3D12Resource,
    /// Windows D3D11 `ID3D11Texture2D` hardware decode surface.
    D3D11Texture2D,
    /// Legacy Windows DXVA2 `IDirect3DSurface9` hardware decode surface.
    Dxva2Surface,
    /// macOS/iOS `CVPixelBuffer` backed by an IOSurface.
    CVPixelBuffer,
    /// Linux VA-API `VASurfaceID`/DMABUF-exportable surface.
    VaapiSurface,
    /// Legacy Linux VDPAU `VdpVideoSurface`.
    VdpauVideoSurface,
    /// CUDA/NVDEC device allocation.
    CudaDeviceMemory,
}

/// FFmpeg hardware pixel format reported by `avcodec_get_hw_config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HwAccelPixelFormat {
    /// FFmpeg D3D12 hardware surfaces (`AV_PIX_FMT_D3D12`).
    D3D12,
    /// FFmpeg D3D11 hardware surfaces (`AV_PIX_FMT_D3D11`).
    D3D11,
    /// Legacy FFmpeg D3D11VA VLD surfaces.
    D3D11VA,
    /// Legacy FFmpeg DXVA2 VLD surfaces.
    Dxva2,
    /// FFmpeg VideoToolbox hardware surfaces.
    VideoToolbox,
    /// FFmpeg VA-API hardware surfaces.
    Vaapi,
    /// FFmpeg VDPAU hardware surfaces.
    Vdpau,
    /// FFmpeg CUDA/NVDEC hardware surfaces.
    Cuda,
    /// A hardware config exists, but Mondrian does not classify this pixel format yet.
    Other(i32),
}

impl HwAccelPixelFormat {
    /// Stable hardware pixel-format name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::D3D12 => "D3D12",
            Self::D3D11 => "D3D11",
            Self::D3D11VA => "D3D11VA",
            Self::Dxva2 => "DXVA2",
            Self::VideoToolbox => "VideoToolbox",
            Self::Vaapi => "Vaapi",
            Self::Vdpau => "VDPAU",
            Self::Cuda => "Cuda",
            Self::Other(_) => "Other",
        }
    }

    fn from_ffmpeg(format: ffmpeg::ffi::AVPixelFormat) -> Self {
        match format {
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D12 => Self::D3D12,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D11 => Self::D3D11,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D11VA_VLD => Self::D3D11VA,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_DXVA2_VLD => Self::Dxva2,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX => Self::VideoToolbox,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI => Self::Vaapi,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VDPAU => Self::Vdpau,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_CUDA => Self::Cuda,
            other => Self::Other(other as i32),
        }
    }

    pub(crate) fn to_ffmpeg(self) -> Option<ffmpeg::ffi::AVPixelFormat> {
        match self {
            Self::D3D12 => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D12),
            Self::D3D11 => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D11),
            Self::D3D11VA => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D11VA_VLD),
            Self::Dxva2 => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_DXVA2_VLD),
            Self::VideoToolbox => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX),
            Self::Vaapi => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI),
            Self::Vdpau => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VDPAU),
            Self::Cuda => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_CUDA),
            Self::Other(_) => None,
        }
    }
}

/// Setup methods advertised by one FFmpeg hardware codec config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct HwAccelCodecConfigMethods {
    /// Config can be initialized from an `AVHWDeviceContext`.
    pub hw_device_ctx: bool,
    /// Config can be initialized from an `AVHWFramesContext`.
    pub hw_frames_ctx: bool,
    /// FFmpeg can initialize this internally.
    pub internal: bool,
    /// Config requires an ad-hoc legacy setup path.
    pub ad_hoc: bool,
}

impl HwAccelCodecConfigMethods {
    fn from_bits(bits: i32) -> Self {
        Self {
            hw_device_ctx: bits & ffmpeg::ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0,
            hw_frames_ctx: bits & ffmpeg::ffi::AV_CODEC_HW_CONFIG_METHOD_HW_FRAMES_CTX as i32 != 0,
            internal: bits & ffmpeg::ffi::AV_CODEC_HW_CONFIG_METHOD_INTERNAL as i32 != 0,
            ad_hoc: bits & ffmpeg::ffi::AV_CODEC_HW_CONFIG_METHOD_AD_HOC as i32 != 0,
        }
    }
}

/// Read-only FFmpeg codec/backend hardware decode capability probe.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HwAccelCodecConfigProbe {
    /// Backend requested for this probe.
    pub backend: HwAccelBackend,
    /// Whether this backend maps to a known FFmpeg hardware device type.
    pub backend_maps_to_ffmpeg_device: bool,
    /// Whether the linked FFmpeg build lists this hardware device type.
    pub ffmpeg_device_type_available: bool,
    /// Whether FFmpeg has a decoder for the requested codec id.
    pub ffmpeg_decoder_available: bool,
    /// Whether that decoder advertises a hardware config for this backend.
    pub ffmpeg_codec_config_available: bool,
    /// Hardware pixel format advertised by FFmpeg for this config.
    pub hw_pixel_format: Option<HwAccelPixelFormat>,
    /// Setup methods advertised by FFmpeg for this config.
    pub methods: HwAccelCodecConfigMethods,
    /// Stable diagnostic reason for unavailable or partial support.
    pub reason: String,
}

impl HwAccelCodecConfigProbe {
    fn unavailable(backend: HwAccelBackend, reason: impl Into<String>) -> Self {
        Self {
            backend,
            backend_maps_to_ffmpeg_device: backend.to_ffmpeg_device_type().is_some(),
            ffmpeg_device_type_available: false,
            ffmpeg_decoder_available: false,
            ffmpeg_codec_config_available: false,
            hw_pixel_format: None,
            methods: HwAccelCodecConfigMethods::default(),
            reason: reason.into(),
        }
    }
}

/// FFmpeg hardware device context creation probe for a backend.
///
/// This is a runtime capability probe for the local machine and linked FFmpeg
/// build. It creates and immediately releases an `AVHWDeviceContext`; it does
/// not attach that context to a decoder or claim that decoded frames are
/// GPU-resident.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HwAccelDeviceContextProbe {
    /// Backend requested for this probe.
    pub backend: HwAccelBackend,
    /// Whether this backend maps to a known FFmpeg hardware device type.
    pub backend_maps_to_ffmpeg_device: bool,
    /// Whether the linked FFmpeg build lists this hardware device type.
    pub ffmpeg_device_type_available: bool,
    /// Whether Mondrian attempted `av_hwdevice_ctx_create`.
    pub device_create_attempted: bool,
    /// Whether FFmpeg created an `AVHWDeviceContext` for this backend.
    pub device_context_created: bool,
    /// Negative FFmpeg error code returned by device creation, when any.
    pub device_create_error_code: Option<i32>,
    /// Stable diagnostic reason for unavailable or partial support.
    pub reason: String,
}

impl HwAccelDeviceContextProbe {
    pub(crate) fn unavailable(backend: HwAccelBackend, reason: impl Into<String>) -> Self {
        Self {
            backend,
            backend_maps_to_ffmpeg_device: backend.to_ffmpeg_device_type().is_some(),
            ffmpeg_device_type_available: false,
            device_create_attempted: false,
            device_context_created: false,
            device_create_error_code: None,
            reason: reason.into(),
        }
    }
}

/// Owned FFmpeg hardware device context for one decode session.
///
/// This wraps an `AVHWDeviceContext` reference. Cloning/sharing across sessions
/// should be introduced deliberately through a small cache; callers should not
/// pass raw FFmpeg pointers across crate boundaries.
pub(crate) struct HwAccelDeviceContext {
    backend: HwAccelBackend,
    ptr: *mut ffmpeg::ffi::AVBufferRef,
}

impl HwAccelDeviceContext {
    /// Backend used to create this device context.
    pub(crate) fn backend(&self) -> HwAccelBackend {
        self.backend
    }

    /// Attach a ref-counted hardware device context reference to an unopened
    /// FFmpeg codec context.
    pub(crate) fn attach_to_codec_context(
        &self,
        context: &mut ffmpeg::codec::context::Context,
    ) -> std::result::Result<(), String> {
        let device_ref = unsafe { ffmpeg::ffi::av_buffer_ref(self.ptr) };
        if device_ref.is_null() {
            return Err(format!(
                "FFmpeg could not retain {} hardware device context",
                self.backend.as_str()
            ));
        }
        unsafe {
            (*context.as_mut_ptr()).hw_device_ctx = device_ref;
        }
        Ok(())
    }
}

impl Drop for HwAccelDeviceContext {
    fn drop(&mut self) {
        unsafe {
            ffmpeg::ffi::av_buffer_unref(&mut self.ptr);
        }
    }
}

impl DecodedGpuFrameHandleKind {
    /// Stable handle-kind name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::D3D12Resource => "D3D12Resource",
            Self::D3D11Texture2D => "D3D11Texture2D",
            Self::Dxva2Surface => "Dxva2Surface",
            Self::CVPixelBuffer => "CVPixelBuffer",
            Self::VaapiSurface => "VaapiSurface",
            Self::VdpauVideoSurface => "VdpauVideoSurface",
            Self::CudaDeviceMemory => "CudaDeviceMemory",
        }
    }
}

/// Hardware decode / zero-copy probe result for the current process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwAccelProbe {
    /// Hardware backend families that should be tried on this platform in
    /// priority order before runtime codec/device validation.
    pub candidate_backends: Vec<HwAccelBackend>,
    /// Hardware backend family that would be preferred on this platform, if a
    /// real decoder adapter is connected.
    pub candidate_backend: Option<HwAccelBackend>,
    /// Native handle family the platform-preferred backend is expected to
    /// produce, if known.
    pub candidate_handle_kind: Option<DecodedGpuFrameHandleKind>,
    /// Native decoded surface formats the platform-preferred backend should
    /// prioritize for GPU-native playback.
    pub candidate_surface_formats: Vec<DecodedVideoSurfaceFormat>,
    /// Whether Mondrian has an implemented decoder adapter for the candidate
    /// backend in this build.
    pub decoder_adapter_available: bool,
    /// Backend that is actually active for the media decode boundary.
    pub selected_backend: HwAccelBackend,
    /// Whether the media decode boundary currently uses a hardware decoder.
    pub hardware_decode_active: bool,
    /// Whether decoded frames currently remain GPU-resident through the media boundary.
    pub zero_copy_active: bool,
    /// Residency produced by the active decode path.
    pub frame_residency: DecodedFrameResidency,
    /// Native handle family produced by the active decoder, if GPU-resident.
    pub gpu_frame_handle_kind: Option<DecodedGpuFrameHandleKind>,
    /// Stable diagnostic reason for the selected path.
    pub reason: String,
}

impl HwAccelBackend {
    /// Return the backend that is actually active for the media decode boundary.
    ///
    /// This intentionally fails closed to `None` until Mondrian has a real
    /// hardware-frame path that exports/imports decoder textures into the
    /// renderer. Platform preference alone must not be reported as active
    /// hardware decode.
    pub fn detect() -> Self {
        Self::probe().selected_backend
    }

    /// Probe the active hardware decode / zero-copy residency state.
    pub fn probe() -> HwAccelProbe {
        let candidate_backends = Self::platform_candidates();
        let candidate_backend = candidate_backends.first().copied();
        HwAccelProbe {
            candidate_backends,
            candidate_backend,
            candidate_handle_kind: candidate_backend.and_then(Self::native_handle_kind),
            candidate_surface_formats: candidate_backend
                .map(Self::preferred_surface_formats)
                .unwrap_or_default(),
            decoder_adapter_available: false,
            selected_backend: Self::None,
            hardware_decode_active: false,
            zero_copy_active: false,
            frame_residency: DecodedFrameResidency::CpuRgba,
            gpu_frame_handle_kind: None,
            reason: hardware_decode_unavailable_reason().to_owned(),
        }
    }

    /// Probe whether the linked FFmpeg decoder advertises a hardware config for
    /// this backend and codec. This is read-only; it does not create a hardware
    /// device or modify the preview decode session.
    pub fn probe_ffmpeg_codec_config(self, codec_id: ffmpeg::codec::Id) -> HwAccelCodecConfigProbe {
        let Some(device_type) = self.to_ffmpeg_device_type() else {
            return HwAccelCodecConfigProbe::unavailable(
                self,
                format!(
                    "{} does not map to an FFmpeg hardware device",
                    self.as_str()
                ),
            );
        };
        let _ = ffmpeg::init();
        let ffmpeg_device_type_available = ffmpeg_hwdevice_type_available(device_type);
        let codec = unsafe { ffmpeg::ffi::avcodec_find_decoder(codec_id.into()) };
        if codec.is_null() {
            return HwAccelCodecConfigProbe {
                backend: self,
                backend_maps_to_ffmpeg_device: true,
                ffmpeg_device_type_available,
                ffmpeg_decoder_available: false,
                ffmpeg_codec_config_available: false,
                hw_pixel_format: None,
                methods: HwAccelCodecConfigMethods::default(),
                reason: format!("FFmpeg decoder for {codec_id:?} is unavailable"),
            };
        }

        let mut index = 0;
        loop {
            let config = unsafe { ffmpeg::ffi::avcodec_get_hw_config(codec, index) };
            if config.is_null() {
                break;
            }
            let config = unsafe { &*config };
            if config.device_type == device_type {
                return HwAccelCodecConfigProbe {
                    backend: self,
                    backend_maps_to_ffmpeg_device: true,
                    ffmpeg_device_type_available,
                    ffmpeg_decoder_available: true,
                    ffmpeg_codec_config_available: true,
                    hw_pixel_format: Some(HwAccelPixelFormat::from_ffmpeg(config.pix_fmt)),
                    methods: HwAccelCodecConfigMethods::from_bits(config.methods),
                    reason: "FFmpeg decoder advertises a hardware config for this backend"
                        .to_owned(),
                };
            }
            index += 1;
        }

        HwAccelCodecConfigProbe {
            backend: self,
            backend_maps_to_ffmpeg_device: true,
            ffmpeg_device_type_available,
            ffmpeg_decoder_available: true,
            ffmpeg_codec_config_available: false,
            hw_pixel_format: None,
            methods: HwAccelCodecConfigMethods::default(),
            reason: format!(
                "FFmpeg decoder for {codec_id:?} does not advertise {} hardware config",
                self.as_str()
            ),
        }
    }

    /// Cached runtime probe for FFmpeg hardware device context creation.
    ///
    /// Device creation can touch GPU drivers. The result is process-stable for
    /// Mondrian's playback lifetime, so preview planning uses this cached
    /// variant instead of probing on every session open.
    pub fn cached_ffmpeg_device_context_probe(self) -> HwAccelDeviceContextProbe {
        self.cached_ffmpeg_device_context_probe_for(None)
    }

    pub(crate) fn cached_ffmpeg_device_context_probe_for(
        self,
        selector: Option<HwAccelDeviceSelector>,
    ) -> HwAccelDeviceContextProbe {
        static PROBES: OnceLock<HwAccelDeviceProbeCache> = OnceLock::new();
        let probes = PROBES.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(guard) = probes.lock() {
            if let Some(probe) = guard.get(&(self, selector)) {
                return probe.clone();
            }
        }
        let probe = self.probe_ffmpeg_device_context_for(selector);
        if let Ok(mut guard) = probes.lock() {
            guard.insert((self, selector), probe.clone());
        }
        probe
    }

    /// Probe whether FFmpeg can create a hardware device context for this
    /// backend. This creates and immediately releases an `AVHWDeviceContext`;
    /// it does not modify decoder negotiation or allocate hardware frames.
    pub fn probe_ffmpeg_device_context(self) -> HwAccelDeviceContextProbe {
        self.probe_ffmpeg_device_context_for(None)
    }

    fn probe_ffmpeg_device_context_for(
        self,
        selector: Option<HwAccelDeviceSelector>,
    ) -> HwAccelDeviceContextProbe {
        let Some(device_type) = self.to_ffmpeg_device_type() else {
            return HwAccelDeviceContextProbe::unavailable(
                self,
                format!(
                    "{} does not map to an FFmpeg hardware device",
                    self.as_str()
                ),
            );
        };
        let _ = ffmpeg::init();
        let ffmpeg_device_type_available = ffmpeg_hwdevice_type_available(device_type);
        if !ffmpeg_device_type_available {
            return HwAccelDeviceContextProbe {
                backend: self,
                backend_maps_to_ffmpeg_device: true,
                ffmpeg_device_type_available,
                device_create_attempted: false,
                device_context_created: false,
                device_create_error_code: None,
                reason: format!(
                    "linked FFmpeg build does not list {} hardware device type",
                    self.as_str()
                ),
            };
        }

        let mut device_context: *mut ffmpeg::ffi::AVBufferRef = ptr::null_mut();
        let device_name = selector.and_then(|selector| selector.device_name_for(self));
        let result = unsafe {
            ffmpeg::ffi::av_hwdevice_ctx_create(
                &mut device_context,
                device_type,
                device_name.as_ref().map_or(ptr::null(), |name| name.as_ptr()),
                ptr::null_mut(),
                0,
            )
        };
        if result < 0 {
            return HwAccelDeviceContextProbe {
                backend: self,
                backend_maps_to_ffmpeg_device: true,
                ffmpeg_device_type_available,
                device_create_attempted: true,
                device_context_created: false,
                device_create_error_code: Some(result),
                reason: format!(
                    "FFmpeg could not create {} hardware device context: {}",
                    self.as_str(),
                    ffmpeg::Error::from(result)
                ),
            };
        }

        let device_context_created = !device_context.is_null();
        unsafe {
            ffmpeg::ffi::av_buffer_unref(&mut device_context);
        }
        HwAccelDeviceContextProbe {
            backend: self,
            backend_maps_to_ffmpeg_device: true,
            ffmpeg_device_type_available,
            device_create_attempted: true,
            device_context_created,
            device_create_error_code: None,
            reason: if device_context_created {
                format!(
                    "FFmpeg created and released {} hardware device context",
                    self.as_str()
                )
            } else {
                format!(
                    "FFmpeg reported success but returned no {} hardware device context",
                    self.as_str()
                )
            },
        }
    }

    pub(crate) fn create_ffmpeg_device_context(
        self,
        selector: Option<HwAccelDeviceSelector>,
    ) -> std::result::Result<HwAccelDeviceContext, HwAccelDeviceContextProbe> {
        let Some(device_type) = self.to_ffmpeg_device_type() else {
            return Err(HwAccelDeviceContextProbe::unavailable(
                self,
                format!(
                    "{} does not map to an FFmpeg hardware device",
                    self.as_str()
                ),
            ));
        };
        let _ = ffmpeg::init();
        let ffmpeg_device_type_available = ffmpeg_hwdevice_type_available(device_type);
        if !ffmpeg_device_type_available {
            return Err(HwAccelDeviceContextProbe {
                backend: self,
                backend_maps_to_ffmpeg_device: true,
                ffmpeg_device_type_available,
                device_create_attempted: false,
                device_context_created: false,
                device_create_error_code: None,
                reason: format!(
                    "linked FFmpeg build does not list {} hardware device type",
                    self.as_str()
                ),
            });
        }

        let mut device_context: *mut ffmpeg::ffi::AVBufferRef = ptr::null_mut();
        let device_name = selector.and_then(|selector| selector.device_name_for(self));
        let result = unsafe {
            ffmpeg::ffi::av_hwdevice_ctx_create(
                &mut device_context,
                device_type,
                device_name.as_ref().map_or(ptr::null(), |name| name.as_ptr()),
                ptr::null_mut(),
                0,
            )
        };
        if result < 0 {
            return Err(HwAccelDeviceContextProbe {
                backend: self,
                backend_maps_to_ffmpeg_device: true,
                ffmpeg_device_type_available,
                device_create_attempted: true,
                device_context_created: false,
                device_create_error_code: Some(result),
                reason: format!(
                    "FFmpeg could not create {} hardware device context: {}",
                    self.as_str(),
                    ffmpeg::Error::from(result)
                ),
            });
        }
        if device_context.is_null() {
            return Err(HwAccelDeviceContextProbe {
                backend: self,
                backend_maps_to_ffmpeg_device: true,
                ffmpeg_device_type_available,
                device_create_attempted: true,
                device_context_created: false,
                device_create_error_code: None,
                reason: format!(
                    "FFmpeg reported success but returned no {} hardware device context",
                    self.as_str()
                ),
            });
        }

        Ok(HwAccelDeviceContext { backend: self, ptr: device_context })
    }

    /// Preferred hardware backend for the current platform before runtime
    /// adapter/device validation.
    pub fn platform_candidate() -> Option<Self> {
        Self::platform_candidates().first().copied()
    }

    /// Preferred hardware backends for the current platform in industrial
    /// decode admission order. Runtime codec/device probes may skip an earlier
    /// candidate and fall through to a later one.
    pub fn platform_candidates() -> Vec<Self> {
        #[cfg(target_os = "windows")]
        {
            vec![Self::D3D12VA, Self::D3D11VA, Self::Dxva2]
        }
        #[cfg(target_os = "macos")]
        {
            vec![Self::VideoToolbox]
        }
        #[cfg(target_os = "linux")]
        {
            vec![Self::Vaapi, Self::Vdpau]
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            Vec::new()
        }
    }

    /// Native handle family expected from this hardware backend.
    pub fn native_handle_kind(self) -> Option<DecodedGpuFrameHandleKind> {
        match self {
            Self::None => None,
            Self::Cuda => Some(DecodedGpuFrameHandleKind::CudaDeviceMemory),
            Self::D3D12VA => Some(DecodedGpuFrameHandleKind::D3D12Resource),
            Self::D3D11VA => Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
            Self::Dxva2 => Some(DecodedGpuFrameHandleKind::Dxva2Surface),
            Self::VideoToolbox => Some(DecodedGpuFrameHandleKind::CVPixelBuffer),
            Self::Vaapi => Some(DecodedGpuFrameHandleKind::VaapiSurface),
            Self::Vdpau => Some(DecodedGpuFrameHandleKind::VdpauVideoSurface),
        }
    }

    fn to_ffmpeg_device_type(self) -> Option<ffmpeg::ffi::AVHWDeviceType> {
        match self {
            Self::None => None,
            Self::Cuda => Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA),
            Self::D3D12VA => Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA),
            Self::D3D11VA => Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA),
            Self::Dxva2 => Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_DXVA2),
            Self::VideoToolbox => Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX),
            Self::Vaapi => Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI),
            Self::Vdpau => Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU),
        }
    }

    /// Preferred decoded surface formats for GPU-native playback.
    pub fn preferred_surface_formats(self) -> Vec<DecodedVideoSurfaceFormat> {
        match self {
            Self::None => Vec::new(),
            Self::Dxva2 | Self::Vdpau => Vec::new(),
            Self::Cuda | Self::D3D12VA | Self::D3D11VA | Self::VideoToolbox | Self::Vaapi => {
                vec![
                    DecodedVideoSurfaceFormat::P010,
                    DecodedVideoSurfaceFormat::Nv12,
                ]
            }
        }
    }

    /// Stable backend name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Cuda => "Cuda",
            Self::D3D12VA => "D3D12VA",
            Self::D3D11VA => "D3D11VA",
            Self::Dxva2 => "DXVA2",
            Self::VideoToolbox => "VideoToolbox",
            Self::Vaapi => "Vaapi",
            Self::Vdpau => "VDPAU",
        }
    }
}

impl DecodedFrameResidency {
    /// Stable residency name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CpuRgba => "CpuRgba",
            Self::CpuFloat => "CpuFloat",
            Self::GpuTexture => "GpuTexture",
        }
    }
}

impl DecodedVideoSurfaceFormat {
    /// Stable surface-format name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Nv12 => "Nv12",
            Self::P010 => "P010",
            Self::Yuv420p => "Yuv420p",
            Self::Yuv420p10le => "Yuv420p10le",
            Self::Rgba8 => "Rgba8",
            Self::Bgra8 => "Bgra8",
            Self::Other => "Other",
        }
    }

    /// Whether this decoded surface format can be carried as a native GPU payload.
    pub fn supports_native_gpu_payload(self) -> bool {
        matches!(self, Self::Nv12 | Self::P010 | Self::Rgba8 | Self::Bgra8)
    }

    /// Effective bit depth for formats with a fixed Mondrian contract.
    pub fn fixed_bit_depth(self) -> Option<u8> {
        match self {
            Self::Nv12 | Self::Yuv420p | Self::Rgba8 | Self::Bgra8 => Some(8),
            Self::P010 | Self::Yuv420p10le => Some(10),
            Self::Unknown | Self::Other => None,
        }
    }
}

fn hardware_decode_unavailable_reason() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "D3D12VA/D3D11VA hardware decode adapter and texture residency are not connected; using CPU RGBA decode"
    }
    #[cfg(target_os = "macos")]
    {
        "VideoToolbox hardware decode adapter and texture residency are not connected; using CPU RGBA decode"
    }
    #[cfg(target_os = "linux")]
    {
        "VA-API hardware decode adapter and texture residency are not connected; using CPU RGBA decode"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        "hardware decode texture residency is not connected for this platform; using CPU RGBA decode"
    }
}

fn ffmpeg_hwdevice_type_available(device_type: ffmpeg::ffi::AVHWDeviceType) -> bool {
    let mut previous = ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_NONE;
    loop {
        let next = unsafe { ffmpeg::ffi::av_hwdevice_iterate_types(previous) };
        if next == ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_NONE {
            return false;
        }
        if next == device_type {
            return true;
        }
        previous = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hw_accel_probe_fails_closed_until_texture_residency_exists() {
        let probe = HwAccelBackend::probe();

        assert_eq!(
            probe.candidate_backends,
            HwAccelBackend::platform_candidates()
        );
        assert_eq!(
            probe.candidate_backend,
            HwAccelBackend::platform_candidate()
        );
        assert_eq!(
            probe.candidate_handle_kind,
            probe.candidate_backend.and_then(HwAccelBackend::native_handle_kind)
        );
        assert_eq!(
            probe.candidate_surface_formats,
            probe
                .candidate_backend
                .map(HwAccelBackend::preferred_surface_formats)
                .unwrap_or_default()
        );
        assert!(!probe.decoder_adapter_available);
        assert_eq!(probe.selected_backend, HwAccelBackend::None);
        assert!(!probe.hardware_decode_active);
        assert!(!probe.zero_copy_active);
        assert_eq!(probe.frame_residency, DecodedFrameResidency::CpuRgba);
        assert_eq!(probe.gpu_frame_handle_kind, None);
        assert!(probe.reason.contains("CPU RGBA decode"));
    }

    #[test]
    fn hardware_backend_candidates_map_to_native_handles_and_surface_formats() {
        assert_eq!(
            HwAccelBackend::D3D12VA.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::D3D12Resource)
        );
        assert_eq!(
            HwAccelBackend::D3D11VA.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::D3D11Texture2D)
        );
        assert_eq!(
            HwAccelBackend::Dxva2.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::Dxva2Surface)
        );
        assert_eq!(
            HwAccelBackend::VideoToolbox.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::CVPixelBuffer)
        );
        assert_eq!(
            HwAccelBackend::Vaapi.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::VaapiSurface)
        );
        assert_eq!(
            HwAccelBackend::Vdpau.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::VdpauVideoSurface)
        );
        assert_eq!(
            HwAccelBackend::Cuda.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::CudaDeviceMemory)
        );
        assert_eq!(HwAccelBackend::None.native_handle_kind(), None);
        assert_eq!(
            HwAccelBackend::D3D12VA.preferred_surface_formats(),
            vec![
                DecodedVideoSurfaceFormat::P010,
                DecodedVideoSurfaceFormat::Nv12
            ]
        );
        assert_eq!(
            HwAccelBackend::D3D11VA.preferred_surface_formats(),
            vec![
                DecodedVideoSurfaceFormat::P010,
                DecodedVideoSurfaceFormat::Nv12
            ]
        );
        assert_eq!(
            HwAccelBackend::Dxva2.preferred_surface_formats(),
            Vec::new()
        );
        assert_eq!(
            HwAccelBackend::Vdpau.preferred_surface_formats(),
            Vec::new()
        );
    }

    #[test]
    fn hardware_backends_map_to_ffmpeg_device_types() {
        assert_eq!(HwAccelBackend::None.to_ffmpeg_device_type(), None);
        assert_eq!(
            HwAccelBackend::D3D12VA.to_ffmpeg_device_type(),
            Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA)
        );
        assert_eq!(
            HwAccelBackend::D3D11VA.to_ffmpeg_device_type(),
            Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA)
        );
        assert_eq!(
            HwAccelBackend::Dxva2.to_ffmpeg_device_type(),
            Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_DXVA2)
        );
        assert_eq!(
            HwAccelBackend::VideoToolbox.to_ffmpeg_device_type(),
            Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX)
        );
        assert_eq!(
            HwAccelBackend::Vaapi.to_ffmpeg_device_type(),
            Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI)
        );
        assert_eq!(
            HwAccelBackend::Vdpau.to_ffmpeg_device_type(),
            Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU)
        );
        assert_eq!(
            HwAccelBackend::Cuda.to_ffmpeg_device_type(),
            Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA)
        );
    }

    #[test]
    fn d3d11va_device_selector_maps_only_to_its_ffmpeg_device_name() {
        let selector = HwAccelDeviceSelector::D3D11VaAdapterIndex(7);

        assert_eq!(
            selector.device_name_for(HwAccelBackend::D3D11VA).as_deref(),
            Some(c"7")
        );
        assert_eq!(selector.device_name_for(HwAccelBackend::D3D12VA), None);
        assert_eq!(selector.device_name_for(HwAccelBackend::Cuda), None);
    }

    #[test]
    fn hw_accel_pixel_format_names_are_stable() {
        assert_eq!(HwAccelPixelFormat::D3D12.as_str(), "D3D12");
        assert_eq!(HwAccelPixelFormat::D3D11.as_str(), "D3D11");
        assert_eq!(HwAccelPixelFormat::D3D11VA.as_str(), "D3D11VA");
        assert_eq!(HwAccelPixelFormat::Dxva2.as_str(), "DXVA2");
        assert_eq!(HwAccelPixelFormat::VideoToolbox.as_str(), "VideoToolbox");
        assert_eq!(HwAccelPixelFormat::Vaapi.as_str(), "Vaapi");
        assert_eq!(HwAccelPixelFormat::Vdpau.as_str(), "VDPAU");
        assert_eq!(HwAccelPixelFormat::Cuda.as_str(), "Cuda");
        assert_eq!(HwAccelPixelFormat::Other(123).as_str(), "Other");
    }

    #[test]
    fn ffmpeg_hw_codec_config_probe_reports_structured_support_for_common_codecs() {
        let backend = HwAccelBackend::platform_candidate().unwrap_or(HwAccelBackend::D3D11VA);
        let h264 = backend.probe_ffmpeg_codec_config(ffmpeg::codec::Id::H264);
        let h265 = backend.probe_ffmpeg_codec_config(ffmpeg::codec::Id::HEVC);

        assert_eq!(h264.backend, backend);
        assert!(h264.backend_maps_to_ffmpeg_device);
        assert!(h264.ffmpeg_decoder_available);
        assert!(!h264.reason.is_empty());
        assert_eq!(h265.backend, backend);
        assert!(h265.ffmpeg_decoder_available);
        assert!(!h265.reason.is_empty());
        if h264.ffmpeg_codec_config_available {
            assert!(h264.hw_pixel_format.is_some());
            assert!(
                h264.methods.hw_device_ctx
                    || h264.methods.hw_frames_ctx
                    || h264.methods.internal
                    || h264.methods.ad_hoc
            );
        }
    }

    #[test]
    fn ffmpeg_hw_device_context_probe_reports_structured_runtime_status() {
        let backend = HwAccelBackend::platform_candidate().unwrap_or(HwAccelBackend::D3D11VA);
        let probe = backend.probe_ffmpeg_device_context();

        assert_eq!(probe.backend, backend);
        assert!(probe.backend_maps_to_ffmpeg_device);
        assert!(!probe.reason.is_empty());
        if probe.ffmpeg_device_type_available {
            assert!(probe.device_create_attempted);
        } else {
            assert!(!probe.device_create_attempted);
        }
        if probe.device_context_created {
            assert_eq!(probe.device_create_error_code, None);
        } else if probe.device_create_attempted {
            assert!(probe.device_create_error_code.is_some());
        }
    }

    #[test]
    fn decoded_gpu_frame_handle_kind_has_stable_names() {
        assert_eq!(
            DecodedGpuFrameHandleKind::D3D12Resource.as_str(),
            "D3D12Resource"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::D3D11Texture2D.as_str(),
            "D3D11Texture2D"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::Dxva2Surface.as_str(),
            "Dxva2Surface"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::CVPixelBuffer.as_str(),
            "CVPixelBuffer"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::VaapiSurface.as_str(),
            "VaapiSurface"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::VdpauVideoSurface.as_str(),
            "VdpauVideoSurface"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::CudaDeviceMemory.as_str(),
            "CudaDeviceMemory"
        );
    }

    #[test]
    fn decoded_frame_residency_has_stable_names() {
        assert_eq!(DecodedFrameResidency::CpuRgba.as_str(), "CpuRgba");
        assert_eq!(DecodedFrameResidency::CpuFloat.as_str(), "CpuFloat");
        assert_eq!(DecodedFrameResidency::GpuTexture.as_str(), "GpuTexture");
    }

    #[test]
    fn decoded_video_surface_format_has_stable_names() {
        assert_eq!(DecodedVideoSurfaceFormat::Unknown.as_str(), "Unknown");
        assert_eq!(DecodedVideoSurfaceFormat::Nv12.as_str(), "Nv12");
        assert_eq!(DecodedVideoSurfaceFormat::P010.as_str(), "P010");
        assert_eq!(DecodedVideoSurfaceFormat::Yuv420p.as_str(), "Yuv420p");
        assert_eq!(
            DecodedVideoSurfaceFormat::Yuv420p10le.as_str(),
            "Yuv420p10le"
        );
        assert_eq!(DecodedVideoSurfaceFormat::Rgba8.as_str(), "Rgba8");
        assert_eq!(DecodedVideoSurfaceFormat::Bgra8.as_str(), "Bgra8");
        assert_eq!(DecodedVideoSurfaceFormat::Other.as_str(), "Other");
    }

    #[test]
    fn decoded_video_surface_format_declares_native_gpu_payload_support() {
        assert!(DecodedVideoSurfaceFormat::Nv12.supports_native_gpu_payload());
        assert!(DecodedVideoSurfaceFormat::P010.supports_native_gpu_payload());
        assert!(DecodedVideoSurfaceFormat::Rgba8.supports_native_gpu_payload());
        assert!(DecodedVideoSurfaceFormat::Bgra8.supports_native_gpu_payload());
        assert!(!DecodedVideoSurfaceFormat::Unknown.supports_native_gpu_payload());
        assert!(!DecodedVideoSurfaceFormat::Yuv420p.supports_native_gpu_payload());
        assert!(!DecodedVideoSurfaceFormat::Yuv420p10le.supports_native_gpu_payload());
        assert!(!DecodedVideoSurfaceFormat::Other.supports_native_gpu_payload());
    }

    #[test]
    fn hw_accel_backend_has_stable_names() {
        assert_eq!(HwAccelBackend::None.as_str(), "None");
        assert_eq!(HwAccelBackend::Cuda.as_str(), "Cuda");
        assert_eq!(HwAccelBackend::D3D12VA.as_str(), "D3D12VA");
        assert_eq!(HwAccelBackend::D3D11VA.as_str(), "D3D11VA");
        assert_eq!(HwAccelBackend::Dxva2.as_str(), "DXVA2");
        assert_eq!(HwAccelBackend::VideoToolbox.as_str(), "VideoToolbox");
        assert_eq!(HwAccelBackend::Vaapi.as_str(), "Vaapi");
        assert_eq!(HwAccelBackend::Vdpau.as_str(), "VDPAU");
    }

    #[test]
    fn platform_hardware_backend_candidates_are_ordered_by_expected_native_path() {
        let candidates = HwAccelBackend::platform_candidates();
        #[cfg(target_os = "windows")]
        assert_eq!(
            candidates,
            vec![
                HwAccelBackend::D3D12VA,
                HwAccelBackend::D3D11VA,
                HwAccelBackend::Dxva2
            ]
        );
        #[cfg(target_os = "macos")]
        assert_eq!(candidates, vec![HwAccelBackend::VideoToolbox]);
        #[cfg(target_os = "linux")]
        assert_eq!(
            candidates,
            vec![HwAccelBackend::Vaapi, HwAccelBackend::Vdpau]
        );
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        assert!(candidates.is_empty());
    }
}
