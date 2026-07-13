//! Renderer-owned Viewer GPU execution contracts.
//!
//! These payloads contain only render, media, and effect facts. Playback
//! tickets, cache identities, Window registration, and headless completion are
//! Adapter metadata and deliberately remain outside this Interface.

use std::sync::Arc;

use mondrian_core::{types::BlendMode, ColorMatrixCoefficients, ColorSpace};
use mondrian_effects::CompiledEffectGpuPlan;
use mondrian_media::{
    DecodedFrameResidency, DecodedGpuFrameHandleKind, DecodedVideoChromaLocation,
    DecodedVideoMatrix, DecodedVideoRange, DecodedVideoSampling, DecodedVideoSurfaceFormat,
    PreviewNativeDecodedFrame,
};

#[cfg(target_os = "windows")]
use crate::{
    execute_native_decoded_frame_import, D3D11Dx12NativeVideoImportBackend,
    GpuNativeDecodedFrameImportBackend,
};
use crate::{
    CpuColorFrame, CpuEncodedColorFrame, GpuColorFrameIdAllocator, GpuColorFrameResource,
    GpuColorFrameWgpuResource, GpuNativeDecodedFrameImportContract,
    GpuNativeDecodedFrameImportSupport, GpuNativeDecodedFrameTextureFormat,
    GpuNativeDecodedFrameVideoSampling, GpuVideoChromaLocation, GpuVideoRange,
    RenderInputTransform, TimelineSolidColorLayer,
};

/// One renderer-neutral layer entering Viewer GPU execution.
pub enum ViewerGpuExecutionLayer {
    /// Working-space media with preferred GPU inputs and a CPU correctness fallback.
    Media {
        /// CPU working frame used if GPU input preparation cannot execute.
        frame: Option<CpuColorFrame>,
        /// Encoded CPU source for a GPU input color transform.
        gpu_source: Option<ViewerGpuMediaSource>,
        /// Native decoder surface for low-copy renderer import.
        native_source: Option<ViewerGpuNativeSource>,
        /// Layer opacity.
        opacity: f32,
        /// Timeline affine transform.
        transform: [f32; 6],
        /// Working-space GPU effect plan.
        effect_plan: Arc<CompiledEffectGpuPlan>,
        /// Timeline seed for temporal effects.
        frame_seed: i64,
    },
    /// Full-frame solid color.
    SolidColor {
        /// Solid layer contract.
        layer: TimelineSolidColorLayer,
        /// Working-space GPU effect plan.
        effect_plan: Arc<CompiledEffectGpuPlan>,
    },
    /// Full-frame adjustment over the current working composite.
    Adjustment {
        /// Working-space GPU effect plan.
        effect_plan: Arc<CompiledEffectGpuPlan>,
        /// Adjustment opacity.
        opacity: f32,
        /// Adjustment blend mode.
        blend_mode: BlendMode,
        /// Timeline seed for temporal effects.
        frame_seed: i64,
    },
}

/// Encoded CPU media source plus its exact GPU input transform contract.
#[derive(Debug, Clone)]
pub struct ViewerGpuMediaSource {
    /// CPU-decoded encoded RGBA source.
    pub source: CpuEncodedColorFrame,
    /// Source/import to timeline-working-space transform.
    pub input_transform: RenderInputTransform,
    /// Residency reported by the decoder boundary.
    pub decoder_residency: DecodedFrameResidency,
    /// Native decoder handle family, when one was produced.
    pub decoder_handle_kind: Option<DecodedGpuFrameHandleKind>,
    /// Decoder output surface before CPU RGBA conversion.
    pub decoded_surface_format: DecodedVideoSurfaceFormat,
    /// Decoder-reported video sampling facts.
    pub decoded_video_sampling: DecodedVideoSampling,
}

/// Native decoder source plus its complete source-to-working transform.
#[derive(Debug, Clone)]
pub struct ViewerGpuNativeSource {
    /// Resolved source color space represented by the native surface.
    pub source_color_space: ColorSpace,
    /// Complete source-to-working input transform.
    pub input_transform: RenderInputTransform,
    /// Media-owned native frame payload consumed by renderer import.
    pub native_frame: Arc<PreviewNativeDecodedFrame>,
}

