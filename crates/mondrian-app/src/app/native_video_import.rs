//! App-layer diagnostics for native decoded video texture import readiness.
//!
//! This module combines facts from media decode and the device-bound renderer
//! backend. Native import is not a process-wide platform capability: it depends
//! on the exact decoder surface, wgpu adapter/device, HAL features, and
//! synchronization contract selected for this Viewer generation.

use mondrian_media::{
    DecodedFrameResidency, DecodedGpuFrameHandleKind, HwAccelDeviceSelector,
    PreviewHardwareDecodeRequest, PreviewNativeSurfaceHint,
};
#[cfg(test)]
use mondrian_media::{
    DecodedVideoChromaLocation, DecodedVideoRange, DecodedVideoSampling, DecodedVideoSurfaceFormat,
};
#[cfg(test)]
use mondrian_renderer::{
    native_source_texture_format_from_decoded, native_video_sampling_from_decoded,
};
use mondrian_renderer::{
    GpuNativeDecodedFrameImportMode, GpuNativeDecodedFrameImportSupport,
    GpuNativeDecodedFrameTextureFormat, GpuNativeDecodedFrameVideoSampling,
};
#[cfg(test)]
use mondrian_renderer::{GpuNativeDecodedFrameImportRoute, GpuVideoChromaLocation, GpuVideoRange};

/// Stable reason playback cannot request GPU-resident decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum PreviewHardwareDecodeAdmissionBlocker {
    /// Renderer runtime has not reported native decoded-frame import support yet.
    SupportUnknown,
    /// Renderer backend has no native decoded-frame import implementation.
    RendererImportUnavailable,
    /// Renderer reports native import but no decoder handle family.
    HandleSupportMissing,
    /// Renderer reports native import but no decoded source texture format.
    SourceTextureFormatSupportMissing,
    /// Renderer reports support without declaring the physical transfer mode.
    ImportModeMissing,
}

/// Stable readiness category for native decoded-frame import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum NativeVideoImportReadinessStatus {
    /// Current media preview path delivered CPU RGBA bytes, not a GPU decoder surface.
    CpuDecodedMedia,
    /// Decoder output claims GPU residency but no native handle family was reported.
    DecoderGpuHandleMissing,
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
    /// The native path performs one declared GPU-local bridge copy.
    ReadyLowCopy,
}

/// Input facts for native decoded-frame import readiness evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeVideoImportReadinessInput {
    /// Residency reported by the media decoder boundary.
    pub decoder_residency: DecodedFrameResidency,
    /// Native decoder handle family, when the decoder produced a GPU surface.
    pub decoder_handle_kind: Option<DecodedGpuFrameHandleKind>,
    /// Source texture format of the decoded GPU surface, when known.
    pub source_texture_format: Option<GpuNativeDecodedFrameTextureFormat>,
    /// Shader-visible sampling contract needed before renderer import can
    /// convert native video surfaces into encoded RGB and then working space.
    pub source_video_sampling: Option<GpuNativeDecodedFrameVideoSampling>,
    /// Renderer backend native decoded-frame import support contract.
    pub renderer_support: GpuNativeDecodedFrameImportSupport,
}

/// Point-in-time native decoded-frame import readiness report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct NativeVideoImportReadiness {
    /// Stable readiness status.
    pub status: NativeVideoImportReadinessStatus,
    /// Whether a zero-copy decoder-surface-to-renderer path is ready.
    pub zero_copy_ready: bool,
    /// Whether a declared GPU-local bridge-copy path is ready.
    pub low_copy_ready: bool,
    /// Whether the decoder output is GPU-resident.
    pub decoder_gpu_resident: bool,
    /// Decoder handle family, serialized as a stable diagnostic string.
    pub decoder_handle_kind: Option<String>,
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
    /// Physical transfer mode declared by the exact Renderer backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer_import_mode: Option<GpuNativeDecodedFrameImportMode>,
    /// Stable human-readable reason for the current status.
    pub reason: String,
}

