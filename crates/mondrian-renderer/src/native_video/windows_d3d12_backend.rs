//! Complete D3D12VA decoder-surface to OCIO working-frame backend.

use super::gpu_timing::NativeVideoImportGpuTimingRuntime;
use super::windows_adapter::{
    renderer_adapter_dxgi_index, renderer_adapter_luid, NativeVideoAdapterError,
    NativeVideoAdapterLuid,
};
use super::windows_d3d12::{
    validated_d3d12_native_decoded_frame_for_luid, ValidatedD3D12NativeDecodedFrame,
};
use super::windows_d3d12_bridge::{
    D3D12PreparedVideoFrame, D3D12SharedVideoTexture, D3D12SharedVideoTextureError,
};
use crate::{
    ColorFrameResidency, GpuColorFrameIdAllocationError, GpuColorFrameResource,
    GpuColorFrameTextureFormat, GpuColorFrameWgpuResource, GpuColorFrameWgpuResourcePool,
    GpuNativeDecodedFrameImportBackend, GpuNativeDecodedFrameImportError,
    GpuNativeDecodedFrameImportPlan, GpuNativeDecodedFrameImportSupport,
    GpuNativeDecodedFrameTextureFormat, GpuNativeDecodedFrameVideoSampling, GpuNativeVideoExtent,
    GpuNativeYuvDecodePlan, GpuNativeYuvDecoder, GpuNativeYuvPlaneViews, GpuNativeYuvPreparedPass,
    NativeVideoImportCandidateTimingReceipt, NativeVideoImportCandidateToken,
    NativeVideoImportCpuTimings, NativeVideoImportGpuTimingDiagnostics,
    NativeVideoImportGpuTimingPolicy, NativeVideoImportGpuTimingSample,
    RenderColorTransformGpuOptions, RenderGpuInputStageRuntimeRecordError,
    RenderGpuOutputBoundaryRuntime, RenderGpuOutputBoundaryRuntimeOwnedBackendContext,
};
use mondrian_core::types::ColorSpace;
use mondrian_media::{DecodedGpuFrameHandleKind, PreviewNativeDecodedFrame};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use windows::core::Interface;

/// Error creating the complete Windows native decoded-frame renderer backend.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum D3D12NativeVideoImportBackendCreateError {
    /// The active renderer adapter cannot provide a stable DX12 LUID contract.
    #[error(transparent)]
    RendererAdapter(#[from] NativeVideoAdapterError),
    /// The wgpu device enabled neither native two-plane format.
    #[error("wgpu device enabled neither TEXTURE_FORMAT_NV12 nor TEXTURE_FORMAT_P010")]
    NoNativeYuvTextureFormats,
    /// A zero-sized in-flight pool could never import a frame.
    #[error("native video bridge pool limit must be greater than zero")]
    ZeroBridgePoolLimit,
    /// A zero-sized contract-pool limit could never retain a decoder session.
    #[error("native video contract pool limit must be greater than zero")]
    ZeroContractPoolLimit,
    /// Renderer frame identity allocation is exhausted.
    #[error(transparent)]
    FrameId(#[from] GpuColorFrameIdAllocationError),
}

/// Resource-pool policy for the Windows native video import backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct D3D12NativeVideoImportBackendOptions {
    /// Maximum bridge entries for one immutable source/sampling contract.
    /// Reaching the limit returns busy without a CPU wait.
    pub max_frames_in_flight_per_contract: usize,
    /// Maximum decoder-device/source contract pools retained by the renderer.
    /// Completed least-recently-used pools are evicted without a GPU wait.
    pub max_contract_pools: usize,
}

impl Default for D3D12NativeVideoImportBackendOptions {
    fn default() -> Self {
        Self {
            max_frames_in_flight_per_contract: 4,
            max_contract_pools: 8,
        }
    }
}

/// Complete low-copy D3D12VA decoder-surface import backend.
///
/// The backend pools bridge entries per source device and immutable video
/// sampling contract. Each entry reuses its shared native texture, timeline
/// fence, plane bind group, and encoded RGB intermediate. Entries expand only
/// when every matching entry is still in flight; CPU waits are never used for
/// normal playback concurrency.
pub struct D3D12NativeVideoImportBackend {
    renderer_adapter_luid: NativeVideoAdapterLuid,
    device: wgpu::Device,
    queue: wgpu::Queue,
    support: GpuNativeDecodedFrameImportSupport,
    yuv_decoder: GpuNativeYuvDecoder,
    color_runtime: RenderGpuOutputBoundaryRuntime,
    pools: HashMap<D3D12BridgePoolKey, D3D12NativeVideoPipelinePool>,
    options: D3D12NativeVideoImportBackendOptions,
    contract_use_sequence: u64,
    frame_cpu_timings: NativeVideoImportCpuTimings,
    gpu_timing: NativeVideoImportGpuTimingRuntime,
}