/// Renderer backend lifetime for native decoded-frame import.
pub struct ViewerNativeVideoImportRuntime {
    support: GpuNativeDecodedFrameImportSupport,
    #[cfg(target_os = "windows")]
    backend: Option<D3D11Dx12NativeVideoImportBackend>,
}

impl ViewerNativeVideoImportRuntime {
    /// Create the backend implementation selected for one renderer device.
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
                support: unavailable_native_import_support(adapter, device.features()),
            }
        }
    }

    /// Return immutable native import capabilities and blocker evidence.
    pub fn support(&self) -> GpuNativeDecodedFrameImportSupport {
        self.support.clone()
    }

    /// Import one native decoder payload into a renderer-owned working resource.
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

/// Map a media decoded-surface fact into the renderer import format.
pub fn native_source_texture_format_from_decoded(
    format: DecodedVideoSurfaceFormat,
) -> Option<GpuNativeDecodedFrameTextureFormat> {
    GpuNativeDecodedFrameTextureFormat::try_from(format).ok()
}

/// Resolve decoder sampling facts into a fail-closed renderer contract.
pub fn native_video_sampling_from_decoded(
    source_color_space: ColorSpace,
    source_texture_format: GpuNativeDecodedFrameTextureFormat,
    decoded: DecodedVideoSampling,
) -> Option<GpuNativeDecodedFrameVideoSampling> {
    let range = match decoded.range {
        DecodedVideoRange::Limited => GpuVideoRange::Limited,
        DecodedVideoRange::Full => GpuVideoRange::Full,
        DecodedVideoRange::Unknown => return None,
    };
    if decoded.bit_depth != expected_native_source_bit_depth(source_texture_format) {
        return None;
    }
    let (matrix, chroma_location) = match source_texture_format {
        GpuNativeDecodedFrameTextureFormat::Nv12 | GpuNativeDecodedFrameTextureFormat::P010 => {
            let matrix = match decoded.matrix {
                DecodedVideoMatrix::Unknown => source_color_space.encoding().matrix,
                DecodedVideoMatrix::Unsupported => return None,
                DecodedVideoMatrix::Bt709 => ColorMatrixCoefficients::Bt709,
                DecodedVideoMatrix::Bt2020NonConstant => ColorMatrixCoefficients::Bt2020NonConstant,
                DecodedVideoMatrix::Fcc => ColorMatrixCoefficients::Fcc,
                DecodedVideoMatrix::Bt470Bg => ColorMatrixCoefficients::Bt470Bg,
                DecodedVideoMatrix::Smpte170M => ColorMatrixCoefficients::Smpte170M,
                DecodedVideoMatrix::Smpte240M => ColorMatrixCoefficients::Smpte240M,
                DecodedVideoMatrix::Rgb => return None,
            };
            (
                matrix,
                decoded_chroma_location_to_gpu(decoded.chroma_location)?,
            )
        }
        GpuNativeDecodedFrameTextureFormat::Rgba8Unorm
        | GpuNativeDecodedFrameTextureFormat::Bgra8Unorm => {
            if source_color_space.encoding().matrix != ColorMatrixCoefficients::Rgb {
                return None;
            }
            (
                ColorMatrixCoefficients::Rgb,
                GpuVideoChromaLocation::Unspecified,
            )
        }
    };
    Some(GpuNativeDecodedFrameVideoSampling {
        range,
        matrix,
        transfer: source_color_space.encoding().transfer,
        bit_depth: decoded.bit_depth,
        chroma_location,
    })
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

#[cfg(not(target_os = "windows"))]
fn unavailable_native_import_support(
    adapter: &wgpu::Adapter,
    device_features: wgpu::Features,
) -> GpuNativeDecodedFrameImportSupport {
    let info = adapter.get_info();
    let reason = match info.backend {
        wgpu::Backend::Dx12
            if !device_features.intersects(
                wgpu::Features::TEXTURE_FORMAT_NV12 | wgpu::Features::TEXTURE_FORMAT_P010,
            ) =>
        {
            "wgpu Dx12 device has no enabled native video texture format".to_owned()
        }
        wgpu::Backend::Dx12 => {
            "wgpu Dx12 native video backend was not constructed on this platform".to_owned()
        }
        wgpu::Backend::Vulkan => {
            "wgpu Vulkan renderer has no external-memory native video import bridge".to_owned()
        }
        wgpu::Backend::Metal => {
            "wgpu Metal renderer has no CVPixelBuffer/IOSurface native video import bridge"
                .to_owned()
        }
        wgpu::Backend::Gl => "wgpu GL renderer has no native video import bridge".to_owned(),
        wgpu::Backend::BrowserWebGpu => {
            "browser WebGPU cannot import desktop native decoder surfaces".to_owned()
        }
        wgpu::Backend::Noop => "noop renderer cannot import native decoder surfaces".to_owned(),
    };
    GpuNativeDecodedFrameImportSupport::unavailable_with_reason(
        format!("{:?}", info.backend),
        reason,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_surface_mapping_is_explicit_and_fail_closed() {
        assert_eq!(
            native_source_texture_format_from_decoded(DecodedVideoSurfaceFormat::Nv12),
            Some(GpuNativeDecodedFrameTextureFormat::Nv12)
        );
        assert_eq!(
            native_source_texture_format_from_decoded(DecodedVideoSurfaceFormat::P010),
            Some(GpuNativeDecodedFrameTextureFormat::P010)
        );
        assert_eq!(
            native_source_texture_format_from_decoded(DecodedVideoSurfaceFormat::Unknown),
            None
        );
    }

    #[test]
    fn p010_sampling_requires_proven_ten_bit_yuv_facts() {
        let valid = native_video_sampling_from_decoded(
            ColorSpace::Rec2100Pq,
            GpuNativeDecodedFrameTextureFormat::P010,
            DecodedVideoSampling {
                matrix: DecodedVideoMatrix::Bt2020NonConstant,
                range: DecodedVideoRange::Limited,
                chroma_location: DecodedVideoChromaLocation::Left,
                bit_depth: 10,
            },
        )
        .expect("valid P010 sampling");
        assert_eq!(valid.range, GpuVideoRange::Limited);
        assert_eq!(valid.bit_depth, 10);

        assert!(native_video_sampling_from_decoded(
            ColorSpace::Rec2100Pq,
            GpuNativeDecodedFrameTextureFormat::P010,
            DecodedVideoSampling {
                matrix: DecodedVideoMatrix::Bt2020NonConstant,
                range: DecodedVideoRange::Unknown,
                chroma_location: DecodedVideoChromaLocation::Left,
                bit_depth: 10,
            },
        )
        .is_none());
        assert!(native_video_sampling_from_decoded(
            ColorSpace::Rec2100Pq,
            GpuNativeDecodedFrameTextureFormat::P010,
            DecodedVideoSampling {
                matrix: DecodedVideoMatrix::Bt2020NonConstant,
                range: DecodedVideoRange::Limited,
                chroma_location: DecodedVideoChromaLocation::Left,
                bit_depth: 8,
            },
        )
        .is_none());
    }

    #[test]
    fn native_sampling_preserves_explicit_bt601_and_rejects_unsupported_matrix() {
        let decoded = DecodedVideoSampling {
            matrix: DecodedVideoMatrix::Smpte170M,
            range: DecodedVideoRange::Limited,
            chroma_location: DecodedVideoChromaLocation::Left,
            bit_depth: 8,
        };
        let sampling = native_video_sampling_from_decoded(
            ColorSpace::Rec601Ntsc,
            GpuNativeDecodedFrameTextureFormat::Nv12,
            decoded,
        )
        .expect("BT.601 matrix can enter the native GPU conversion path");
        assert_eq!(sampling.matrix, ColorMatrixCoefficients::Smpte170M);
        assert_eq!(
            sampling.transfer,
            mondrian_core::ColorTransferCharacteristic::Smpte170M
        );

        assert!(native_video_sampling_from_decoded(
            ColorSpace::Rec2100Pq,
            GpuNativeDecodedFrameTextureFormat::P010,
            DecodedVideoSampling {
                matrix: DecodedVideoMatrix::Unsupported,
                range: DecodedVideoRange::Limited,
                chroma_location: DecodedVideoChromaLocation::Left,
                bit_depth: 10,
            },
        )
        .is_none());
    }
}
