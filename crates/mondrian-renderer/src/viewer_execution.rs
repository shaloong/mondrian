//! Renderer-owned Viewer GPU execution contracts.
//!
//! These payloads contain only render, media, and effect facts. Playback
//! tickets, cache identities, Window registration, and headless completion are
//! Adapter metadata and deliberately remain outside this Interface.

use std::sync::Arc;

use mondrian_core::{types::BlendMode, ColorMatrixCoefficients, ColorSpace};
use mondrian_effects::{CompiledEffectGpuPlan, PreparedHeterogeneousCpuCompletion};
use mondrian_media::{
    DecodedFrameResidency, DecodedGpuFrameHandleKind, DecodedVideoChromaLocation,
    DecodedVideoMatrix, DecodedVideoRange, DecodedVideoSampling, DecodedVideoSurfaceFormat,
    PreviewNativeDecodedFrame,
};

#[cfg(target_os = "windows")]
use crate::D3D12NativeVideoImportBackend;
#[cfg(target_os = "macos")]
use crate::MetalNativeVideoImportBackend;
#[cfg(target_os = "linux")]
use crate::VulkanNativeVideoImportBackend;
#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
use crate::{execute_native_decoded_frame_import, GpuNativeDecodedFrameImportBackend};
use crate::{
    CpuColorFrame, CpuSourceColorFrame, GpuColorFrameIdAllocator, GpuColorFrameResource,
    GpuColorFrameWgpuResource, GpuColorFrameWgpuResourcePool, GpuNativeDecodedFrameImportContract,
    GpuNativeDecodedFrameImportError, GpuNativeDecodedFrameImportSupport,
    GpuNativeDecodedFrameTextureFormat, GpuNativeDecodedFrameVideoSampling, GpuVideoChromaLocation,
    GpuVideoRange, HeterogeneousGpuContinuationRequest, NativeVideoImportCandidateTimingReceipt,
    NativeVideoImportCandidateToken, NativeVideoImportCpuTimings,
    NativeVideoImportGpuTimingDiagnostics, NativeVideoImportGpuTimingPolicy,
    NativeVideoImportGpuTimingSample, RenderInputTransform, TimelineSolidColorLayer,
};

/// One exact CPU-prefix completion consumed by a Viewer GPU continuation.
///
/// The layer points at this frame-local table by address. Separating the
/// move-only completion from the reusable Viewer layer plan keeps render
/// topology immutable while ensuring the CPU value can be consumed exactly
/// once during command recording.
pub struct ViewerHeterogeneousGpuInput {
    /// Exact graph/generation/frame/resource binding expected by the renderer.
    pub request: HeterogeneousGpuContinuationRequest,
    /// Private CPU-prefix pixels and pending GPU suffix token chain.
    pub completion: PreparedHeterogeneousCpuCompletion,
}