impl D3D12NativeVideoImportBackend {
    /// Create a backend bound to one wgpu DX12 device/queue.
    pub fn new(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Self, D3D12NativeVideoImportBackendCreateError> {
        Self::new_with_gpu_timing_policy(
            adapter,
            device,
            queue,
            NativeVideoImportGpuTimingPolicy::default(),
        )
    }

    /// Create a backend with an explicit native-import timing activation policy.
    pub fn new_with_gpu_timing_policy(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        gpu_timing_policy: NativeVideoImportGpuTimingPolicy,
    ) -> Result<Self, D3D12NativeVideoImportBackendCreateError> {
        Self::new_with_options_and_resource_pool_and_gpu_timing_policy(
            adapter,
            device,
            queue,
            D3D12NativeVideoImportBackendOptions::default(),
            Arc::new(GpuColorFrameWgpuResourcePool::default()),
            gpu_timing_policy,
        )
    }

    /// Create a backend with an explicit bounded in-flight pool policy.
    pub fn new_with_options(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        options: D3D12NativeVideoImportBackendOptions,
    ) -> Result<Self, D3D12NativeVideoImportBackendCreateError> {
        Self::new_with_options_and_resource_pool_and_gpu_timing_policy(
            adapter,
            device,
            queue,
            options,
            Arc::new(GpuColorFrameWgpuResourcePool::default()),
            NativeVideoImportGpuTimingPolicy::default(),
        )
    }

    /// Create a backend that shares renderer color-frame resources across stages.
    pub fn new_with_resource_pool(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    ) -> Result<Self, D3D12NativeVideoImportBackendCreateError> {
        Self::new_with_resource_pool_and_gpu_timing_policy(
            adapter,
            device,
            queue,
            resource_pool,
            NativeVideoImportGpuTimingPolicy::default(),
        )
    }

    /// Create a shared-resource backend with an explicit timing activation policy.
    pub fn new_with_resource_pool_and_gpu_timing_policy(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
        gpu_timing_policy: NativeVideoImportGpuTimingPolicy,
    ) -> Result<Self, D3D12NativeVideoImportBackendCreateError> {
        Self::new_with_options_and_resource_pool_and_gpu_timing_policy(
            adapter,
            device,
            queue,
            D3D12NativeVideoImportBackendOptions::default(),
            resource_pool,
            gpu_timing_policy,
        )
    }

    /// Create a backend with explicit bridge and shared-resource policies.
    pub fn new_with_options_and_resource_pool(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        options: D3D12NativeVideoImportBackendOptions,
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    ) -> Result<Self, D3D12NativeVideoImportBackendCreateError> {
        Self::new_with_options_and_resource_pool_and_gpu_timing_policy(
            adapter,
            device,
            queue,
            options,
            resource_pool,
            NativeVideoImportGpuTimingPolicy::default(),
        )
    }

    /// Create a backend with explicit bridge, resource, and timing policies.
    pub fn new_with_options_and_resource_pool_and_gpu_timing_policy(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        options: D3D12NativeVideoImportBackendOptions,
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
        gpu_timing_policy: NativeVideoImportGpuTimingPolicy,
    ) -> Result<Self, D3D12NativeVideoImportBackendCreateError> {
        if options.max_frames_in_flight_per_contract == 0 {
            return Err(D3D12NativeVideoImportBackendCreateError::ZeroBridgePoolLimit);
        }
        if options.max_contract_pools == 0 {
            return Err(D3D12NativeVideoImportBackendCreateError::ZeroContractPoolLimit);
        }
        let formats = conformed_decoder_surface_formats(device.features())?;
        let renderer_adapter_luid = renderer_adapter_luid(adapter)?;
        let decoder_adapter_index = renderer_adapter_dxgi_index(adapter)?;
        let support = GpuNativeDecodedFrameImportSupport::ready_gpu_bridge_copy(
            vec![DecodedGpuFrameHandleKind::D3D12Resource],
            formats,
        )
        .with_hardware_decode_device_selector(
            mondrian_media::HwAccelDeviceSelector::D3D12VaAdapterIndex(decoder_adapter_index),
        )
        .with_renderer_backend_label("wgpu Dx12 D3D12VA shared YUV + OCIO");
        Ok(Self {
            renderer_adapter_luid,
            device: device.clone(),
            queue: queue.clone(),
            support,
            yuv_decoder: GpuNativeYuvDecoder::new(device),
            color_runtime: RenderGpuOutputBoundaryRuntime::with_resource_pool(resource_pool)?,
            pools: HashMap::new(),
            options,
            contract_use_sequence: 0,
            frame_cpu_timings: NativeVideoImportCpuTimings::default(),
            gpu_timing: NativeVideoImportGpuTimingRuntime::new(device, queue, gpu_timing_policy),
        })
    }

    /// Return the number of reusable bridge entries currently allocated.
    pub fn bridge_entry_count(&self) -> usize {
        self.pools.values().map(|pool| pool.entries.len()).sum()
    }

    /// Return the number of decoder-device/source contracts currently retained.
    pub fn contract_pool_count(&self) -> usize {
        self.pools.len()
    }

    /// Decoder surfaces still retained only until their bridge copy completes.
    pub fn retained_source_count(&self) -> usize {
        self.pools
            .values()
            .flat_map(|pool| &pool.entries)
            .filter(|entry| entry.bridge.has_retained_source())
            .count()
    }

    /// Non-blockingly retire decoder surfaces whose bridge-copy fence completed.
    pub fn retire_completed_source_residency(
        &mut self,
    ) -> Result<usize, GpuNativeDecodedFrameImportError> {
        let mut retired = 0usize;
        for entry in self.pools.values_mut().flat_map(|pool| &mut pool.entries) {
            if entry.bridge.retire_completed_source().map_err(native_bridge_import_error)? {
                retired = retired.saturating_add(1);
            }
        }
        Ok(retired)
    }

    /// Borrow the OCIO runtime used by the native input path.
    pub fn color_runtime(&self) -> &RenderGpuOutputBoundaryRuntime {
        &self.color_runtime
    }

    /// Begin one explicit Viewer-candidate timing/CPU-attribution scope.
    pub fn begin_viewer_candidate(&mut self) -> Option<NativeVideoImportCandidateToken> {
        self.frame_cpu_timings = NativeVideoImportCpuTimings::default();
        self.gpu_timing.begin_candidate()
    }

    /// Return accumulated native-import CPU attribution for the current candidate.
    pub fn frame_cpu_timings(&self) -> NativeVideoImportCpuTimings {
        self.frame_cpu_timings
    }

    /// End the exact Viewer-candidate scope, including failed recordings.
    ///
    /// Active timing returns a move-only receipt only for a successful record.
    pub fn end_viewer_candidate(
        &mut self,
        candidate: Option<NativeVideoImportCandidateToken>,
        viewer_record_succeeded: bool,
    ) -> Option<NativeVideoImportCandidateTimingReceipt> {
        self.gpu_timing.end_candidate(candidate, viewer_record_succeeded)
    }

    /// Collect callbacks after the execution owner has already polled the device.
    pub fn collect_gpu_timings_after_device_poll(&mut self) {
        self.gpu_timing.collect_after_device_poll();
    }

    /// Drain asynchronously completed native-import GPU timing samples.
    pub fn take_completed_gpu_timings(&mut self) -> Vec<NativeVideoImportGpuTimingSample> {
        self.gpu_timing.take_completed()
    }

    /// Return cumulative native-import GPU timing coverage and health.
    pub fn gpu_timing_diagnostics(&self) -> NativeVideoImportGpuTimingDiagnostics {
        self.gpu_timing.diagnostics()
    }

    fn import_frame(
        &mut self,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &PreviewNativeDecodedFrame,
    ) -> Result<GpuColorFrameResource<GpuColorFrameWgpuResource>, GpuNativeDecodedFrameImportError>
    {
        let total_started = Instant::now();
        let source_validation_started = Instant::now();
        let source =
            validated_d3d12_native_decoded_frame_for_luid(self.renderer_adapter_luid, native_frame)
                .map_err(|error| backend_rejected(error.to_string()))?;
        let key = D3D12BridgePoolKey::new(&source, plan);
        let source_validation_us = elapsed_us(source_validation_started);
        self.ensure_contract_pool(key)?;
        let Self {
            device,
            queue,
            yuv_decoder,
            color_runtime,
            pools,
            options,
            contract_use_sequence,
            frame_cpu_timings,
            gpu_timing,
            ..
        } = self;
        *contract_use_sequence = contract_use_sequence.saturating_add(1);
        let pool = pools.get_mut(&key).ok_or_else(|| {
            backend_rejected("native video contract pool disappeared after admission".to_owned())
        })?;
        pool.last_used_sequence = *contract_use_sequence;
        let bridge_acquire_started = Instant::now();
        let (entry_index, prepared_native) = acquire_or_grow_entry(
            &mut pool.entries,
            options.max_frames_in_flight_per_contract,
            device,
            queue,
            &source,
        )
        .map_err(native_bridge_import_error)?;
        let bridge_acquire_us = elapsed_us(bridge_acquire_started);
        let entry = pool.entries.get_mut(entry_index).ok_or_else(|| {
            backend_rejected("native video bridge acquisition returned an invalid index".to_owned())
        })?;
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
                return Err(backend_rejected(discard_with_reason(
                    entry,
                    prepared_native,
                    error.to_string(),
                )));
            }
        };

