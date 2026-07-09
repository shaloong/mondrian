//! Decode residency and hardware-frame import diagnostics.
//!
//! Preview frame scheduling and access-mode FFmpeg session ownership live in
//! `preview.rs` plus the app preview worker. This module intentionally does not
//! expose a second preview decode pool.

/// GPU hardware acceleration backend family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum HwAccelBackend {
    /// CPU software decode.
    #[default]
    None,
    /// NVIDIA NVDEC.
    Cuda,
    /// Windows DirectX 11 Video Acceleration.
    D3D11VA,
    /// macOS/iOS VideoToolbox.
    VideoToolbox,
    /// Linux VA-API.
    Vaapi,
}

/// Residency of frames produced by the media decode boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum DecodedFrameResidency {
    /// Decoder output is CPU RGBA memory.
    #[default]
    CpuRgba,
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
    /// 10/12-bit P010 two-plane YUV 4:2:0 surface.
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

/// Native hardware-frame handle family produced by a decoder.
///
/// This enum names the cross-crate contract only. It does not claim that
/// Mondrian can import the handle into the renderer; that requires a separate
/// renderer/platform import probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum DecodedGpuFrameHandleKind {
    /// Windows D3D11 `ID3D11Texture2D` hardware decode surface.
    D3D11Texture2D,
    /// macOS/iOS `CVPixelBuffer` backed by an IOSurface.
    CVPixelBuffer,
    /// Linux VA-API `VASurfaceID`/DMABUF-exportable surface.
    VaapiSurface,
    /// CUDA/NVDEC device allocation.
    CudaDeviceMemory,
}

impl DecodedGpuFrameHandleKind {
    /// Stable handle-kind name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::D3D11Texture2D => "D3D11Texture2D",
            Self::CVPixelBuffer => "CVPixelBuffer",
            Self::VaapiSurface => "VaapiSurface",
            Self::CudaDeviceMemory => "CudaDeviceMemory",
        }
    }
}

/// Hardware decode / zero-copy probe result for the current process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwAccelProbe {
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
    /// Whether the active decode path has a renderer texture-import contract.
    pub renderer_import_ready: bool,
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
        let candidate_backend = Self::platform_candidate();
        HwAccelProbe {
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
            renderer_import_ready: false,
            reason: hardware_decode_unavailable_reason().to_owned(),
        }
    }

    /// Preferred hardware backend for the current platform before runtime
    /// adapter/device validation.
    pub fn platform_candidate() -> Option<Self> {
        #[cfg(target_os = "windows")]
        {
            Some(Self::D3D11VA)
        }
        #[cfg(target_os = "macos")]
        {
            Some(Self::VideoToolbox)
        }
        #[cfg(target_os = "linux")]
        {
            Some(Self::Vaapi)
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            None
        }
    }

    /// Native handle family expected from this hardware backend.
    pub fn native_handle_kind(self) -> Option<DecodedGpuFrameHandleKind> {
        match self {
            Self::None => None,
            Self::Cuda => Some(DecodedGpuFrameHandleKind::CudaDeviceMemory),
            Self::D3D11VA => Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
            Self::VideoToolbox => Some(DecodedGpuFrameHandleKind::CVPixelBuffer),
            Self::Vaapi => Some(DecodedGpuFrameHandleKind::VaapiSurface),
        }
    }

    /// Preferred decoded surface formats for GPU-native playback.
    pub fn preferred_surface_formats(self) -> Vec<DecodedVideoSurfaceFormat> {
        match self {
            Self::None => Vec::new(),
            Self::Cuda | Self::D3D11VA | Self::VideoToolbox | Self::Vaapi => {
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
            Self::D3D11VA => "D3D11VA",
            Self::VideoToolbox => "VideoToolbox",
            Self::Vaapi => "Vaapi",
        }
    }
}

impl DecodedFrameResidency {
    /// Stable residency name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CpuRgba => "CpuRgba",
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
}

fn hardware_decode_unavailable_reason() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "D3D11VA/DXVA hardware decode adapter and texture residency are not connected; using CPU RGBA decode"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hw_accel_probe_fails_closed_until_texture_residency_exists() {
        let probe = HwAccelBackend::probe();

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
        assert!(!probe.renderer_import_ready);
        assert!(probe.reason.contains("CPU RGBA decode"));
    }

    #[test]
    fn hardware_backend_candidates_map_to_native_handles_and_surface_formats() {
        assert_eq!(
            HwAccelBackend::D3D11VA.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::D3D11Texture2D)
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
            HwAccelBackend::Cuda.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::CudaDeviceMemory)
        );
        assert_eq!(HwAccelBackend::None.native_handle_kind(), None);
        assert_eq!(
            HwAccelBackend::D3D11VA.preferred_surface_formats(),
            vec![
                DecodedVideoSurfaceFormat::P010,
                DecodedVideoSurfaceFormat::Nv12
            ]
        );
    }

    #[test]
    fn decoded_gpu_frame_handle_kind_has_stable_names() {
        assert_eq!(
            DecodedGpuFrameHandleKind::D3D11Texture2D.as_str(),
            "D3D11Texture2D"
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
            DecodedGpuFrameHandleKind::CudaDeviceMemory.as_str(),
            "CudaDeviceMemory"
        );
    }

    #[test]
    fn decoded_frame_residency_has_stable_names() {
        assert_eq!(DecodedFrameResidency::CpuRgba.as_str(), "CpuRgba");
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
    fn hw_accel_backend_has_stable_names() {
        assert_eq!(HwAccelBackend::None.as_str(), "None");
        assert_eq!(HwAccelBackend::Cuda.as_str(), "Cuda");
        assert_eq!(HwAccelBackend::D3D11VA.as_str(), "D3D11VA");
        assert_eq!(HwAccelBackend::VideoToolbox.as_str(), "VideoToolbox");
        assert_eq!(HwAccelBackend::Vaapi.as_str(), "Vaapi");
    }
}