/// One renderer-neutral source branch entering Viewer GPU execution.
///
/// Ordinary Timeline layers and two-input visual Transitions share this exact
/// source contract so media/color/effect preparation cannot diverge.
pub enum ViewerGpuSourceLayer {
    /// Working-space media with preferred GPU inputs and a CPU correctness fallback.
    Media {
        /// CPU working frame used if GPU input preparation cannot execute.
        frame: Option<CpuColorFrame>,
        /// Encoded CPU source for a GPU input color transform.
        gpu_source: Option<ViewerGpuMediaSource>,
        /// Native decoder surface for low-copy renderer import.
        native_source: Option<ViewerGpuNativeSource>,
        /// Address into
        /// [`crate::ViewerGpuExecutionRequest::heterogeneous_inputs`] when this
        /// source begins at an already-completed CPU Effect prefix.
        ///
        /// A heterogeneous source is exclusive with the ordinary CPU/GPU/native
        /// source fields and requires an identity `effect_plan`: the exact GPU
        /// suffix is already carried by the addressed completion.
        heterogeneous_input: Option<u32>,
        /// Layer opacity.
        opacity: f32,
        /// Canonical Timeline blend mode.
        blend_mode: BlendMode,
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
}

/// One input to a typed two-input Viewer visual Transition.
pub enum ViewerGpuTransitionInput {
    /// Explicit absence of coverage from a disabled endpoint.
    Transparent,
    /// Media or generated source prepared through the ordinary source seam.
    Source(ViewerGpuSourceLayer),
}

/// Complete payload for one typed two-input Viewer visual Transition.
///
/// Execution layers box this payload once so ordinary source and adjustment
/// nodes do not inherit the combined inline size of both Transition endpoints.
pub struct ViewerGpuCrossDissolveLayer {
    /// Earlier edit endpoint.
    pub left: ViewerGpuTransitionInput,
    /// Later edit endpoint.
    pub right: ViewerGpuTransitionInput,
    /// Normalized interpolation coefficient.
    pub progress: f32,
}

/// One renderer-neutral layer or graph node entering Viewer GPU execution.
pub enum ViewerGpuExecutionLayer {
    /// Ordinary source occupying one position in the bottom-to-top stack.
    Source(ViewerGpuSourceLayer),
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
    /// Two independently prepared sources replacing their endpoint Clips at
    /// one Track-stack position.
    CrossDissolve(Box<ViewerGpuCrossDissolveLayer>),
}

/// CPU media source plus its exact GPU input transform contract.
#[derive(Debug, Clone)]
pub struct ViewerGpuMediaSource {
    /// CPU-decoded RGBA8 or scene-linear float source.
    pub source: Arc<CpuSourceColorFrame>,
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
    /// Width materialized into the renderer working graph.
    pub materialization_width: u32,
    /// Height materialized into the renderer working graph.
    pub materialization_height: u32,
    /// Media-owned native frame payload consumed by renderer import.
    pub native_frame: Arc<PreviewNativeDecodedFrame>,
}

/// Renderer backend lifetime for native decoded-frame import.
pub struct ViewerNativeVideoImportRuntime {
    support: GpuNativeDecodedFrameImportSupport,
    #[cfg(target_os = "windows")]
    backend: Option<D3D12NativeVideoImportBackend>,
    #[cfg(target_os = "macos")]
    backend: Option<MetalNativeVideoImportBackend>,
    #[cfg(target_os = "linux")]
    backend: Option<VulkanNativeVideoImportBackend>,
}

impl ViewerNativeVideoImportRuntime {
    /// Create the backend implementation selected for one renderer device.
    pub fn new(adapter: &wgpu::Adapter, device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self::new_with_gpu_timing_policy(
            adapter,
            device,
            queue,
            NativeVideoImportGpuTimingPolicy::default(),
        )
    }

    /// Create the backend with an explicit native-import timing policy.
    pub fn new_with_gpu_timing_policy(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        gpu_timing_policy: NativeVideoImportGpuTimingPolicy,
    ) -> Self {
        Self::new_with_resource_pool_and_gpu_timing_policy(
            adapter,
            device,
            queue,
            Arc::new(GpuColorFrameWgpuResourcePool::default()),
            gpu_timing_policy,
        )
    }

    /// Create a backend that shares a device-scoped color-frame resource pool.
    pub fn new_with_resource_pool(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    ) -> Self {
        Self::new_with_resource_pool_and_gpu_timing_policy(
            adapter,
            device,
            queue,
            resource_pool,
            NativeVideoImportGpuTimingPolicy::default(),
        )
    }

    /// Create a shared-resource backend with an explicit timing policy.
    pub fn new_with_resource_pool_and_gpu_timing_policy(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
        gpu_timing_policy: NativeVideoImportGpuTimingPolicy,
    ) -> Self {
        #[cfg(target_os = "windows")]
        {
            match D3D12NativeVideoImportBackend::new_with_resource_pool_and_gpu_timing_policy(
                adapter,
                device,
                queue,
                resource_pool,
                gpu_timing_policy,
            ) {
                Ok(backend) => Self {
                    support: backend.support().clone(),
                    backend: Some(backend),
                },
                Err(error) => Self {
                    support: GpuNativeDecodedFrameImportSupport::unavailable_with_reason(
                        format!("{:?}", adapter.get_info().backend),
                        format!("native D3D12VA YUV + OCIO backend unavailable: {error}"),
                    ),
                    backend: None,
                },
            }
        }
        #[cfg(target_os = "macos")]
        {
            let _ = gpu_timing_policy;
            match MetalNativeVideoImportBackend::new_with_resource_pool(
                adapter,
                device,
                queue,
                resource_pool,
            ) {
                Ok(backend) => Self {
                    support: backend.support().clone(),
                    backend: Some(backend),
                },
                Err(error) => Self {
                    support: GpuNativeDecodedFrameImportSupport::unavailable_with_reason(
                        format!("{:?}", adapter.get_info().backend),
                        format!("native VideoToolbox Metal + OCIO backend unavailable: {error}"),
                    ),
                    backend: None,
                },
            }
        }
        #[cfg(target_os = "linux")]
        {
            let _ = gpu_timing_policy;
            match VulkanNativeVideoImportBackend::new_with_resource_pool(
                adapter,
                device,
                queue,
                resource_pool,
            ) {
                Ok(backend) => Self {
                    support: backend.support().clone(),
                    backend: Some(backend),
                },
                Err(error) => Self {
                    support: GpuNativeDecodedFrameImportSupport::unavailable_with_reason(
                        format!("{:?}", adapter.get_info().backend),
                        format!("native VA-API Vulkan + OCIO backend unavailable: {error}"),
                    ),
                    backend: None,
                },
            }
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            let _ = (device, queue, resource_pool, gpu_timing_policy);
            Self {
                support: unavailable_native_import_support(adapter, device.features()),
            }
        }
    }