        if entry.prepared_yuv.is_none() {
            let views = match entry.bridge.plane_views(&prepared_native) {
                Ok(views) => views,
                Err(error) => {
                    return Err(backend_rejected(discard_with_reason(
                        entry,
                        prepared_native,
                        error.to_string(),
                    )));
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
            label: Some("mondrian.native-video.d3d12-import"),
        });
        let mut gpu_timing_probe = gpu_timing.begin_import(
            &mut encoder,
            prepared_native.decode_fence_ready_at_admission(),
        );
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
            gpu_timing.mark_after_yuv(&mut encoder, &mut gpu_timing_probe);
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
            gpu_timing.mark_after_input_color(&mut encoder, &mut gpu_timing_probe);
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
                    gpu_timing.abandon_before_submit(gpu_timing_probe);
                    restore_encoded_source(color_runtime, entry, plan);
                    return Err(backend_rejected(discard_with_reason(
                        entry,
                        prepared_native,
                        reason,
                    )));
                }
            };
        entry.encoded_source = Some(encoded_payload);
        gpu_timing.finish_recording(&mut encoder, &mut gpu_timing_probe);
        let submit_started = Instant::now();
        if let Err(error) = entry
            .bridge
            .submit_renderer_commands(prepared_native, std::iter::once(encoder.finish()))
        {
            gpu_timing.submission_failed_after_queue(gpu_timing_probe, error.to_string());
            return Err(native_bridge_import_error(error));
        }
        gpu_timing.after_submit(gpu_timing_probe);
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

    fn ensure_contract_pool(
        &mut self,
        key: D3D12BridgePoolKey,
    ) -> Result<(), GpuNativeDecodedFrameImportError> {
        if self.pools.contains_key(&key) {
            return Ok(());
        }
        if self.pools.len() >= self.options.max_contract_pools {
            let mut candidates = Vec::with_capacity(self.pools.len());
            for (candidate_key, pool) in &self.pools {
                let mut reclaimable = true;
                for entry in &pool.entries {
                    if !entry
                        .bridge
                        .renderer_work_completed()
                        .map_err(native_bridge_import_error)?
                    {
                        reclaimable = false;
                        break;
                    }
                }
                candidates.push((*candidate_key, pool.last_used_sequence, reclaimable));
            }
            let Some(reclaim_key) = select_oldest_reclaimable_contract(candidates) else {
                return Err(GpuNativeDecodedFrameImportError::Backpressure {
                    reason: format!(
                        "all {} native video contract pools are still in flight",
                        self.options.max_contract_pools
                    ),
                });
            };
            self.pools.remove(&reclaim_key);
        }
        self.pools.insert(
            key,
            D3D12NativeVideoPipelinePool {
                entries: Vec::new(),
                last_used_sequence: self.contract_use_sequence,
            },
        );
        Ok(())
    }
}

