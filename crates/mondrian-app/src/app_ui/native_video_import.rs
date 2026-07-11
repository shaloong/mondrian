//! App-layer diagnostics for native decoded video texture import readiness.
//!
//! This module combines facts from media decode, platform capability probing,
//! and renderer backend support. None of those lower layers should depend on
//! each other just to explain why preview playback is still using CPU RGBA
//! uploads.

use mondrian_core::{ColorMatrixCoefficients, ColorSpace};
use mondrian_media::PreviewNativeDecodedFrame;
use mondrian_media::{
    DecodedFrameResidency, DecodedGpuFrameHandleKind, DecodedVideoChromaLocation,
    DecodedVideoRange, DecodedVideoSampling, DecodedVideoSurfaceFormat,
};
use mondrian_platform::{NativeVideoTextureHandleKind, NativeVideoTextureImportProbeResult};
#[cfg(target_os = "windows")]
use mondrian_renderer::{
    execute_native_decoded_frame_import, D3D11Dx12NativeVideoImportBackend,
    GpuNativeDecodedFrameImportBackend,
};
use mondrian_renderer::{
    GpuColorFrameIdAllocator, GpuColorFrameResource, GpuColorFrameWgpuResource,
    GpuNativeDecodedFrameImportContract, GpuNativeDecodedFrameImportSupport,
    GpuNativeDecodedFrameTextureFormat, GpuNativeDecodedFrameVideoSampling, GpuVideoChromaLocation,
    GpuVideoRange, RenderInputTransform,
};

/// Stable readiness category for native decoded-frame import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum AppUiNativeVideoImportReadinessStatus {
    /// Current media preview path delivered CPU RGBA bytes, not a GPU decoder surface.
    CpuDecodedMedia,
    /// Decoder output claims GPU residency but no native handle family was reported.
    DecoderGpuHandleMissing,
    /// The platform has no native texture import adapter.
    PlatformImportUnsupported,
    /// The platform adapter exists but reports no usable import path yet.
    PlatformImportMissing,
    /// The platform cannot import the decoder handle family.
    PlatformHandleUnsupported,
    /// The renderer backend has no native decoded-frame import implementation.
    RendererBackendUnavailable,
    /// The renderer backend cannot consume the decoder handle family.
    RendererHandleUnsupported,
    /// The decoded source texture format was not reported.
    SourceTextureFormatUnknown,
    /// The renderer cannot safely sample the native surface without explicit
    /// range/matrix/transfer/bit-depth/chroma metadata.
    SourceVideoSamplingUnknown,
    /// The renderer backend cannot consume the decoded source texture format.
    RendererSourceTextureFormatUnsupported,
    /// The full path can stay zero-copy.
    ReadyZeroCopy,
    /// The path cannot stay zero-copy but can use a declared low-copy fallback.
    ReadyLowCopy,
}

/// App-owned native video backend lifetime and renderer support contract.
///
/// This runtime is independent of swapchain/UI renderer rebuilds. Imported
/// working resources use frame ids allocated by the destination color runtime
/// before entering its shared frame table.
pub(crate) struct AppUiNativeVideoImportRuntime {
    support: GpuNativeDecodedFrameImportSupport,
    #[cfg(target_os = "windows")]
    backend: Option<D3D11Dx12NativeVideoImportBackend>,
}