/// Evaluate native decoded-frame import readiness from media and the exact
/// renderer-device Adapter facts.
pub(crate) fn evaluate_native_video_import_readiness(
    input: NativeVideoImportReadinessInput,
) -> NativeVideoImportReadiness {
    let decoder_gpu_resident = input.decoder_residency == DecodedFrameResidency::GpuTexture;
    let decoder_handle_kind = input.decoder_handle_kind.map(DecodedGpuFrameHandleKind::as_str);
    let renderer_supports_handle_kind = input
        .decoder_handle_kind
        .map(|kind| input.renderer_support.supports_handle_kind(kind))
        .unwrap_or(false);
    let renderer_supports_source_texture_format = input
        .source_texture_format
        .map(|format| input.renderer_support.supports_source_texture_format(format))
        .unwrap_or(false);
    let renderer_import_mode = input.decoder_handle_kind.and_then(|handle_kind| {
        input
            .source_texture_format
            .and_then(|format| input.renderer_support.import_mode_for(handle_kind, format))
    });

    let base = NativeVideoImportReadiness {
        status: NativeVideoImportReadinessStatus::CpuDecodedMedia,
        zero_copy_ready: false,
        low_copy_ready: false,
        decoder_gpu_resident,
        decoder_handle_kind: decoder_handle_kind.map(str::to_owned),
        renderer_backend_ready: input.renderer_support.renderer_backend_ready,
        renderer_backend_label: input.renderer_support.renderer_backend_label.clone(),
        renderer_unavailable_reason: input.renderer_support.unavailable_reason.clone(),
        renderer_supports_handle_kind,
        renderer_supports_source_texture_format,
        renderer_import_mode,
        reason: String::new(),
    };

    if !decoder_gpu_resident {
        return base.with_status(
            NativeVideoImportReadinessStatus::CpuDecodedMedia,
            "media preview delivered CPU RGBA bytes; native decoder texture import is not active",
        );
    }

    let Some(decoder_handle_kind) = input.decoder_handle_kind else {
        return base.with_status(
            NativeVideoImportReadinessStatus::DecoderGpuHandleMissing,
            "decoder reported GPU residency without a native handle family",
        );
    };
    if !input.renderer_support.renderer_backend_ready {
        return base.with_status(
            NativeVideoImportReadinessStatus::RendererBackendUnavailable,
            input
                .renderer_support
                .unavailable_reason
                .as_deref()
                .unwrap_or("renderer backend does not support native decoded-frame import"),
        );
    }
    if !renderer_supports_handle_kind {
        return base.with_status(
            NativeVideoImportReadinessStatus::RendererHandleUnsupported,
            "renderer backend does not support the decoder handle family",
        );
    }

    let Some(source_texture_format) = input.source_texture_format else {
        return base.with_status(
            NativeVideoImportReadinessStatus::SourceTextureFormatUnknown,
            "decoded GPU source texture format is unknown",
        );
    };
    if !input.renderer_support.supports_source_texture_format(source_texture_format) {
        return base.with_status(
            NativeVideoImportReadinessStatus::RendererSourceTextureFormatUnsupported,
            "renderer backend does not support the decoded source texture format",
        );
    }
    let Some(import_mode) = input
        .renderer_support
        .import_mode_for(decoder_handle_kind, source_texture_format)
    else {
        return base.with_status(
            NativeVideoImportReadinessStatus::RendererSourceTextureFormatUnsupported,
            "renderer backend has no exact route for this decoder handle and source format",
        );
    };
    if input.source_video_sampling.is_none() {
        return base.with_status(
            NativeVideoImportReadinessStatus::SourceVideoSamplingUnknown,
            "native decoded-frame import requires explicit video sampling metadata",
        );
    }
    base.with_ready(import_mode)
}

impl NativeVideoImportReadiness {
    fn with_status(
        mut self,
        status: NativeVideoImportReadinessStatus,
        reason: impl Into<String>,
    ) -> Self {
        self.status = status;
        self.reason = reason.into();
        self
    }