/// Return native formats enabled on the concrete wgpu DX12 device.
///
/// D3D12VA admission still validates every real resource descriptor, adapter,
/// and fence before it allocates or submits a bridge entry.
fn conformed_decoder_surface_formats(
    features: wgpu::Features,
) -> Result<Vec<GpuNativeDecodedFrameTextureFormat>, D3D12NativeVideoImportBackendCreateError> {
    let mut formats = Vec::with_capacity(2);
    if features.contains(wgpu::Features::TEXTURE_FORMAT_NV12) {
        formats.push(GpuNativeDecodedFrameTextureFormat::Nv12);
    }
    if features
        .contains(wgpu::Features::TEXTURE_FORMAT_P010 | wgpu::Features::TEXTURE_FORMAT_16BIT_NORM)
    {
        formats.push(GpuNativeDecodedFrameTextureFormat::P010);
    }
    if formats.is_empty() {
        return Err(D3D12NativeVideoImportBackendCreateError::NoNativeYuvTextureFormats);
    }
    Ok(formats)
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

impl GpuNativeDecodedFrameImportBackend for D3D12NativeVideoImportBackend {
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
    }
}

fn native_bridge_import_error(
    error: D3D12SharedVideoTextureError,
) -> GpuNativeDecodedFrameImportError {
    match error {
        D3D12SharedVideoTextureError::EntryBusy { required, completed } => {
            GpuNativeDecodedFrameImportError::Backpressure {
                reason: format!(
                    "shared native video texture is busy until fence {required}, completed {completed}"
                ),
            }
        }
        D3D12SharedVideoTextureError::DeviceRemoved => {
            GpuNativeDecodedFrameImportError::NativeDeviceRemoved {
                reason: "D3D shared copy fence reported the device-removed sentinel".to_owned(),
            }
        }
        error => backend_rejected(error.to_string()),
    }
}