impl AppUiNativeVideoImportRuntime {
    pub fn new(adapter: &wgpu::Adapter, device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        #[cfg(target_os = "windows")]
        {
            match D3D11Dx12NativeVideoImportBackend::new(adapter, device, queue) {
                Ok(backend) => Self {
                    support: backend.support().clone(),
                    backend: Some(backend),
                },
                Err(error) => Self {
                    support: GpuNativeDecodedFrameImportSupport::unavailable_with_reason(
                        format!("{:?}", adapter.get_info().backend),
                        format!("native D3D11/DX12 YUV + OCIO backend unavailable: {error}"),
                    ),
                    backend: None,
                },
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (device, queue);
            Self {
                support: super::rendering::native_decoded_frame_import_support_from_adapter(
                    &adapter.get_info(),
                    device.features(),
                ),
            }
        }
    }

    pub fn support(&self) -> GpuNativeDecodedFrameImportSupport {
        self.support.clone()
    }

    pub fn import(
        &mut self,
        ids: &mut GpuColorFrameIdAllocator,
        source_color_space: ColorSpace,
        input_transform: &RenderInputTransform,
        native_frame: &PreviewNativeDecodedFrame,
    ) -> Result<GpuColorFrameResource<GpuColorFrameWgpuResource>, String> {
        let source_texture_format = native_source_texture_format_from_decoded(
            native_frame.surface_format,
        )
        .ok_or_else(|| {
            format!(
                "decoded native surface format {:?} has no renderer import contract",
                native_frame.surface_format
            )
        })?;
        let video_sampling = native_video_sampling_from_decoded(
            source_color_space,
            source_texture_format,
            native_frame.diagnostics.decoded_video_sampling,
        )
        .ok_or_else(|| {
            "decoded native surface has incomplete video sampling metadata".to_owned()
        })?;
        let contract = GpuNativeDecodedFrameImportContract {
            width: native_frame.width,
            height: native_frame.height,
            source_color_space,
            input_transform: input_transform.clone(),
            handle_kind: native_frame.handle_kind(),
            source_texture_format,
            video_sampling,
            label: format!("viewer-native-working-{}", native_frame.handle.id().get()),
        };
        #[cfg(target_os = "windows")]
        {
            let backend = self.backend.as_mut().ok_or_else(|| {
                self.support
                    .unavailable_reason
                    .clone()
                    .unwrap_or_else(|| "native video backend is unavailable".to_owned())
            })?;
            execute_native_decoded_frame_import(backend, ids, contract, native_frame)
                .map(|execution| execution.resource)
                .map_err(|error| error.to_string())
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (ids, contract, native_frame);
            Err(self
                .support
                .unavailable_reason
                .clone()
                .unwrap_or_else(|| "native video backend is unavailable".to_owned()))
        }
    }
}

/// Input facts for native decoded-frame import readiness evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppUiNativeVideoImportReadinessInput {
    /// Residency reported by the media decoder boundary.
    pub decoder_residency: DecodedFrameResidency,
    /// Native decoder handle family, when the decoder produced a GPU surface.
    pub decoder_handle_kind: Option<DecodedGpuFrameHandleKind>,
    /// Source texture format of the decoded GPU surface, when known.
    pub source_texture_format: Option<GpuNativeDecodedFrameTextureFormat>,
    /// Shader-visible sampling contract needed before renderer import can
    /// convert native video surfaces into encoded RGB and then working space.
    pub source_video_sampling: Option<GpuNativeDecodedFrameVideoSampling>,
    /// OS/platform native texture import probe.
    pub platform_probe: NativeVideoTextureImportProbeResult,
    /// Renderer backend native decoded-frame import support contract.
    pub renderer_support: GpuNativeDecodedFrameImportSupport,
}

/// Point-in-time native decoded-frame import readiness report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct AppUiNativeVideoImportReadiness {
    /// Stable readiness status.
    pub status: AppUiNativeVideoImportReadinessStatus,
    /// Whether a zero-copy decoder-surface-to-renderer path is ready.
    pub zero_copy_ready: bool,
    /// Whether a declared low-copy fallback path is ready.
    pub low_copy_ready: bool,
    /// Whether the decoder output is GPU-resident.
    pub decoder_gpu_resident: bool,
    /// Decoder handle family, serialized as a stable diagnostic string.
    pub decoder_handle_kind: Option<String>,
    /// Platform handle family required for this decoder output.
    pub platform_handle_kind: Option<String>,
    /// Whether platform capability discovery is available.
    pub platform_discovery_available: bool,
    /// Whether the platform reports zero-copy import support.
    pub platform_zero_copy_supported: bool,
    /// Whether the platform reports a low-copy fallback.
    pub platform_low_copy_fallback_supported: bool,
    /// Platform probe diagnostic when discovery is partial or unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_error: Option<String>,
    /// Whether the renderer backend reports native import readiness.
    pub renderer_backend_ready: bool,
    /// Renderer backend label observed by the app/runtime, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer_backend_label: Option<String>,
    /// Renderer-side reason native decoded-frame import is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer_unavailable_reason: Option<String>,
    /// Whether the renderer backend accepts this decoder handle family.
    pub renderer_supports_handle_kind: bool,
    /// Whether the renderer backend accepts this decoded source texture format.
    pub renderer_supports_source_texture_format: bool,
    /// Stable human-readable reason for the current status.
    pub reason: String,
}