    fn with_ready(mut self, import_mode: GpuNativeDecodedFrameImportMode) -> Self {
        match import_mode {
            GpuNativeDecodedFrameImportMode::ZeroCopy => {
                self.status = NativeVideoImportReadinessStatus::ReadyZeroCopy;
                self.zero_copy_ready = true;
                self.reason =
                    "device-bound zero-copy native decoded-frame import is ready".to_owned();
            }
            GpuNativeDecodedFrameImportMode::GpuBridgeCopy => {
                self.status = NativeVideoImportReadinessStatus::ReadyLowCopy;
                self.low_copy_ready = true;
                self.reason =
                    "device-bound native decoded-frame import is ready with one GPU bridge copy"
                        .to_owned();
            }
        }
        self
    }
}

/// Shared device-bound renderer admission facts for playback hardware decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaybackHardwareDecodeAdmission {
    pub(crate) request: PreviewHardwareDecodeRequest,
    pub(crate) hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
    pub(crate) renderer_native_import_ready: bool,
    pub(crate) renderer_import_mode: Option<GpuNativeDecodedFrameImportMode>,
    pub(crate) native_import_admission_ready: bool,
    pub(crate) admission_blocker: Option<PreviewHardwareDecodeAdmissionBlocker>,
    pub(crate) renderer_supported_handle_kinds: u8,
    pub(crate) renderer_supported_source_texture_formats: u8,
    pub(crate) renderer_supports_nv12: bool,
    pub(crate) renderer_supports_p010: bool,
    pub(crate) renderer_supported_surface_hint_mask: u16,
}

/// Resolve one hardware-decode request from the exact renderer-device Adapter.
/// Capability evidence alone does not constitute frame execution.
pub(crate) fn resolve_playback_hardware_decode_admission(
    renderer_support: &GpuNativeDecodedFrameImportSupport,
) -> PlaybackHardwareDecodeAdmission {
    let renderer_supported_handle_kinds =
        saturated_u8_len(renderer_support.supported_handle_kinds.len());
    let renderer_supported_source_texture_formats =
        saturated_u8_len(renderer_support.supported_source_texture_formats.len());
    let renderer_supports_nv12 = renderer_support
        .supported_source_texture_formats
        .contains(&GpuNativeDecodedFrameTextureFormat::Nv12);
    let renderer_supports_p010 = renderer_support
        .supported_source_texture_formats
        .contains(&GpuNativeDecodedFrameTextureFormat::P010);
    let renderer_supported_surface_hint_mask =
        renderer_support.routes.iter().fold(0u16, |mask, route| {
            mask | native_surface_hint_bit_for_format(route.source_texture_format)
        });
    let renderer_native_import_ready =
        renderer_support.renderer_backend_ready && !renderer_support.routes.is_empty();
    let native_import_admission_ready = renderer_native_import_ready;
    let admission_blocker = if native_import_admission_ready {
        None
    } else if !renderer_support.renderer_backend_ready {
        Some(PreviewHardwareDecodeAdmissionBlocker::RendererImportUnavailable)
    } else if renderer_support.supported_handle_kinds.is_empty() {
        Some(PreviewHardwareDecodeAdmissionBlocker::HandleSupportMissing)
    } else if renderer_support.supported_source_texture_formats.is_empty() {
        Some(PreviewHardwareDecodeAdmissionBlocker::SourceTextureFormatSupportMissing)
    } else if renderer_support.routes.is_empty() {
        Some(PreviewHardwareDecodeAdmissionBlocker::ImportModeMissing)
    } else {
        Some(PreviewHardwareDecodeAdmissionBlocker::SupportUnknown)
    };
    let request = if native_import_admission_ready {
        PreviewHardwareDecodeRequest::PreferGpuResident
    } else {
        PreviewHardwareDecodeRequest::PreferHardwareDecode
    };
    PlaybackHardwareDecodeAdmission {
        request,
        hardware_decode_device_selector: renderer_support.hardware_decode_device_selector,
        renderer_native_import_ready,
        renderer_import_mode: renderer_support.import_mode,
        native_import_admission_ready,
        admission_blocker,
        renderer_supported_handle_kinds,
        renderer_supported_source_texture_formats,
        renderer_supports_nv12,
        renderer_supports_p010,
        renderer_supported_surface_hint_mask,
    }
}

