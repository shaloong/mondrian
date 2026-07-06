//! App-layer diagnostics for native decoded video texture import readiness.
//!
//! This module combines facts from media decode, platform capability probing,
//! and renderer backend support. None of those lower layers should depend on
//! each other just to explain why preview playback is still using CPU RGBA
//! uploads.

use mondrian_media::{DecodedFrameResidency, DecodedGpuFrameHandleKind};
use mondrian_platform::{NativeVideoTextureHandleKind, NativeVideoTextureImportProbeResult};
use mondrian_renderer::{
    GpuColorFrameTextureFormat, GpuNativeDecodedFrameImportSupport,
    GpuNativeDecodedFrameTextureFormat,
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
    /// The renderer backend cannot consume the decoded source texture format.
    RendererSourceTextureFormatUnsupported,
    /// The requested working texture format cannot preserve linear working pixels.
    RendererWorkingTextureFormatUnsupported,
    /// The full path can stay zero-copy.
    ReadyZeroCopy,
    /// The path cannot stay zero-copy but can use a declared low-copy fallback.
    ReadyLowCopy,
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
    /// Renderer working texture format requested after import/input transform.
    pub working_texture_format: GpuColorFrameTextureFormat,
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
    /// Whether the renderer backend reports native import readiness.
    pub renderer_backend_ready: bool,
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
        renderer_backend_ready: input.renderer_support.renderer_backend_ready,
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
            "renderer backend does not support native decoded-frame import",
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
    if !matches!(
        input.working_texture_format,
        GpuColorFrameTextureFormat::Rgba16Float | GpuColorFrameTextureFormat::Rgba32Float
    ) {
        return base.with_status(
            AppUiNativeVideoImportReadinessStatus::RendererWorkingTextureFormatUnsupported,
            "native decoded-frame import must produce a float linear working texture",
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

fn platform_handle_kind_for_decoder(
    handle_kind: DecodedGpuFrameHandleKind,
) -> NativeVideoTextureHandleKind {
    match handle_kind {
        DecodedGpuFrameHandleKind::D3D11Texture2D => NativeVideoTextureHandleKind::D3D11Texture2D,
        DecodedGpuFrameHandleKind::CVPixelBuffer => NativeVideoTextureHandleKind::CVPixelBuffer,
        DecodedGpuFrameHandleKind::VaapiSurface => NativeVideoTextureHandleKind::DmaBuf,
        DecodedGpuFrameHandleKind::CudaDeviceMemory => {
            NativeVideoTextureHandleKind::CudaDeviceMemory
        }
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
            working_texture_format: GpuColorFrameTextureFormat::Rgba16Float,
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
            renderer_support: GpuNativeDecodedFrameImportSupport::unavailable(),
            ..ready_input()
        });

        assert_eq!(
            report.status,
            AppUiNativeVideoImportReadinessStatus::RendererBackendUnavailable
        );
        assert!(!report.renderer_backend_ready);
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
    fn native_video_import_readiness_rejects_rgba8_working_texture() {
        let report = evaluate_native_video_import_readiness(AppUiNativeVideoImportReadinessInput {
            working_texture_format: GpuColorFrameTextureFormat::Rgba8Unorm,
            ..ready_input()
        });

        assert_eq!(
            report.status,
            AppUiNativeVideoImportReadinessStatus::RendererWorkingTextureFormatUnsupported
        );
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

    fn cpu_decoded_input() -> AppUiNativeVideoImportReadinessInput {
        AppUiNativeVideoImportReadinessInput {
            decoder_residency: DecodedFrameResidency::CpuRgba,
            decoder_handle_kind: None,
            source_texture_format: None,
            working_texture_format: GpuColorFrameTextureFormat::Rgba16Float,
            platform_probe: NativeVideoTextureImportProbeResult::unsupported("not probed"),
            renderer_support: GpuNativeDecodedFrameImportSupport::unavailable(),
        }
    }

    fn ready_input() -> AppUiNativeVideoImportReadinessInput {
        AppUiNativeVideoImportReadinessInput {
            decoder_residency: DecodedFrameResidency::GpuTexture,
            decoder_handle_kind: Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
            source_texture_format: Some(GpuNativeDecodedFrameTextureFormat::Nv12),
            working_texture_format: GpuColorFrameTextureFormat::Rgba16Float,
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
}