/// Evaluate native decoded-frame import readiness from media/platform/renderer facts.
pub(crate) fn evaluate_native_video_import_readiness(
    input: AppUiNativeVideoImportReadinessInput,
) -> AppUiNativeVideoImportReadiness {
    let decoder_gpu_resident = input.decoder_residency == DecodedFrameResidency::GpuTexture;
    let decoder_handle_kind = input.decoder_handle_kind.map(DecodedGpuFrameHandleKind::as_str);
    let platform_handle_kind = input
        .decoder_handle_kind
        .map(platform_handle_kind_for_decoder)
        .map(NativeVideoTextureHandleKind::as_str);
    let renderer_supports_handle_kind = input
        .decoder_handle_kind
        .map(|kind| input.renderer_support.supports_handle_kind(kind))
        .unwrap_or(false);
    let renderer_supports_source_texture_format = input
        .source_texture_format
        .map(|format| input.renderer_support.supports_source_texture_format(format))
        .unwrap_or(false);

    let base = AppUiNativeVideoImportReadiness {
        status: AppUiNativeVideoImportReadinessStatus::CpuDecodedMedia,
        zero_copy_ready: false,
        low_copy_ready: false,
        decoder_gpu_resident,
        decoder_handle_kind: decoder_handle_kind.map(str::to_owned),
        platform_handle_kind: platform_handle_kind.map(str::to_owned),
        platform_discovery_available: input.platform_probe.discovery_available,
        platform_zero_copy_supported: input.platform_probe.zero_copy_supported,
        platform_low_copy_fallback_supported: input.platform_probe.low_copy_fallback_supported,
        platform_error: input.platform_probe.error.clone(),
        renderer_backend_ready: input.renderer_support.renderer_backend_ready,
        renderer_backend_label: input.renderer_support.renderer_backend_label.clone(),
        renderer_unavailable_reason: input.renderer_support.unavailable_reason.clone(),
        renderer_supports_handle_kind,
        renderer_supports_source_texture_format,
        reason: String::new(),
    };

    if !decoder_gpu_resident {
        return base.with_status(
            AppUiNativeVideoImportReadinessStatus::CpuDecodedMedia,
            "media preview delivered CPU RGBA bytes; native decoder texture import is not active",
        );
    }

    let Some(decoder_handle_kind) = input.decoder_handle_kind else {
        return base.with_status(
            AppUiNativeVideoImportReadinessStatus::DecoderGpuHandleMissing,
            "decoder reported GPU residency without a native handle family",
        );
    };
    let platform_kind = platform_handle_kind_for_decoder(decoder_handle_kind);

    if !input.platform_probe.discovery_available {
        return base.with_status(
            AppUiNativeVideoImportReadinessStatus::PlatformImportUnsupported,
            input
                .platform_probe
                .error
                .as_deref()
                .unwrap_or("platform native video texture import probe is unavailable"),
        );
    }
    if !input.platform_probe.supports(platform_kind) {
        let status = if input.platform_probe.supported_handle_kinds.is_empty() {
            AppUiNativeVideoImportReadinessStatus::PlatformImportMissing
        } else {
            AppUiNativeVideoImportReadinessStatus::PlatformHandleUnsupported
        };
        return base.with_status(
            status,
            input.platform_probe.error.as_deref().unwrap_or(
                "platform native video texture import does not support the decoder handle family",
            ),
        );
    }

    if !input.renderer_support.renderer_backend_ready {
        return base.with_status(
            AppUiNativeVideoImportReadinessStatus::RendererBackendUnavailable,
            input
                .renderer_support
                .unavailable_reason
                .as_deref()
                .unwrap_or("renderer backend does not support native decoded-frame import"),
        );
    }
    if !renderer_supports_handle_kind {
        return base.with_status(
            AppUiNativeVideoImportReadinessStatus::RendererHandleUnsupported,
            "renderer backend does not support the decoder handle family",
        );
    }

    let Some(source_texture_format) = input.source_texture_format else {
        return base.with_status(
            AppUiNativeVideoImportReadinessStatus::SourceTextureFormatUnknown,
            "decoded GPU source texture format is unknown",
        );
    };
    if !input.renderer_support.supports_source_texture_format(source_texture_format) {
        return base.with_status(
            AppUiNativeVideoImportReadinessStatus::RendererSourceTextureFormatUnsupported,
            "renderer backend does not support the decoded source texture format",
        );
    }
    if input.source_video_sampling.is_none() {
        return base.with_status(
            AppUiNativeVideoImportReadinessStatus::SourceVideoSamplingUnknown,
            "native decoded-frame import requires explicit video sampling metadata",
        );
    }
    if input.platform_probe.zero_copy_supported {
        return base.with_ready(
            AppUiNativeVideoImportReadinessStatus::ReadyZeroCopy,
            true,
            false,
        );
    }
    if input.platform_probe.low_copy_fallback_supported {
        return base.with_ready(
            AppUiNativeVideoImportReadinessStatus::ReadyLowCopy,
            false,
            true,
        );
    }

    base.with_status(
        AppUiNativeVideoImportReadinessStatus::PlatformImportMissing,
        "platform import support was reported without zero-copy or low-copy residency",
    )
}