pub(crate) const fn native_surface_hint_bit(hint: PreviewNativeSurfaceHint) -> u16 {
    match hint {
        PreviewNativeSurfaceHint::Nv12 => 1 << 0,
        PreviewNativeSurfaceHint::P010 => 1 << 1,
        PreviewNativeSurfaceHint::Yuv420p12 => 1 << 2,
        PreviewNativeSurfaceHint::Yuv420p16 => 1 << 3,
        PreviewNativeSurfaceHint::Yuv422p10 => 1 << 4,
        PreviewNativeSurfaceHint::Yuv422p12 => 1 << 5,
        PreviewNativeSurfaceHint::Yuv422p16 => 1 << 6,
        PreviewNativeSurfaceHint::Yuv444p10 => 1 << 7,
        PreviewNativeSurfaceHint::Yuv444p12 => 1 << 8,
        PreviewNativeSurfaceHint::Yuv444p16 => 1 << 9,
    }
}

const fn native_surface_hint_bit_for_format(format: GpuNativeDecodedFrameTextureFormat) -> u16 {
    match format {
        GpuNativeDecodedFrameTextureFormat::Nv12 => {
            native_surface_hint_bit(PreviewNativeSurfaceHint::Nv12)
        }
        GpuNativeDecodedFrameTextureFormat::P010 => {
            native_surface_hint_bit(PreviewNativeSurfaceHint::P010)
        }
        GpuNativeDecodedFrameTextureFormat::P012 => {
            native_surface_hint_bit(PreviewNativeSurfaceHint::Yuv420p12)
        }
        GpuNativeDecodedFrameTextureFormat::P016 => {
            native_surface_hint_bit(PreviewNativeSurfaceHint::Yuv420p16)
        }
        GpuNativeDecodedFrameTextureFormat::P210 | GpuNativeDecodedFrameTextureFormat::Y210 => {
            native_surface_hint_bit(PreviewNativeSurfaceHint::Yuv422p10)
        }
        GpuNativeDecodedFrameTextureFormat::P212 | GpuNativeDecodedFrameTextureFormat::Y212 => {
            native_surface_hint_bit(PreviewNativeSurfaceHint::Yuv422p12)
        }
        GpuNativeDecodedFrameTextureFormat::P216 => {
            native_surface_hint_bit(PreviewNativeSurfaceHint::Yuv422p16)
        }
        GpuNativeDecodedFrameTextureFormat::P410 | GpuNativeDecodedFrameTextureFormat::Xv30 => {
            native_surface_hint_bit(PreviewNativeSurfaceHint::Yuv444p10)
        }
        GpuNativeDecodedFrameTextureFormat::P412 | GpuNativeDecodedFrameTextureFormat::Xv36 => {
            native_surface_hint_bit(PreviewNativeSurfaceHint::Yuv444p12)
        }
        GpuNativeDecodedFrameTextureFormat::P416 => {
            native_surface_hint_bit(PreviewNativeSurfaceHint::Yuv444p16)
        }
        GpuNativeDecodedFrameTextureFormat::Rgba8Unorm
        | GpuNativeDecodedFrameTextureFormat::Bgra8Unorm
        | GpuNativeDecodedFrameTextureFormat::Rgba16Float
        | GpuNativeDecodedFrameTextureFormat::Rgba32Float => 0,
    }
}

