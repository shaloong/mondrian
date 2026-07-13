//! Complete D3D11 decoder-surface to OCIO working-frame backend.

use super::windows_d3d11::{
    renderer_adapter_dxgi_index, renderer_adapter_luid,
    validated_d3d11_native_decoded_frame_for_luid, D3D11NativeDecodedFrameInspectionError,
    NativeVideoAdapterLuid, ValidatedD3D11NativeDecodedFrame,
};
use super::windows_d3d11_bridge::{
    D3D11Dx12PreparedVideoFrame, D3D11Dx12SharedVideoTexture, D3D11Dx12SharedVideoTextureError,
};
use crate::{
    ColorFrameResidency, GpuColorFrameResource, GpuColorFrameTextureFormat,
    GpuColorFrameWgpuResource, GpuColorFrameWgpuResourcePool, GpuNativeDecodedFrameImportBackend,
    GpuNativeDecodedFrameImportError, GpuNativeDecodedFrameImportPlan,
    GpuNativeDecodedFrameImportSupport, GpuNativeDecodedFrameTextureFormat,
    GpuNativeDecodedFrameVideoSampling, GpuNativeVideoExtent, GpuNativeYuvDecodePlan,
    GpuNativeYuvDecoder, GpuNativeYuvPlaneViews, GpuNativeYuvPreparedPass,
    NativeVideoImportCpuTimings, RenderColorTransformGpuOptions,
    RenderGpuInputStageRuntimeRecordError, RenderGpuOutputBoundaryRuntime,
    RenderGpuOutputBoundaryRuntimeOwnedBackendContext,
};
use mondrian_core::types::ColorSpace;
use mondrian_media::{DecodedGpuFrameHandleKind, PreviewNativeDecodedFrame};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use windows::core::Interface;

/// Error creating the complete Windows native decoded-frame renderer backend.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum D3D11Dx12NativeVideoImportBackendCreateError {
    /// The active renderer adapter cannot provide a stable DX12 LUID contract.
    #[error(transparent)]
    RendererAdapter(#[from] D3D11NativeDecodedFrameInspectionError),
    /// The wgpu device enabled neither native two-plane format.
    #[error("wgpu device enabled neither TEXTURE_FORMAT_NV12 nor TEXTURE_FORMAT_P010")]
    NoNativeYuvTextureFormats,
    /// A zero-sized in-flight pool could never import a frame.
    #[error("native video bridge pool limit must be greater than zero")]
    ZeroBridgePoolLimit,
}

/// Resource-pool policy for the Windows native video import backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct D3D11Dx12NativeVideoImportBackendOptions {
    /// Maximum bridge entries for one immutable source/sampling contract.
    /// Reaching the limit returns busy without a CPU wait.
    pub max_frames_in_flight_per_contract: usize,
}

impl Default for D3D11Dx12NativeVideoImportBackendOptions {
    fn default() -> Self {
        Self { max_frames_in_flight_per_contract: 4 }
    }
}

/// Complete low-copy D3D11 decoder-surface import backend.
///
/// The backend pools bridge entries per source device and immutable video
/// sampling contract. Each entry reuses its shared native texture, timeline
/// fence, plane bind group, and encoded RGB intermediate. Entries expand only
/// when every matching entry is still in flight; CPU waits are never used for
/// normal playback concurrency.
pub struct D3D11Dx12NativeVideoImportBackend {
    renderer_adapter_luid: NativeVideoAdapterLuid,
    device: wgpu::Device,
    queue: wgpu::Queue,
    support: GpuNativeDecodedFrameImportSupport,
    yuv_decoder: GpuNativeYuvDecoder,
    color_runtime: RenderGpuOutputBoundaryRuntime,
    pools: HashMap<D3D11BridgePoolKey, Vec<D3D11NativeVideoPipelineEntry>>,
    options: D3D11Dx12NativeVideoImportBackendOptions,
    frame_cpu_timings: NativeVideoImportCpuTimings,
}