impl AppUiNativeVideoImportReadiness {
    fn with_status(
        mut self,
        status: AppUiNativeVideoImportReadinessStatus,
        reason: impl Into<String>,
    ) -> Self {
        self.status = status;
        self.reason = reason.into();
        self
    }

    fn with_ready(
        mut self,
        status: AppUiNativeVideoImportReadinessStatus,
        zero_copy_ready: bool,
        low_copy_ready: bool,
    ) -> Self {
        self.status = status;
        self.zero_copy_ready = zero_copy_ready;
        self.low_copy_ready = low_copy_ready;
        self.reason = match status {
            AppUiNativeVideoImportReadinessStatus::ReadyZeroCopy => {
                "native decoded-frame import is ready for zero-copy playback".to_owned()
            }
            AppUiNativeVideoImportReadinessStatus::ReadyLowCopy => {
                "native decoded-frame import is ready through a declared low-copy fallback"
                    .to_owned()
            }
            _ => self.reason,
        };
        self
    }
}

pub(crate) fn platform_handle_kind_for_decoder(
    handle_kind: DecodedGpuFrameHandleKind,
) -> NativeVideoTextureHandleKind {
    match handle_kind {
        DecodedGpuFrameHandleKind::D3D12Resource => NativeVideoTextureHandleKind::D3D12Resource,
        DecodedGpuFrameHandleKind::D3D11Texture2D => NativeVideoTextureHandleKind::D3D11Texture2D,
        DecodedGpuFrameHandleKind::Dxva2Surface => NativeVideoTextureHandleKind::Dxva2Surface,
        DecodedGpuFrameHandleKind::CVPixelBuffer => NativeVideoTextureHandleKind::CVPixelBuffer,
        DecodedGpuFrameHandleKind::VaapiSurface => NativeVideoTextureHandleKind::DmaBuf,
        DecodedGpuFrameHandleKind::VdpauVideoSurface => {
            NativeVideoTextureHandleKind::VdpauVideoSurface
        }
        DecodedGpuFrameHandleKind::CudaDeviceMemory => {
            NativeVideoTextureHandleKind::CudaDeviceMemory
        }
    }
}

/// Map a media-layer decoded surface fact to the renderer native import format contract.
pub(crate) fn native_source_texture_format_from_decoded(
    format: DecodedVideoSurfaceFormat,
) -> Option<GpuNativeDecodedFrameTextureFormat> {
    GpuNativeDecodedFrameTextureFormat::try_from(format).ok()
}