fn saturated_u8_len(len: usize) -> u8 {
    len.min(usize::from(u8::MAX)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_hardware_decode_admission_requires_device_bound_renderer_import() {
        let selector = HwAccelDeviceSelector::D3D12VaAdapterIndex(2);
        let renderer_support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
            vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
            vec![GpuNativeDecodedFrameTextureFormat::P010],
        )
        .with_hardware_decode_device_selector(selector);
        let admission = resolve_playback_hardware_decode_admission(&renderer_support);

        assert_eq!(
            admission.request,
            PreviewHardwareDecodeRequest::PreferGpuResident
        );
        assert!(admission.renderer_native_import_ready);
        assert_eq!(
            admission.renderer_import_mode,
            Some(GpuNativeDecodedFrameImportMode::ZeroCopy)
        );
        assert!(admission.native_import_admission_ready);
        assert_eq!(admission.admission_blocker, None);
        assert_eq!(admission.hardware_decode_device_selector, Some(selector));
        assert_eq!(admission.renderer_supported_handle_kinds, 1);
        assert_eq!(admission.renderer_supported_source_texture_formats, 1);
        assert!(!admission.renderer_supports_nv12);
        assert!(admission.renderer_supports_p010);
    }

    #[test]
    fn playback_hardware_decode_admission_uses_cpu_transfer_when_backend_is_missing() {
        let renderer_support = GpuNativeDecodedFrameImportSupport::unavailable_with_reason(
            "Vulkan",
            "DMA-BUF import is unavailable",
        );

        let admission = resolve_playback_hardware_decode_admission(&renderer_support);

        assert_eq!(
            admission.request,
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        );
        assert!(!admission.renderer_native_import_ready);
        assert!(!admission.native_import_admission_ready);
        assert_eq!(
            admission.admission_blocker,
            Some(PreviewHardwareDecodeAdmissionBlocker::RendererImportUnavailable)
        );
        assert_eq!(admission.renderer_supported_handle_kinds, 0);
        assert_eq!(admission.renderer_supported_source_texture_formats, 0);
    }

    #[test]
    fn playback_hardware_decode_admission_rejects_empty_device_contract() {
        let renderer_support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
            Vec::new(),
            vec![GpuNativeDecodedFrameTextureFormat::P010],
        );

        let admission = resolve_playback_hardware_decode_admission(&renderer_support);

        assert_eq!(
            admission.request,
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        );
        assert!(!admission.renderer_native_import_ready);
        assert!(!admission.native_import_admission_ready);
        assert_eq!(
            admission.admission_blocker,
            Some(PreviewHardwareDecodeAdmissionBlocker::HandleSupportMissing)
        );
        assert_eq!(admission.renderer_supported_handle_kinds, 0);
        assert_eq!(admission.renderer_supported_source_texture_formats, 1);
    }

    #[test]
    fn playback_hardware_decode_admission_rejects_missing_transfer_mode() {
        let mut renderer_support = renderer_support();
        renderer_support.routes.clear();
        renderer_support.import_mode = None;

        let admission = resolve_playback_hardware_decode_admission(&renderer_support);

        assert_eq!(
            admission.request,
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        );
        assert!(!admission.native_import_admission_ready);
        assert_eq!(
            admission.admission_blocker,
            Some(PreviewHardwareDecodeAdmissionBlocker::ImportModeMissing)
        );
    }

    #[test]
    fn native_video_import_readiness_reports_cpu_decoded_media() {
        let report = evaluate_native_video_import_readiness(cpu_decoded_input());

        assert_eq!(
            report.status,
            NativeVideoImportReadinessStatus::CpuDecodedMedia
        );
        assert!(!report.decoder_gpu_resident);
        assert!(!report.zero_copy_ready);
        assert!(!report.low_copy_ready);
        assert!(report.reason.contains("CPU RGBA"));
    }

    #[test]
    fn native_video_import_readiness_reports_renderer_unavailable() {
        let report = evaluate_native_video_import_readiness(NativeVideoImportReadinessInput {
            renderer_support: GpuNativeDecodedFrameImportSupport::unavailable_with_reason(
                "Dx12",
                "D3D11 shared texture import bridge is not connected",
            ),
            ..ready_input()
        });

        assert_eq!(
            report.status,
            NativeVideoImportReadinessStatus::RendererBackendUnavailable
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
    fn native_video_import_readiness_requires_source_texture_format() {
        let report = evaluate_native_video_import_readiness(NativeVideoImportReadinessInput {
            source_texture_format: None,
            ..ready_input()
        });

        assert_eq!(
            report.status,
            NativeVideoImportReadinessStatus::SourceTextureFormatUnknown
        );
        assert!(!report.renderer_supports_source_texture_format);
    }

    #[test]
    fn native_video_import_readiness_requires_video_sampling_contract() {
        let report = evaluate_native_video_import_readiness(NativeVideoImportReadinessInput {
            source_video_sampling: None,
            ..ready_input()
        });

        assert_eq!(
            report.status,
            NativeVideoImportReadinessStatus::SourceVideoSamplingUnknown
        );
        assert!(!report.zero_copy_ready);
        assert!(report.reason.contains("video sampling"));
    }

    #[test]
    fn native_video_import_readiness_reports_zero_copy_ready() {
        let report = evaluate_native_video_import_readiness(ready_input());

        assert_eq!(
            report.status,
            NativeVideoImportReadinessStatus::ReadyZeroCopy
        );
        assert!(report.zero_copy_ready);
        assert!(!report.low_copy_ready);
        assert!(report.renderer_supports_handle_kind);
        assert!(report.renderer_supports_source_texture_format);
    }

    #[test]
    fn native_video_import_readiness_reports_gpu_bridge_copy_without_claiming_zero_copy() {
        let report = evaluate_native_video_import_readiness(NativeVideoImportReadinessInput {
            renderer_support: GpuNativeDecodedFrameImportSupport::ready_gpu_bridge_copy(
                vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
                vec![GpuNativeDecodedFrameTextureFormat::Nv12],
            ),
            ..ready_input()
        });

        assert_eq!(
            report.status,
            NativeVideoImportReadinessStatus::ReadyLowCopy
        );
        assert!(!report.zero_copy_ready);
        assert!(report.low_copy_ready);
        assert_eq!(
            report.renderer_import_mode,
            Some(GpuNativeDecodedFrameImportMode::GpuBridgeCopy)
        );
    }

    #[test]
    fn native_video_import_readiness_requires_one_exact_handle_format_route() {
        let support = GpuNativeDecodedFrameImportSupport::try_ready_routes(vec![
            GpuNativeDecodedFrameImportRoute {
                handle_kind: DecodedGpuFrameHandleKind::D3D11Texture2D,
                source_texture_format: GpuNativeDecodedFrameTextureFormat::P010,
                import_mode: GpuNativeDecodedFrameImportMode::ZeroCopy,
            },
            GpuNativeDecodedFrameImportRoute {
                handle_kind: DecodedGpuFrameHandleKind::D3D12Resource,
                source_texture_format: GpuNativeDecodedFrameTextureFormat::Nv12,
                import_mode: GpuNativeDecodedFrameImportMode::GpuBridgeCopy,
            },
        ])
        .expect("non-Cartesian renderer route matrix");
        let report = evaluate_native_video_import_readiness(NativeVideoImportReadinessInput {
            renderer_support: support,
            ..ready_input()
        });

        assert_eq!(
            report.status,
            NativeVideoImportReadinessStatus::RendererSourceTextureFormatUnsupported
        );
        assert!(report.renderer_supports_handle_kind);
        assert!(report.renderer_supports_source_texture_format);
        assert_eq!(report.renderer_import_mode, None);
        assert!(report.reason.contains("no exact route"));
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
                matrix: mondrian_media::DecodedVideoMatrix::Bt709,
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
                matrix: mondrian_media::DecodedVideoMatrix::Bt2020NonConstant,
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
                    matrix: mondrian_media::DecodedVideoMatrix::Bt709,
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
                    matrix: mondrian_media::DecodedVideoMatrix::Bt709,
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
                    matrix: mondrian_media::DecodedVideoMatrix::Bt709,
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
                    matrix: mondrian_media::DecodedVideoMatrix::Rgb,
                    range: DecodedVideoRange::Full,
                    chroma_location: DecodedVideoChromaLocation::Unknown,
                    bit_depth: 8,
                },
            ),
            None
        );
    }

    fn cpu_decoded_input() -> NativeVideoImportReadinessInput {
        NativeVideoImportReadinessInput {
            decoder_residency: DecodedFrameResidency::CpuRgba,
            decoder_handle_kind: None,
            source_texture_format: None,
            source_video_sampling: None,
            renderer_support: GpuNativeDecodedFrameImportSupport::unavailable(),
        }
    }

    fn ready_input() -> NativeVideoImportReadinessInput {
        NativeVideoImportReadinessInput {
            decoder_residency: DecodedFrameResidency::GpuTexture,
            decoder_handle_kind: Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
            source_texture_format: Some(GpuNativeDecodedFrameTextureFormat::Nv12),
            source_video_sampling: Some(native_video_sampling()),
            renderer_support: renderer_support(),
        }
    }

    fn renderer_support() -> GpuNativeDecodedFrameImportSupport {
        GpuNativeDecodedFrameImportSupport::ready_zero_copy(
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