impl D3D11Dx12NativeVideoImportBackend {
    /// Create a backend bound to one wgpu DX12 device/queue.
    pub fn new(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Self, D3D11Dx12NativeVideoImportBackendCreateError> {
        Self::new_with_options(
            adapter,
            device,
            queue,
            D3D11Dx12NativeVideoImportBackendOptions::default(),
        )
    }

    /// Create a backend with an explicit bounded in-flight pool policy.
    pub fn new_with_options(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        options: D3D11Dx12NativeVideoImportBackendOptions,
    ) -> Result<Self, D3D11Dx12NativeVideoImportBackendCreateError> {
        Self::new_with_options_and_resource_pool(
            adapter,
            device,
            queue,
            options,
            Arc::new(GpuColorFrameWgpuResourcePool::default()),
        )
    }

    /// Create a backend that shares renderer color-frame resources across stages.
    pub fn new_with_resource_pool(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    ) -> Result<Self, D3D11Dx12NativeVideoImportBackendCreateError> {
        Self::new_with_options_and_resource_pool(
            adapter,
            device,
            queue,
            D3D11Dx12NativeVideoImportBackendOptions::default(),
            resource_pool,
        )
    }

    /// Create a backend with explicit bridge and shared-resource policies.
    pub fn new_with_options_and_resource_pool(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        options: D3D11Dx12NativeVideoImportBackendOptions,
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    ) -> Result<Self, D3D11Dx12NativeVideoImportBackendCreateError> {
        if options.max_frames_in_flight_per_contract == 0 {
            return Err(D3D11Dx12NativeVideoImportBackendCreateError::ZeroBridgePoolLimit);
        }
        let renderer_adapter_luid = renderer_adapter_luid(adapter)?;
        let decoder_adapter_index = renderer_adapter_dxgi_index(adapter)?;
        let mut formats = Vec::with_capacity(2);
        if device.features().contains(wgpu::Features::TEXTURE_FORMAT_NV12) {
            formats.push(GpuNativeDecodedFrameTextureFormat::Nv12);
        }
        if device.features().contains(wgpu::Features::TEXTURE_FORMAT_P010) {
            formats.push(GpuNativeDecodedFrameTextureFormat::P010);
        }
        if formats.is_empty() {
            return Err(D3D11Dx12NativeVideoImportBackendCreateError::NoNativeYuvTextureFormats);
        }
        let support = GpuNativeDecodedFrameImportSupport::ready(
            vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
            formats,
        )
        .with_hardware_decode_device_selector(
            mondrian_media::HwAccelDeviceSelector::D3D11VaAdapterIndex(decoder_adapter_index),
        )
        .with_renderer_backend_label("wgpu Dx12 D3D11 shared YUV + OCIO");
        Ok(Self {
            renderer_adapter_luid,
            device: device.clone(),
            queue: queue.clone(),
            support,
            yuv_decoder: GpuNativeYuvDecoder::new(device),
            color_runtime: RenderGpuOutputBoundaryRuntime::with_resource_pool(resource_pool),
            pools: HashMap::new(),
            options,
            frame_cpu_timings: NativeVideoImportCpuTimings::default(),
        })
    }

    /// Return the number of reusable bridge entries currently allocated.
    pub fn bridge_entry_count(&self) -> usize {
        self.pools.values().map(Vec::len).sum()
    }

    /// Borrow the OCIO runtime used by the native input path.
    pub fn color_runtime(&self) -> &RenderGpuOutputBoundaryRuntime {
        &self.color_runtime
    }

    /// Reset CPU attribution before recording one Viewer candidate.
    pub fn reset_frame_cpu_timings(&mut self) {
        self.frame_cpu_timings = NativeVideoImportCpuTimings::default();
    }

    /// Return accumulated native-import CPU attribution for the current candidate.
    pub fn frame_cpu_timings(&self) -> NativeVideoImportCpuTimings {
        self.frame_cpu_timings
    }

    fn import_frame(
        &mut self,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &PreviewNativeDecodedFrame,
    ) -> Result<GpuColorFrameResource<GpuColorFrameWgpuResource>, String> {
        let total_started = Instant::now();
        let source_validation_started = Instant::now();
        let source =
            validated_d3d11_native_decoded_frame_for_luid(self.renderer_adapter_luid, native_frame)
                .map_err(|error| error.to_string())?;
        let key = D3D11BridgePoolKey::new(&source, plan);
        let source_validation_us = elapsed_us(source_validation_started);
        let Self {
            device,
            queue,
            yuv_decoder,
            color_runtime,
            pools,
            options,
            frame_cpu_timings,
            ..
        } = self;
        let pool = pools.entry(key).or_default();
        let bridge_acquire_started = Instant::now();
        let (entry_index, prepared_native) = acquire_or_grow_entry(
            pool,
            options.max_frames_in_flight_per_contract,
            device,
            queue,
            &source,
        )
        .map_err(|error| error.to_string())?;
        let bridge_acquire_us = elapsed_us(bridge_acquire_started);
        let entry = &mut pool[entry_index];
        let pipeline_prepare_started = Instant::now();
        let yuv_plan = match GpuNativeYuvDecodePlan::from_import_plan(
            plan,
            GpuNativeVideoExtent {
                width: source.inspection.storage_width,
                height: source.inspection.storage_height,
            },
        ) {
            Ok(plan) => plan,
            Err(error) => {
                return Err(discard_with_reason(
                    entry,
                    prepared_native,
                    error.to_string(),
                ));
            }
        };

        if entry.prepared_yuv.is_none() {
            let views = match entry.bridge.plane_views(&prepared_native) {
                Ok(views) => views,
                Err(error) => {
                    return Err(discard_with_reason(
                        entry,
                        prepared_native,
                        error.to_string(),
                    ));
                }
            };
            entry.prepared_yuv = Some(yuv_decoder.prepare_pass(
                device,
                &yuv_plan,
                GpuNativeYuvPlaneViews { luma: views.luma, chroma: views.chroma },
            ));
        }

        let encoded_payload = match entry.encoded_source.take() {
            Some(payload) => payload,
            None => {
                let (_, payload) =
                    GpuNativeYuvDecoder::allocate_output(device, &yuv_plan).into_parts();
                payload
            }
        };
        let encoded_resource =
            GpuColorFrameResource::new(plan.encoded_source_frame.clone(), encoded_payload);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.native-video.d3d11-import"),
        });
        let pipeline_prepare_us = elapsed_us(pipeline_prepare_started);