/// Combine media decoder sampling facts with the resolved source color space.
///
/// This is app-layer admission logic: media does not depend on renderer types,
/// and renderer backends do not guess platform defaults. Unknown or unsupported
/// payload facts fail closed so viewer diagnostics can explain why native video
/// import stayed on the CPU/low-copy path.
pub(crate) fn native_video_sampling_from_decoded(
    source_color_space: ColorSpace,
    source_texture_format: GpuNativeDecodedFrameTextureFormat,
    decoded: DecodedVideoSampling,
) -> Option<GpuNativeDecodedFrameVideoSampling> {
    let range = match decoded.range {
        DecodedVideoRange::Limited => GpuVideoRange::Limited,
        DecodedVideoRange::Full => GpuVideoRange::Full,
        DecodedVideoRange::Unknown => return None,
    };
    let expected_bit_depth = expected_native_source_bit_depth(source_texture_format);
    if decoded.bit_depth != expected_bit_depth {
        return None;
    }

    let chroma_location = match source_texture_format {
        GpuNativeDecodedFrameTextureFormat::Nv12 | GpuNativeDecodedFrameTextureFormat::P010 => {
            decoded_chroma_location_to_gpu(decoded.chroma_location)?
        }
        GpuNativeDecodedFrameTextureFormat::Rgba8Unorm
        | GpuNativeDecodedFrameTextureFormat::Bgra8Unorm => {
            if source_color_space.encoding().matrix != ColorMatrixCoefficients::Rgb {
                return None;
            }
            GpuVideoChromaLocation::Unspecified
        }
    };

    Some(GpuNativeDecodedFrameVideoSampling::from_source_color_space(
        source_color_space,
        range,
        decoded.bit_depth,
        chroma_location,
    ))
}

fn expected_native_source_bit_depth(format: GpuNativeDecodedFrameTextureFormat) -> u8 {
    match format {
        GpuNativeDecodedFrameTextureFormat::Nv12
        | GpuNativeDecodedFrameTextureFormat::Rgba8Unorm
        | GpuNativeDecodedFrameTextureFormat::Bgra8Unorm => 8,
        GpuNativeDecodedFrameTextureFormat::P010 => 10,
    }
}