fn backend_rejected(reason: String) -> GpuNativeDecodedFrameImportError {
    GpuNativeDecodedFrameImportError::BackendRejected { reason }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct D3D12BridgePoolKey {
    source_device_identity: usize,
    source_visible_extent: GpuNativeVideoExtent,
    materialization_extent: GpuNativeVideoExtent,
    storage_extent: GpuNativeVideoExtent,
    source_texture_format: GpuNativeDecodedFrameTextureFormat,
    source_color_space: ColorSpace,
    working_color_space: mondrian_core::WorkingColorSpace,
    video_sampling: GpuNativeDecodedFrameVideoSampling,
    working_texture_format: GpuColorFrameTextureFormat,
}

impl D3D12BridgePoolKey {
    fn new(
        source: &ValidatedD3D12NativeDecodedFrame,
        plan: &GpuNativeDecodedFrameImportPlan,
    ) -> Self {
        Self {
            source_device_identity: source.device.as_raw() as usize,
            source_visible_extent: GpuNativeVideoExtent {
                width: source.inspection.visible_width,
                height: source.inspection.visible_height,
            },
            materialization_extent: GpuNativeVideoExtent {
                width: plan.working_frame.descriptor().width,
                height: plan.working_frame.descriptor().height,
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

struct D3D12NativeVideoPipelineEntry {
    bridge: D3D12SharedVideoTexture,
    prepared_yuv: Option<GpuNativeYuvPreparedPass>,
    encoded_source: Option<GpuColorFrameWgpuResource>,
}

struct D3D12NativeVideoPipelinePool {
    entries: Vec<D3D12NativeVideoPipelineEntry>,
    last_used_sequence: u64,
}

fn select_oldest_reclaimable_contract<K: Copy>(
    candidates: impl IntoIterator<Item = (K, u64, bool)>,
) -> Option<K> {
    candidates
        .into_iter()
        .filter(|(_, _, reclaimable)| *reclaimable)
        .min_by_key(|(_, last_used_sequence, _)| *last_used_sequence)
        .map(|(key, _, _)| key)
}

fn acquire_or_grow_entry(
    pool: &mut Vec<D3D12NativeVideoPipelineEntry>,
    max_entries: usize,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: &ValidatedD3D12NativeDecodedFrame,
) -> Result<(usize, D3D12PreparedVideoFrame), D3D12SharedVideoTextureError> {
    let mut last_busy = None;
    for (index, entry) in pool.iter_mut().enumerate() {
        if !bridge_sync_phase_is_eligible(entry.bridge.sync_phase()) {
            continue;
        }
        match entry.bridge.begin_validated_frame(source) {
            Ok(prepared) => return Ok((index, prepared)),
            Err(error @ D3D12SharedVideoTextureError::EntryBusy { .. }) => {
                last_busy = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    if pool.len() >= max_entries {
        return Err(
            last_busy.unwrap_or(D3D12SharedVideoTextureError::SyncProtocol {
                reason: "native video bridge pool reached its configured limit".to_owned(),
            }),
        );
    }
    let mut bridge = D3D12SharedVideoTexture::new_from_validated_source(device, queue, source)?;
    let prepared = bridge.begin_validated_frame(source)?;
    pool.push(D3D12NativeVideoPipelineEntry { bridge, prepared_yuv: None, encoded_source: None });
    Ok((pool.len() - 1, prepared))
}

fn bridge_sync_phase_is_eligible(sync_phase: &str) -> bool {
    sync_phase != "poisoned"
}

fn restore_encoded_source(
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    entry: &mut D3D12NativeVideoPipelineEntry,
    plan: &GpuNativeDecodedFrameImportPlan,
) {
    runtime.frame_table_mut().remove(plan.working_frame.id());
    if let Some(encoded) = runtime.frame_table_mut().remove(plan.encoded_source_frame.id()) {
        entry.encoded_source = Some(encoded.into_parts().1);
    }
}

fn discard_with_reason(
    entry: &mut D3D12NativeVideoPipelineEntry,
    prepared: D3D12PreparedVideoFrame,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_bridge_pool_is_retryable_backpressure() {
        let error = native_bridge_import_error(D3D12SharedVideoTextureError::EntryBusy {
            required: 8,
            completed: 6,
        });

        assert!(error.is_backpressure());
        assert!(error.to_string().contains("fence 8, completed 6"));
    }

    #[test]
    fn bridge_protocol_failure_remains_terminal() {
        let error = native_bridge_import_error(D3D12SharedVideoTextureError::SyncProtocol {
            reason: "foreign frame token".to_owned(),
        });

        assert!(!error.is_backpressure());
        assert!(matches!(
            error,
            GpuNativeDecodedFrameImportError::BackendRejected { .. }
        ));
    }

    #[test]
    fn device_removed_remains_typed_retirement_proof() {
        let error = native_bridge_import_error(D3D12SharedVideoTextureError::DeviceRemoved);

        assert!(error.is_native_device_removed());
        assert!(matches!(
            error,
            GpuNativeDecodedFrameImportError::NativeDeviceRemoved { .. }
        ));
    }

    #[test]
    fn poisoned_bridge_entry_does_not_block_other_pool_entries() {
        assert!(!bridge_sync_phase_is_eligible("poisoned"));
        assert!(bridge_sync_phase_is_eligible("available"));
        assert!(bridge_sync_phase_is_eligible("renderer_in_flight"));
    }

    #[test]
    fn contract_pool_eviction_selects_oldest_completed_candidate() {
        assert_eq!(
            select_oldest_reclaimable_contract([
                ("recent", 30, true),
                ("busy-oldest", 1, false),
                ("oldest-complete", 10, true),
            ]),
            Some("oldest-complete")
        );
        assert_eq!(
            select_oldest_reclaimable_contract([("busy-a", 1, false), ("busy-b", 2, false)]),
            None
        );
    }

    #[test]
    fn default_native_pool_policy_is_bounded_in_both_dimensions() {
        let options = D3D12NativeVideoImportBackendOptions::default();

        assert_eq!(options.max_frames_in_flight_per_contract, 4);
        assert_eq!(options.max_contract_pools, 8);
    }

    #[test]
    fn d3d12_native_formats_require_complete_device_feature_contracts() {
        assert_eq!(
            conformed_decoder_surface_formats(wgpu::Features::TEXTURE_FORMAT_NV12)
                .expect("NV12 device feature"),
            vec![GpuNativeDecodedFrameTextureFormat::Nv12]
        );
        assert_eq!(
            conformed_decoder_surface_formats(wgpu::Features::TEXTURE_FORMAT_P010)
                .expect_err("P010 also requires 16-bit normalized texture support"),
            D3D12NativeVideoImportBackendCreateError::NoNativeYuvTextureFormats
        );
        assert_eq!(
            conformed_decoder_surface_formats(
                wgpu::Features::TEXTURE_FORMAT_P010 | wgpu::Features::TEXTURE_FORMAT_16BIT_NORM,
            )
            .expect("complete P010 device feature contract"),
            vec![GpuNativeDecodedFrameTextureFormat::P010]
        );
        assert_eq!(
            conformed_decoder_surface_formats(wgpu::Features::empty())
                .expect_err("missing formats must remain distinguishable"),
            D3D12NativeVideoImportBackendCreateError::NoNativeYuvTextureFormats
        );
    }
}