        let record_result = (|| {
            let yuv_record_started = Instant::now();
            yuv_decoder
                .record(
                    &mut encoder,
                    &yuv_plan,
                    entry
                        .prepared_yuv
                        .as_ref()
                        .ok_or_else(|| "native YUV pass was not prepared".to_owned())?,
                    &encoded_resource,
                )
                .map_err(|error| error.to_string())?;
            let yuv_record_us = elapsed_us(yuv_record_started);
            let color_stage_started = Instant::now();
            if color_runtime
                .frame_table_mut()
                .insert(encoded_resource)
                .map_err(|error| format!("encoded source table insertion failed: {error:?}"))?
                .is_some()
            {
                return Err(
                    "encoded source frame id unexpectedly replaced a live resource".to_owned(),
                );
            }
            color_runtime
                .record_wgpu_input_stage_gpu_frame_owned_backend(
                    &plan.input_transform,
                    &plan.encoded_source_frame,
                    &plan.working_frame,
                    RenderColorTransformGpuOptions {
                        output_residency: ColorFrameResidency::Gpu,
                        ..RenderColorTransformGpuOptions::default()
                    },
                    RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                        device,
                        queue,
                        encoder: &mut encoder,
                        load_op: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    },
                )
                .map_err(format_input_stage_error)?;
            let color_stage_us = elapsed_us(color_stage_started);
            let resource_extract_started = Instant::now();
            let working = color_runtime
                .frame_table_mut()
                .remove(plan.working_frame.id())
                .ok_or_else(|| "OCIO input stage did not retain its working output".to_owned())?;
            let encoded = color_runtime
                .frame_table_mut()
                .remove(plan.encoded_source_frame.id())
                .ok_or_else(|| "OCIO input stage lost its encoded source resource".to_owned())?;
            let resource_extract_us = elapsed_us(resource_extract_started);
            Ok((
                working,
                encoded.into_parts().1,
                yuv_record_us,
                color_stage_us,
                resource_extract_us,
            ))
        })();

        let (working, encoded_payload, yuv_record_us, color_stage_us, resource_extract_us) =
            match record_result {
                Ok(result) => result,
                Err(reason) => {
                    restore_encoded_source(color_runtime, entry, plan);
                    return Err(discard_with_reason(entry, prepared_native, reason));
                }
            };
        entry.encoded_source = Some(encoded_payload);
        let submit_started = Instant::now();
        entry
            .bridge
            .submit_renderer_commands(prepared_native, std::iter::once(encoder.finish()))
            .map_err(|error| error.to_string())?;
        let submit_us = elapsed_us(submit_started);
        frame_cpu_timings.accumulate(NativeVideoImportCpuTimings {
            source_validation_us,
            bridge_acquire_us,
            pipeline_prepare_us,
            yuv_record_us,
            color_stage_us,
            resource_extract_us,
            submit_us,
            total_us: elapsed_us(total_started),
        });
        Ok(working)
    }
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