fn decoded_chroma_location_to_gpu(
    location: DecodedVideoChromaLocation,
) -> Option<GpuVideoChromaLocation> {
    match location {
        DecodedVideoChromaLocation::Left => Some(GpuVideoChromaLocation::Left),
        DecodedVideoChromaLocation::Center => Some(GpuVideoChromaLocation::Center),
        DecodedVideoChromaLocation::TopLeft => Some(GpuVideoChromaLocation::TopLeft),
        DecodedVideoChromaLocation::Unknown
        | DecodedVideoChromaLocation::Top
        | DecodedVideoChromaLocation::BottomLeft
        | DecodedVideoChromaLocation::Bottom => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_video_import_readiness_reports_cpu_decoded_media() {
        let report = evaluate_native_video_import_readiness(cpu_decoded_input());

        assert_eq!(
            report.status,
            AppUiNativeVideoImportReadinessStatus::CpuDecodedMedia
        );
        assert!(!report.decoder_gpu_resident);
        assert!(!report.zero_copy_ready);
        assert!(!report.low_copy_ready);
        assert!(report.reason.contains("CPU RGBA"));
    }

    #[test]
    fn native_video_import_readiness_reports_platform_missing() {
        let report = evaluate_native_video_import_readiness(AppUiNativeVideoImportReadinessInput {
            decoder_residency: DecodedFrameResidency::GpuTexture,
            decoder_handle_kind: Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
            source_texture_format: Some(GpuNativeDecodedFrameTextureFormat::Nv12),
            source_video_sampling: Some(native_video_sampling()),
            platform_probe: NativeVideoTextureImportProbeResult::missing("dxgi import missing"),
            renderer_support: renderer_support(),
        });

        assert_eq!(
            report.status,
            AppUiNativeVideoImportReadinessStatus::PlatformImportMissing
        );
        assert_eq!(
            report.platform_handle_kind.as_deref(),
            Some("D3D11Texture2D")
        );
        assert!(report.reason.contains("dxgi import missing"));
    }

    #[test]
    fn native_video_import_readiness_reports_renderer_unavailable() {
        let report = evaluate_native_video_import_readiness(AppUiNativeVideoImportReadinessInput {
            renderer_support: GpuNativeDecodedFrameImportSupport::unavailable_with_reason(
                "Dx12",
                "D3D11 shared texture import bridge is not connected",
            ),
            ..ready_input()
        });

        assert_eq!(
            report.status,
            AppUiNativeVideoImportReadinessStatus::RendererBackendUnavailable
        );
        assert!(!report.renderer_backend_ready);
        assert_eq!(report.renderer_backend_label.as_deref(), Some("Dx12"));
        assert_eq!(
            report.renderer_unavailable_reason.as_deref(),
            Some("D3D11 shared texture import bridge is not connected")
        );
        assert!(report.reason.contains("D3D11 shared texture import bridge"));
    }

    #[test]
    fn native_video_import_readiness_preserves_platform_partial_diagnostics() {
        let report = evaluate_native_video_import_readiness(AppUiNativeVideoImportReadinessInput {
            platform_probe: NativeVideoTextureImportProbeResult::found_partial(
                vec![NativeVideoTextureHandleKind::D3D11Texture2D],
                false,
                true,
                "D3D11 device probe succeeded; zero-copy renderer import is gated",
            ),
            renderer_support: GpuNativeDecodedFrameImportSupport::unavailable(),
            ..ready_input()
        });

        assert_eq!(
            report.status,
            AppUiNativeVideoImportReadinessStatus::RendererBackendUnavailable
        );
        assert!(!report.platform_zero_copy_supported);
        assert!(report.platform_low_copy_fallback_supported);
        assert!(report.platform_error.as_deref().unwrap_or_default().contains("D3D11"));
    }

    #[test]
    fn native_video_import_readiness_requires_source_texture_format() {
        let report = evaluate_native_video_import_readiness(AppUiNativeVideoImportReadinessInput {
            source_texture_format: None,
            ..ready_input()
        });

        assert_eq!(
            report.status,
            AppUiNativeVideoImportReadinessStatus::SourceTextureFormatUnknown
        );
        assert!(!report.renderer_supports_source_texture_format);
    }

    #[test]
    fn native_video_import_readiness_requires_video_sampling_contract() {
        let report = evaluate_native_video_import_readiness(AppUiNativeVideoImportReadinessInput {
            source_video_sampling: None,
            ..ready_input()
        });

        assert_eq!(
            report.status,
            AppUiNativeVideoImportReadinessStatus::SourceVideoSamplingUnknown
        );
        assert!(!report.zero_copy_ready);
        assert!(report.reason.contains("video sampling"));
    }

    #[test]
    fn native_video_import_readiness_reports_zero_copy_ready() {
        let report = evaluate_native_video_import_readiness(ready_input());

        assert_eq!(
            report.status,
            AppUiNativeVideoImportReadinessStatus::ReadyZeroCopy
        );
        assert!(report.zero_copy_ready);
        assert!(!report.low_copy_ready);
        assert!(report.renderer_supports_handle_kind);
        assert!(report.renderer_supports_source_texture_format);
    }

    #[test]
    fn native_source_texture_format_maps_gpu_native_media_surfaces() {
        assert_eq!(
            native_source_texture_format_from_decoded(DecodedVideoSurfaceFormat::Nv12),
            Some(GpuNativeDecodedFrameTextureFormat::Nv12)
        );
        assert_eq!(
            native_source_texture_format_from_decoded(DecodedVideoSurfaceFormat::P010),
            Some(GpuNativeDecodedFrameTextureFormat::P010)
        );
        assert_eq!(
            native_source_texture_format_from_decoded(DecodedVideoSurfaceFormat::Rgba8),
            Some(GpuNativeDecodedFrameTextureFormat::Rgba8Unorm)
        );
        assert_eq!(
            native_source_texture_format_from_decoded(DecodedVideoSurfaceFormat::Bgra8),
            Some(GpuNativeDecodedFrameTextureFormat::Bgra8Unorm)
        );
        assert_eq!(
            native_source_texture_format_from_decoded(DecodedVideoSurfaceFormat::Yuv420p),
            None
        );
        assert_eq!(
            native_source_texture_format_from_decoded(DecodedVideoSurfaceFormat::Unknown),
            None
        );
    }

    #[test]
    fn native_video_sampling_maps_nv12_decoder_facts() {
        let sampling = native_video_sampling_from_decoded(
            mondrian_core::ColorSpace::Rec709,
            GpuNativeDecodedFrameTextureFormat::Nv12,
            DecodedVideoSampling {
                range: DecodedVideoRange::Limited,
                chroma_location: DecodedVideoChromaLocation::Left,
                bit_depth: 8,
            },
        )
        .expect("complete NV12 facts should build renderer sampling");

        assert_eq!(sampling.range, GpuVideoRange::Limited);
        assert_eq!(sampling.bit_depth, 8);
        assert_eq!(sampling.chroma_location, GpuVideoChromaLocation::Left);
    }

    #[test]
    fn native_video_sampling_maps_p010_decoder_facts() {
        let sampling = native_video_sampling_from_decoded(
            mondrian_core::ColorSpace::Rec2100Pq,
            GpuNativeDecodedFrameTextureFormat::P010,
            DecodedVideoSampling {
                range: DecodedVideoRange::Limited,
                chroma_location: DecodedVideoChromaLocation::TopLeft,
                bit_depth: 10,
            },
        )
        .expect("complete P010 facts should build renderer sampling");

        assert_eq!(sampling.range, GpuVideoRange::Limited);
        assert_eq!(sampling.bit_depth, 10);
        assert_eq!(sampling.chroma_location, GpuVideoChromaLocation::TopLeft);
    }

    #[test]
    fn native_video_sampling_rejects_unknown_range() {
        assert_eq!(
            native_video_sampling_from_decoded(
                mondrian_core::ColorSpace::Rec709,
                GpuNativeDecodedFrameTextureFormat::Nv12,
                DecodedVideoSampling {
                    range: DecodedVideoRange::Unknown,
                    chroma_location: DecodedVideoChromaLocation::Left,
                    bit_depth: 8,
                },
            ),
            None
        );
    }

    #[test]
    fn native_video_sampling_rejects_unsupported_chroma_location() {
        assert_eq!(
            native_video_sampling_from_decoded(
                mondrian_core::ColorSpace::Rec709,
                GpuNativeDecodedFrameTextureFormat::Nv12,
                DecodedVideoSampling {
                    range: DecodedVideoRange::Limited,
                    chroma_location: DecodedVideoChromaLocation::Bottom,
                    bit_depth: 8,
                },
            ),
            None
        );
    }

    #[test]
    fn native_video_sampling_rejects_bit_depth_mismatch() {
        assert_eq!(
            native_video_sampling_from_decoded(
                mondrian_core::ColorSpace::Rec709,
                GpuNativeDecodedFrameTextureFormat::P010,
                DecodedVideoSampling {
                    range: DecodedVideoRange::Limited,
                    chroma_location: DecodedVideoChromaLocation::Left,
                    bit_depth: 8,
                },
            ),
            None
        );
    }

    #[test]
    fn native_video_sampling_rejects_rgb_surface_with_ycbcr_source_matrix() {
        assert_eq!(
            native_video_sampling_from_decoded(
                mondrian_core::ColorSpace::Rec709,
                GpuNativeDecodedFrameTextureFormat::Bgra8Unorm,
                DecodedVideoSampling {
                    range: DecodedVideoRange::Full,
                    chroma_location: DecodedVideoChromaLocation::Unknown,
                    bit_depth: 8,
                },
            ),
            None
        );
    }

    fn cpu_decoded_input() -> AppUiNativeVideoImportReadinessInput {
        AppUiNativeVideoImportReadinessInput {
            decoder_residency: DecodedFrameResidency::CpuRgba,
            decoder_handle_kind: None,
            source_texture_format: None,
            source_video_sampling: None,
            platform_probe: NativeVideoTextureImportProbeResult::unsupported("not probed"),
            renderer_support: GpuNativeDecodedFrameImportSupport::unavailable(),
        }
    }

    fn ready_input() -> AppUiNativeVideoImportReadinessInput {
        AppUiNativeVideoImportReadinessInput {
            decoder_residency: DecodedFrameResidency::GpuTexture,
            decoder_handle_kind: Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
            source_texture_format: Some(GpuNativeDecodedFrameTextureFormat::Nv12),
            source_video_sampling: Some(native_video_sampling()),
            platform_probe: NativeVideoTextureImportProbeResult::found(
                vec![NativeVideoTextureHandleKind::D3D11Texture2D],
                true,
                false,
            ),
            renderer_support: renderer_support(),
        }
    }

    fn renderer_support() -> GpuNativeDecodedFrameImportSupport {
        GpuNativeDecodedFrameImportSupport::ready(
            vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
            vec![GpuNativeDecodedFrameTextureFormat::Nv12],
        )
    }

    fn native_video_sampling() -> GpuNativeDecodedFrameVideoSampling {
        GpuNativeDecodedFrameVideoSampling::from_source_color_space(
            mondrian_core::ColorSpace::Rec709,
            GpuVideoRange::Limited,
            8,
            GpuVideoChromaLocation::Left,
        )
    }
}
