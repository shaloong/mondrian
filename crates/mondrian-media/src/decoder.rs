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
        HwAccelProbe {
            selected_backend: Self::None,
            hardware_decode_active: false,
            zero_copy_active: false,
            frame_residency: DecodedFrameResidency::CpuRgba,
            gpu_frame_handle_kind: None,
            renderer_import_ready: false,
            reason: hardware_decode_unavailable_reason().to_owned(),
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
        "D3D11VA/DXVA hardware decode texture residency is not connected; using CPU RGBA decode"
    }
    #[cfg(target_os = "macos")]
    {
        "VideoToolbox hardware decode texture residency is not connected; using CPU RGBA decode"
    }
    #[cfg(target_os = "linux")]
    {
        "VA-API hardware decode texture residency is not connected; using CPU RGBA decode"
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

        assert_eq!(probe.selected_backend, HwAccelBackend::None);
        assert!(!probe.hardware_decode_active);
        assert!(!probe.zero_copy_active);
        assert_eq!(probe.frame_residency, DecodedFrameResidency::CpuRgba);
        assert_eq!(probe.gpu_frame_handle_kind, None);
        assert!(!probe.renderer_import_ready);
        assert!(probe.reason.contains("CPU RGBA decode"));
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