    /// Return immutable native import capabilities and blocker evidence.
    pub fn support(&self) -> GpuNativeDecodedFrameImportSupport {
        self.support.clone()
    }

    /// Begin one explicit Viewer-candidate attribution scope.
    pub fn begin_viewer_candidate(&mut self) -> Option<NativeVideoImportCandidateToken> {
        #[cfg(target_os = "windows")]
        if let Some(backend) = self.backend.as_mut() {
            return backend.begin_viewer_candidate();
        }
        None
    }

    /// End the exact Viewer-candidate scope on success or failure.
    ///
    /// Active timing returns a move-only receipt only for a successful record.
    pub fn end_viewer_candidate(
        &mut self,
        candidate: Option<NativeVideoImportCandidateToken>,
        viewer_record_succeeded: bool,
    ) -> Option<NativeVideoImportCandidateTimingReceipt> {
        #[cfg(target_os = "windows")]
        if let Some(backend) = self.backend.as_mut() {
            return backend.end_viewer_candidate(candidate, viewer_record_succeeded);
        }
        #[cfg(not(target_os = "windows"))]
        let _ = (candidate, viewer_record_succeeded);
        None
    }

    /// Return accumulated native-import CPU attribution for the current candidate.
    pub fn frame_cpu_timings(&self) -> NativeVideoImportCpuTimings {
        #[cfg(target_os = "windows")]
        if let Some(backend) = self.backend.as_ref() {
            return backend.frame_cpu_timings();
        }
        #[cfg(target_os = "macos")]
        if let Some(backend) = self.backend.as_ref() {
            return backend.frame_cpu_timings();
        }
        #[cfg(target_os = "linux")]
        if let Some(backend) = self.backend.as_ref() {
            return backend.frame_cpu_timings();
        }
        NativeVideoImportCpuTimings::default()
    }

    /// Collect callbacks after the execution owner has polled the device.
    pub fn collect_gpu_timings_after_device_poll(&mut self) {
        #[cfg(target_os = "windows")]
        if let Some(backend) = self.backend.as_mut() {
            backend.collect_gpu_timings_after_device_poll();
        }
    }

    /// Drain completed native-import hardware timestamp samples.
    pub fn take_completed_gpu_timings(&mut self) -> Vec<NativeVideoImportGpuTimingSample> {
        #[cfg(target_os = "windows")]
        if let Some(backend) = self.backend.as_mut() {
            return backend.take_completed_gpu_timings();
        }
        Vec::new()
    }