impl GpuNativeDecodedFrameImportBackend for D3D11Dx12NativeVideoImportBackend {
    type NativeFrame = PreviewNativeDecodedFrame;
    type Resource = GpuColorFrameWgpuResource;

    fn support(&self) -> &GpuNativeDecodedFrameImportSupport {
        &self.support
    }

    fn import_native_decoded_frame(
        &mut self,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &Self::NativeFrame,
    ) -> Result<GpuColorFrameResource<Self::Resource>, GpuNativeDecodedFrameImportError> {
        self.import_frame(plan, native_frame)
            .map_err(|reason| GpuNativeDecodedFrameImportError::BackendRejected { reason })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct D3D11BridgePoolKey {
    source_device_identity: usize,
    visible_extent: GpuNativeVideoExtent,
    storage_extent: GpuNativeVideoExtent,
    source_texture_format: GpuNativeDecodedFrameTextureFormat,
    source_color_space: ColorSpace,
    working_color_space: mondrian_core::WorkingColorSpace,
    video_sampling: GpuNativeDecodedFrameVideoSampling,
    working_texture_format: GpuColorFrameTextureFormat,
}

impl D3D11BridgePoolKey {
    fn new(
        source: &ValidatedD3D11NativeDecodedFrame,
        plan: &GpuNativeDecodedFrameImportPlan,
    ) -> Self {
        Self {
            source_device_identity: source.device.as_raw() as usize,
            visible_extent: GpuNativeVideoExtent {
                width: source.inspection.visible_width,
                height: source.inspection.visible_height,
            },
            storage_extent: GpuNativeVideoExtent {
                width: source.inspection.storage_width,
                height: source.inspection.storage_height,
            },
            source_texture_format: source.inspection.source_texture_format,
            source_color_space: plan.source_color_space,
            working_color_space: plan.input_transform.working_color_space,
            video_sampling: plan.video_sampling,
            working_texture_format: plan.working_frame.texture_format(),
        }
    }
}

struct D3D11NativeVideoPipelineEntry {
    bridge: D3D11Dx12SharedVideoTexture,
    prepared_yuv: Option<GpuNativeYuvPreparedPass>,
    encoded_source: Option<GpuColorFrameWgpuResource>,
}

fn acquire_or_grow_entry(
    pool: &mut Vec<D3D11NativeVideoPipelineEntry>,
    max_entries: usize,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: &ValidatedD3D11NativeDecodedFrame,
) -> Result<(usize, D3D11Dx12PreparedVideoFrame), D3D11Dx12SharedVideoTextureError> {
    pool.retain(|entry| entry.bridge.sync_phase() != "poisoned");
    let mut last_busy = None;
    for (index, entry) in pool.iter_mut().enumerate() {
        match entry.bridge.begin_validated_frame(source) {
            Ok(prepared) => return Ok((index, prepared)),
            Err(error @ D3D11Dx12SharedVideoTextureError::EntryBusy { .. }) => {
                last_busy = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    if pool.len() >= max_entries {
        return Err(
            last_busy.unwrap_or(D3D11Dx12SharedVideoTextureError::SyncProtocol {
                reason: "native video bridge pool reached its configured limit".to_owned(),
            }),
        );
    }
    let mut bridge = D3D11Dx12SharedVideoTexture::new_from_validated_source(device, queue, source)?;
    let prepared = bridge.begin_validated_frame(source)?;
    pool.push(D3D11NativeVideoPipelineEntry { bridge, prepared_yuv: None, encoded_source: None });
    Ok((pool.len() - 1, prepared))
}

fn restore_encoded_source(
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    entry: &mut D3D11NativeVideoPipelineEntry,
    plan: &GpuNativeDecodedFrameImportPlan,
) {
    runtime.frame_table_mut().remove(plan.working_frame.id());
    if let Some(encoded) = runtime.frame_table_mut().remove(plan.encoded_source_frame.id()) {
        entry.encoded_source = Some(encoded.into_parts().1);
    }
}

fn discard_with_reason(
    entry: &mut D3D11NativeVideoPipelineEntry,
    prepared: D3D11Dx12PreparedVideoFrame,
    reason: String,
) -> String {
    match entry.bridge.discard_prepared_frame(prepared) {
        Ok(_) => reason,
        Err(discard_error) => {
            format!("{reason}; native bridge release also failed: {discard_error}")
        }
    }
}

fn format_input_stage_error(error: RenderGpuInputStageRuntimeRecordError) -> String {
    format!("OCIO native input stage failed: {error:?}")
}