    /// Cumulative native-import GPU timing coverage and availability.
    pub fn gpu_timing_diagnostics(&self) -> NativeVideoImportGpuTimingDiagnostics {
        #[cfg(target_os = "windows")]
        if let Some(backend) = self.backend.as_ref() {
            return backend.gpu_timing_diagnostics();
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if self.support.renderer_backend_ready {
            return NativeVideoImportGpuTimingDiagnostics::inactive(
                false,
                "native-import GPU timestamp attribution is not implemented for this backend",
            );
        }
        NativeVideoImportGpuTimingDiagnostics::inactive(
            false,
            self.support
                .unavailable_reason
                .clone()
                .unwrap_or_else(|| "native video import backend is unavailable".to_owned()),
        )
    }

    /// Bounded native-import contract-pool and bridge-entry residency.
    pub fn pool_residency(&self) -> (usize, usize) {
        #[cfg(target_os = "windows")]
        if let Some(backend) = self.backend.as_ref() {
            return (backend.contract_pool_count(), backend.bridge_entry_count());
        }
        (0, 0)
    }

    /// Decoder surfaces still retained only for an outstanding native bridge copy.
    pub fn retained_source_count(&self) -> usize {
        #[cfg(target_os = "windows")]
        if let Some(backend) = self.backend.as_ref() {
            return backend.retained_source_count();
        }
        0
    }

    /// Non-blockingly retire native decoder sources after their bridge copy completes.
    pub fn retire_completed_source_residency(
        &mut self,
    ) -> Result<usize, GpuNativeDecodedFrameImportError> {
        #[cfg(target_os = "windows")]
        if let Some(backend) = self.backend.as_mut() {
            return backend.retire_completed_source_residency();
        }
        Ok(0)
    }

    /// Import one native decoder payload into a renderer-owned working resource.
    pub fn import(
        &mut self,
        ids: &mut GpuColorFrameIdAllocator,
        source_color_space: ColorSpace,
        input_transform: &RenderInputTransform,
        output_width: u32,
        output_height: u32,
        native_frame: &PreviewNativeDecodedFrame,
    ) -> Result<GpuColorFrameResource<GpuColorFrameWgpuResource>, GpuNativeDecodedFrameImportError>
    {
        let source_texture_format = native_source_texture_format_from_decoded(
            native_frame.surface_format,
        )
        .ok_or_else(|| GpuNativeDecodedFrameImportError::BackendRejected {
            reason: format!(
                "decoded native surface format {:?} has no renderer import contract",
                native_frame.surface_format
            ),
        })?;
        let video_sampling = native_video_sampling_from_decoded(
            source_color_space,
            source_texture_format,
            native_frame.diagnostics.decoded_video_sampling,
        )
        .ok_or_else(|| GpuNativeDecodedFrameImportError::BackendRejected {
            reason: "decoded native surface has incomplete video sampling metadata".to_owned(),
        })?;
        let contract = GpuNativeDecodedFrameImportContract {
            width: native_frame.width,
            height: native_frame.height,
            output_width,
            output_height,
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
                GpuNativeDecodedFrameImportError::BackendRejected {
                    reason: self
                        .support
                        .unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "native video backend is unavailable".to_owned()),
                }
            })?;
            execute_native_decoded_frame_import(backend, ids, contract, native_frame)
                .map(|execution| execution.resource)
        }
        #[cfg(target_os = "macos")]
        {
            let backend = self.backend.as_mut().ok_or_else(|| {
                GpuNativeDecodedFrameImportError::BackendRejected {
                    reason: self
                        .support
                        .unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "native video backend is unavailable".to_owned()),
                }
            })?;
            execute_native_decoded_frame_import(backend, ids, contract, native_frame)
                .map(|execution| execution.resource)
        }
        #[cfg(target_os = "linux")]
        {
            let backend = self.backend.as_mut().ok_or_else(|| {
                GpuNativeDecodedFrameImportError::BackendRejected {
                    reason: self
                        .support
                        .unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "native video backend is unavailable".to_owned()),
                }
            })?;
            execute_native_decoded_frame_import(backend, ids, contract, native_frame)
                .map(|execution| execution.resource)
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            let _ = (ids, contract, native_frame);
            Err(GpuNativeDecodedFrameImportError::BackendRejected {
                reason: self
                    .support
                    .unavailable_reason
                    .clone()
                    .unwrap_or_else(|| "native video backend is unavailable".to_owned()),
            })
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
                DecodedVideoMatrix::Unknown
                | DecodedVideoMatrix::Unsupported
                | DecodedVideoMatrix::Rgb => return None,
                DecodedVideoMatrix::Bt709 => ColorMatrixCoefficients::Bt709,
                DecodedVideoMatrix::Bt2020NonConstant => ColorMatrixCoefficients::Bt2020NonConstant,
                DecodedVideoMatrix::Fcc => ColorMatrixCoefficients::Fcc,
                DecodedVideoMatrix::Bt470Bg => ColorMatrixCoefficients::Bt470Bg,
                DecodedVideoMatrix::Smpte170M => ColorMatrixCoefficients::Smpte170M,
                DecodedVideoMatrix::Smpte240M => ColorMatrixCoefficients::Smpte240M,
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

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
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

        for matrix in [DecodedVideoMatrix::Unknown, DecodedVideoMatrix::Rgb] {
            assert!(
                native_video_sampling_from_decoded(
                    ColorSpace::Rec2100Pq,
                    GpuNativeDecodedFrameTextureFormat::P010,
                    DecodedVideoSampling {
                        matrix,
                        range: DecodedVideoRange::Limited,
                        chroma_location: DecodedVideoChromaLocation::Left,
                        bit_depth: 10,
                    },
                )
                .is_none(),
                "YUV sampling must not infer or accept matrix {matrix:?}"
            );
        }
    }
}
