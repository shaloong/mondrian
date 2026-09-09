//! Same-device D3D12VA decoder-surface to OCIO working-frame backend.
//!
//! The active wgpu D3D12 device is installed into FFmpeg before codec Session
//! creation. Decoder textures therefore belong to the exact renderer device:
//! this Adapter waits on FFmpeg's decode fence, adopts the texture into wgpu,
//! samples its Y/UV planes, restores COMMON, and retains the Media lease until
//! a renderer fence proves all reads complete. No bridge texture or pixel copy
//! exists in this path.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use mondrian_media::{
    DecodedGpuFrameHandleKind, PreviewNativeDecodedFrame, PreviewNativeDecodedFrameHandle,
    RendererHwAccelDeviceContext,
};
use windows::core::{w, Interface};
use windows::Win32::Graphics::Direct3D12::{
    ID3D12CommandQueue, ID3D12Device, ID3D12Fence, ID3D12Resource, D3D12_FENCE_FLAG_NONE,
    D3D12_RESOURCE_STATE_COMMON,
};

use super::gpu_timing::NativeVideoImportGpuTimingRuntime;
use super::windows_adapter::{
    renderer_adapter_dxgi_index, renderer_adapter_luid, NativeVideoAdapterError,
    NativeVideoAdapterLuid,
};
use super::windows_d3d12::validated_d3d12_native_decoded_frame_for_luid;
use super::windows_d3d12_texture::{
    adopt_wgpu_video_texture, validate_device_feature, validate_device_limits,
    D3D12NativeTextureError, D3D12TransitionCommands, WGPU_RESOURCE_STATE,
};
use crate::{
    ColorFrameResidency, GpuColorFrameIdAllocationError, GpuColorFrameResource,
    GpuColorFrameWgpuResource, GpuColorFrameWgpuResourcePool, GpuNativeDecodedFrameImportBackend,
    GpuNativeDecodedFrameImportError, GpuNativeDecodedFrameImportPlan,
    GpuNativeDecodedFrameImportSupport, GpuNativeDecodedFrameTextureFormat, GpuNativeVideoExtent,
    GpuNativeYuvDecodePlan, GpuNativeYuvDecoder, GpuNativeYuvPlaneViews,
    NativeVideoImportCandidateTimingReceipt, NativeVideoImportCandidateToken,
    NativeVideoImportCpuTimings, NativeVideoImportGpuTimingDiagnostics,
    NativeVideoImportGpuTimingPolicy, NativeVideoImportGpuTimingSample,
    RenderColorTransformGpuOptions, RenderGpuInputStageRuntimeRecordError,
    RenderGpuOutputBoundaryRuntime, RenderGpuOutputBoundaryRuntimeOwnedBackendContext,
};

/// Error creating the Windows same-device native decoded-frame backend.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum D3D12NativeVideoImportBackendCreateError {
    /// The active renderer adapter/device contract could not be resolved.
    #[error(transparent)]
    RendererAdapter(#[from] NativeVideoAdapterError),
    /// The active wgpu objects are not D3D12 objects.
    #[error("native D3D12VA import requires a wgpu DX12 {object}")]
    WrongBackend {
        /// Missing native object.
        object: &'static str,
    },
    /// The device enabled no supported native video format.
    #[error("wgpu device enabled neither complete NV12 nor P010 native texture support")]
    NoNativeYuvTextureFormats,
    /// A zero in-flight limit cannot preserve source ownership.
    #[error("native D3D12VA in-flight source limit must be greater than zero")]
    ZeroInFlightLimit,
    /// A zero contract-pool limit is not a valid compatibility policy.
    #[error("native D3D12VA contract-pool limit must be greater than zero")]
    ZeroContractPoolLimit,
    /// Renderer frame identity allocation is exhausted.
    #[error(transparent)]
    FrameId(#[from] GpuColorFrameIdAllocationError),
    /// The shared color runtime could not be created.
    #[error("could not create native D3D12VA color runtime: {reason}")]
    ColorRuntime {
        /// Concrete renderer failure.
        reason: String,
    },
    /// FFmpeg could not adopt the exact renderer device.
    #[error("could not create renderer-qualified FFmpeg D3D12VA device root: {reason}")]
    DecoderDeviceRoot {
        /// Concrete Media Adapter failure.
        reason: String,
    },
    /// The renderer could not allocate its completion fence.
    #[error("could not create native D3D12VA renderer completion fence: {reason}")]
    CompletionFence {
        /// Concrete D3D12 failure.
        reason: String,
    },
}

/// Bounded source-residency policy for same-device native import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct D3D12NativeVideoImportBackendOptions {
    /// Maximum decoder surfaces retained until renderer completion.
    pub max_frames_in_flight_per_contract: usize,
    /// Compatibility field retained for callers of the former bridge backend.
    /// Same-device execution has one renderer/decoder device contract.
    pub max_contract_pools: usize,
}

impl Default for D3D12NativeVideoImportBackendOptions {
    fn default() -> Self {
        Self {
            max_frames_in_flight_per_contract: 4,
            max_contract_pools: 1,
        }
    }
}

struct DirectSubmissionResidency {
    completion_value: u64,
    // Keep an explicit COM lease for the exact resource referenced by the
    // acquire, wgpu, and release command lists. The retained AVFrame below
    // protects decoder ownership, but is not the renderer queue's physical
    // resource-lifetime authority.
    _texture: ID3D12Resource,
    _source: PreviewNativeDecodedFrameHandle,
    _acquire_commands: D3D12TransitionCommands,
    _release_commands: D3D12TransitionCommands,
}

struct WgpuSubmissionResidency {
    completion_value: u64,
    wgpu_complete: Arc<AtomicBool>,
    texture: ID3D12Resource,
    source: PreviewNativeDecodedFrameHandle,
    acquire_commands: D3D12TransitionCommands,
    release_commands: D3D12TransitionCommands,
}

struct PoisonedSubmissionResidency {
    _texture: ID3D12Resource,
    _source: PreviewNativeDecodedFrameHandle,
    _acquire_commands: D3D12TransitionCommands,
    _release_commands: D3D12TransitionCommands,
}

/// Complete zero-copy D3D12VA decoder-surface import backend.
pub struct D3D12NativeVideoImportBackend {
    renderer_adapter_luid: NativeVideoAdapterLuid,
    device: wgpu::Device,
    queue: wgpu::Queue,
    raw_device: ID3D12Device,
    raw_queue: ID3D12CommandQueue,
    support: GpuNativeDecodedFrameImportSupport,
    decoder_device_root: RendererHwAccelDeviceContext,
    yuv_decoder: GpuNativeYuvDecoder,
    color_runtime: RenderGpuOutputBoundaryRuntime,
    completion_fence: ID3D12Fence,
    next_completion_value: u64,
    pending_wgpu_sources: Vec<WgpuSubmissionResidency>,
    pending_sources: Vec<DirectSubmissionResidency>,
    poisoned_sources: Vec<PoisonedSubmissionResidency>,
    max_frames_in_flight: usize,
    frame_cpu_timings: NativeVideoImportCpuTimings,
    gpu_timing: NativeVideoImportGpuTimingRuntime,
}

impl Drop for D3D12NativeVideoImportBackend {
    fn drop(&mut self) {
        // These source leases can be the last owners of FFmpeg D3D12VA
        // AVFrames. Release them while the renderer-qualified FFmpeg device
        // root and every wgpu/raw D3D12 device/queue lease are still alive.
        // Poisoned submissions stay quarantined for the backend lifetime, but
        // generation teardown is their final safe release boundary.
        self.pending_wgpu_sources.clear();
        self.pending_sources.clear();
        self.poisoned_sources.clear();
    }
}

impl D3D12NativeVideoImportBackend {
    /// Create a backend bound to one concrete wgpu DX12 device and queue.
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

    /// Create a backend with an explicit native-import timing policy.
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

    /// Create a backend with an explicit bounded in-flight policy.
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

    /// Create a backend sharing renderer color-frame resources.
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

    /// Create a shared-resource backend with explicit timing activation.
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

    /// Create a backend with explicit source-residency and resource policies.
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

    /// Create a backend with complete source-residency, resource, and timing policy.
    pub fn new_with_options_and_resource_pool_and_gpu_timing_policy(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        options: D3D12NativeVideoImportBackendOptions,
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
        gpu_timing_policy: NativeVideoImportGpuTimingPolicy,
    ) -> Result<Self, D3D12NativeVideoImportBackendCreateError> {
        if options.max_frames_in_flight_per_contract == 0 {
            return Err(D3D12NativeVideoImportBackendCreateError::ZeroInFlightLimit);
        }
        if options.max_contract_pools == 0 {
            return Err(D3D12NativeVideoImportBackendCreateError::ZeroContractPoolLimit);
        }
        let formats = conformed_decoder_surface_formats(device.features())?;
        let renderer_adapter_luid = renderer_adapter_luid(adapter)?;
        let decoder_adapter_index = renderer_adapter_dxgi_index(adapter)?;
        let hal_device = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }
            .ok_or(D3D12NativeVideoImportBackendCreateError::WrongBackend { object: "device" })?;
        let hal_queue = unsafe { queue.as_hal::<wgpu::hal::api::Dx12>() }
            .ok_or(D3D12NativeVideoImportBackendCreateError::WrongBackend { object: "queue" })?;
        let raw_device = hal_device.raw_device().clone();
        let raw_queue = hal_queue.as_raw().clone();
        drop(hal_queue);
        drop(hal_device);
        let decoder_device_root =
            RendererHwAccelDeviceContext::from_d3d12_device(raw_device.clone()).map_err(
                |error| D3D12NativeVideoImportBackendCreateError::DecoderDeviceRoot {
                    reason: error.to_string(),
                },
            )?;
        let completion_fence: ID3D12Fence =
            unsafe { raw_device.CreateFence(0, D3D12_FENCE_FLAG_NONE) }.map_err(|error| {
                D3D12NativeVideoImportBackendCreateError::CompletionFence {
                    reason: error.to_string(),
                }
            })?;
        let _ = unsafe { raw_queue.SetName(w!("mondrian.renderer.native-video.direct")) };
        let _ = unsafe { completion_fence.SetName(w!("mondrian.renderer.native-video.fence")) };
        let color_runtime =
            RenderGpuOutputBoundaryRuntime::with_resource_pool(resource_pool).map_err(|error| {
                D3D12NativeVideoImportBackendCreateError::ColorRuntime { reason: error.to_string() }
            })?;
        let support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
            vec![DecodedGpuFrameHandleKind::D3D12Resource],
            formats,
        )
        .with_hardware_decode_device_selector(
            mondrian_media::HwAccelDeviceSelector::D3D12VaAdapterIndex(decoder_adapter_index),
        )
        .with_renderer_backend_label("wgpu Dx12 same-device D3D12VA + OCIO");
        Ok(Self {
            renderer_adapter_luid,
            device: device.clone(),
            queue: queue.clone(),
            raw_device,
            raw_queue,
            support,
            decoder_device_root,
            yuv_decoder: GpuNativeYuvDecoder::new(device),
            color_runtime,
            completion_fence,
            next_completion_value: 1,
            pending_wgpu_sources: Vec::new(),
            pending_sources: Vec::new(),
            poisoned_sources: Vec::new(),
            max_frames_in_flight: options.max_frames_in_flight_per_contract,
            frame_cpu_timings: NativeVideoImportCpuTimings::default(),
            gpu_timing: NativeVideoImportGpuTimingRuntime::new(device, queue, gpu_timing_policy),
        })
    }

    /// Exact FFmpeg device root that must be installed into Preview workers.
    pub fn decoder_device_root(&self) -> RendererHwAccelDeviceContext {
        self.decoder_device_root.clone()
    }

    /// Actual native import capability for this device generation.
    pub fn support(&self) -> &GpuNativeDecodedFrameImportSupport {
        &self.support
    }

    /// Begin one explicit Viewer-candidate attribution scope.
    pub fn begin_viewer_candidate(&mut self) -> Option<NativeVideoImportCandidateToken> {
        self.frame_cpu_timings = NativeVideoImportCpuTimings::default();
        self.gpu_timing.begin_candidate()
    }

    /// End the exact Viewer-candidate scope.
    pub fn end_viewer_candidate(
        &mut self,
        candidate: Option<NativeVideoImportCandidateToken>,
        succeeded: bool,
    ) -> Option<NativeVideoImportCandidateTimingReceipt> {
        self.gpu_timing.end_candidate(candidate, succeeded)
    }

    /// Latest host-side stage attribution.
    pub fn frame_cpu_timings(&self) -> NativeVideoImportCpuTimings {
        self.frame_cpu_timings
    }

    /// Materialize contract-specific OCIO backend objects before the native
    /// decoder surface reaches its exact presentation opportunity.
    pub(crate) fn prepare_import_plan(
        &mut self,
        plan: &GpuNativeDecodedFrameImportPlan,
    ) -> Result<(), GpuNativeDecodedFrameImportError> {
        self.color_runtime
            .prepare_wgpu_input_stage_gpu_frame_backend_objects(
                &plan.input_transform,
                &plan.encoded_source_frame,
                &plan.working_frame,
                RenderColorTransformGpuOptions {
                    output_residency: ColorFrameResidency::Gpu,
                    ..RenderColorTransformGpuOptions::default()
                },
                &self.device,
                &self.queue,
            )
            .map_err(|error| {
                rejected(format!(
                    "source-to-working color backend preparation failed: {error:?}"
                ))
            })
    }

    /// Collect timestamp callbacks after device polling.
    pub fn collect_gpu_timings_after_device_poll(&mut self) {
        self.gpu_timing.collect_after_device_poll();
    }

    /// Drain completed native-import hardware timestamp samples.
    pub fn take_completed_gpu_timings(&mut self) -> Vec<NativeVideoImportGpuTimingSample> {
        self.gpu_timing.take_completed()
    }

    /// Cumulative native-import GPU timing evidence.
    pub fn gpu_timing_diagnostics(&self) -> NativeVideoImportGpuTimingDiagnostics {
        self.gpu_timing.diagnostics()
    }

    /// One renderer/decoder device contract is retained for this backend.
    pub fn contract_pool_count(&self) -> usize {
        1
    }

    /// Same-device import owns no bridge textures.
    pub fn bridge_entry_count(&self) -> usize {
        0
    }

    /// Decoder sources still retained through GPU completion.
    pub fn retained_source_count(&self) -> usize {
        self.pending_wgpu_sources
            .len()
            .saturating_add(self.pending_sources.len())
            .saturating_add(self.poisoned_sources.len())
    }

    fn poison_submission(
        &mut self,
        texture: ID3D12Resource,
        source: PreviewNativeDecodedFrameHandle,
        acquire_commands: D3D12TransitionCommands,
        release_commands: D3D12TransitionCommands,
    ) {
        self.poisoned_sources.push(PoisonedSubmissionResidency {
            _texture: texture,
            _source: source,
            _acquire_commands: acquire_commands,
            _release_commands: release_commands,
        });
    }

    fn resource_in_flight(&self, texture: &ID3D12Resource) -> bool {
        let identity = texture.as_raw();
        self.pending_wgpu_sources
            .iter()
            .any(|source| source.texture.as_raw() == identity)
            || self.pending_sources.iter().any(|source| source._texture.as_raw() == identity)
            || self.poisoned_sources.iter().any(|source| source._texture.as_raw() == identity)
    }

    fn release_and_retain_source(
        &mut self,
        completion_value: u64,
        texture: ID3D12Resource,
        source: PreviewNativeDecodedFrameHandle,
        acquire_commands: D3D12TransitionCommands,
        release_commands: D3D12TransitionCommands,
    ) -> Result<(), GpuNativeDecodedFrameImportError> {
        if let Err(error) = release_commands.execute(&self.raw_queue) {
            self.poison_submission(texture, source, acquire_commands, release_commands);
            return Err(native_texture_error(error));
        }
        if let Err(error) =
            unsafe { self.raw_queue.Signal(&self.completion_fence, completion_value) }
        {
            self.poison_submission(texture, source, acquire_commands, release_commands);
            return Err(rejected(format!(
                "renderer completion-fence signal failed: {error}"
            )));
        }
        self.pending_sources.push(DirectSubmissionResidency {
            completion_value,
            _texture: texture,
            _source: source,
            _acquire_commands: acquire_commands,
            _release_commands: release_commands,
        });
        Ok(())
    }

    fn retain_until_wgpu_completion(
        &mut self,
        completion_value: u64,
        texture: ID3D12Resource,
        source: PreviewNativeDecodedFrameHandle,
        acquire_commands: D3D12TransitionCommands,
        release_commands: D3D12TransitionCommands,
    ) {
        let wgpu_complete = Arc::new(AtomicBool::new(false));
        let callback_complete = Arc::clone(&wgpu_complete);
        self.queue.on_submitted_work_done(move || {
            callback_complete.store(true, Ordering::Release);
        });
        self.pending_wgpu_sources.push(WgpuSubmissionResidency {
            completion_value,
            wgpu_complete,
            texture,
            source,
            acquire_commands,
            release_commands,
        });
    }

    /// Non-blockingly release sources whose renderer fence has completed.
    pub fn retire_completed_source_residency(
        &mut self,
    ) -> Result<usize, GpuNativeDecodedFrameImportError> {
        let mut waiting = Vec::with_capacity(self.pending_wgpu_sources.len());
        let mut pending = std::mem::take(&mut self.pending_wgpu_sources).into_iter();
        while let Some(residency) = pending.next() {
            if !residency.wgpu_complete.load(Ordering::Acquire) {
                waiting.push(residency);
                continue;
            }
            if let Err(error) = self.release_and_retain_source(
                residency.completion_value,
                residency.texture,
                residency.source,
                residency.acquire_commands,
                residency.release_commands,
            ) {
                waiting.extend(pending);
                self.pending_wgpu_sources = waiting;
                return Err(error);
            }
        }
        self.pending_wgpu_sources = waiting;

        let completed = unsafe { self.completion_fence.GetCompletedValue() };
        if completed == u64::MAX {
            self.pending_sources.clear();
            return Err(GpuNativeDecodedFrameImportError::NativeDeviceRemoved {
                reason: "same-device native import completion fence reported device removal"
                    .to_owned(),
            });
        }
        let before = self.pending_sources.len();
        self.pending_sources.retain(|source| source.completion_value > completed);
        Ok(before.saturating_sub(self.pending_sources.len()))
    }

    fn import_frame(
        &mut self,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &PreviewNativeDecodedFrame,
    ) -> Result<GpuColorFrameResource<GpuColorFrameWgpuResource>, GpuNativeDecodedFrameImportError>
    {
        let total_started = Instant::now();
        let _ = self.retire_completed_source_residency()?;
        if self.pending_wgpu_sources.len().saturating_add(self.pending_sources.len())
            >= self.max_frames_in_flight
        {
            return Err(GpuNativeDecodedFrameImportError::Backpressure {
                reason: format!(
                    "all {} same-device native decoder surfaces are still in renderer flight",
                    self.max_frames_in_flight
                ),
            });
        }

        let validation_started = Instant::now();
        let source =
            validated_d3d12_native_decoded_frame_for_luid(self.renderer_adapter_luid, native_frame)
                .map_err(|error| rejected(error.to_string()))?;
        if source.device.as_raw() != self.raw_device.as_raw() {
            return Err(rejected(
                "decoder surface was not allocated by the exact renderer D3D12 device".to_owned(),
            ));
        }
        let _ = unsafe { source.texture.SetName(w!("mondrian.decoder.d3d12va.surface")) };
        if self.resource_in_flight(&source.texture) {
            return Err(GpuNativeDecodedFrameImportError::Backpressure {
                reason:
                    "the same D3D12VA decoder surface already has a renderer submission in flight"
                        .to_owned(),
            });
        }
        validate_device_feature(&self.device, source.inspection.source_texture_format)
            .map_err(native_texture_error)?;
        validate_device_limits(&self.device, source.inspection).map_err(native_texture_error)?;
        let source_validation_us = elapsed_us(validation_started);

        let prepare_started = Instant::now();
        let yuv_plan = GpuNativeYuvDecodePlan::from_import_plan(
            plan,
            GpuNativeVideoExtent {
                width: source.inspection.storage_width,
                height: source.inspection.storage_height,
            },
        )
        .map_err(|error| rejected(error.to_string()))?;
        let (texture, luma, chroma) =
            adopt_wgpu_video_texture(&self.device, source.texture.clone(), source.inspection)
                .map_err(native_texture_error)?;
        let prepared_yuv = self.yuv_decoder.prepare_pass(
            &self.device,
            &yuv_plan,
            GpuNativeYuvPlaneViews { luma: &luma, chroma: &chroma, chroma_v: &chroma },
        );
        let (_, encoded_payload) =
            GpuNativeYuvDecoder::allocate_output(&self.device, &yuv_plan).into_parts();
        let encoded_resource =
            GpuColorFrameResource::new(plan.encoded_source_frame.clone(), encoded_payload);
        let mut acquire_commands =
            D3D12TransitionCommands::new(&self.raw_device).map_err(native_texture_error)?;
        let mut release_commands =
            D3D12TransitionCommands::new(&self.raw_device).map_err(native_texture_error)?;
        acquire_commands
            .record_transition(
                &source.texture,
                D3D12_RESOURCE_STATE_COMMON,
                WGPU_RESOURCE_STATE,
            )
            .map_err(native_texture_error)?;
        release_commands
            .record_transition(
                &source.texture,
                WGPU_RESOURCE_STATE,
                D3D12_RESOURCE_STATE_COMMON,
            )
            .map_err(native_texture_error)?;
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.native-video.d3d12-zero-copy"),
        });
        let mut timing_probe = self.gpu_timing.begin_import(
            &mut encoder,
            decode_fence_ready_at_admission(
                &source.decode_fence,
                source.inspection.decode_fence_value,
            ),
        );
        let pipeline_prepare_us = elapsed_us(prepare_started);

        // Reserve the fence value before native queue work. Exhaustion must
        // fail while the source is still in its decoder-owned state.
        let completion_value = self.next_completion_value;
        self.next_completion_value = self
            .next_completion_value
            .checked_add(1)
            .ok_or_else(|| rejected("native completion-fence value exhausted".to_owned()))?;

        let acquire_started = Instant::now();
        if let Err(error) = unsafe {
            self.raw_queue.Wait(&source.decode_fence, source.inspection.decode_fence_value)
        } {
            self.gpu_timing.abandon_before_submit(timing_probe);
            return Err(rejected(format!(
                "renderer queue decode-fence wait failed: {error}"
            )));
        }
        if let Err(error) = acquire_commands.execute(&self.raw_queue) {
            self.gpu_timing.abandon_before_submit(timing_probe);
            self.poison_submission(
                source.texture.clone(),
                native_frame.handle.clone(),
                acquire_commands,
                release_commands,
            );
            return Err(native_texture_error(error));
        }
        let bridge_acquire_us = elapsed_us(acquire_started);

        let record_result = (|| {
            let yuv_started = Instant::now();
            self.yuv_decoder
                .record(&mut encoder, &yuv_plan, &prepared_yuv, &encoded_resource)
                .map_err(|error| error.to_string())?;
            self.gpu_timing.mark_after_yuv(&mut encoder, &mut timing_probe);
            let yuv_record_us = elapsed_us(yuv_started);
            let color_started = Instant::now();
            if self
                .color_runtime
                .frame_table_mut()
                .insert(encoded_resource)
                .map_err(|error| format!("encoded source insertion failed: {error:?}"))?
                .is_some()
            {
                return Err("encoded source id replaced a live resource".to_owned());
            }
            self.color_runtime
                .record_wgpu_input_stage_gpu_frame_owned_backend(
                    &plan.input_transform,
                    &plan.encoded_source_frame,
                    &plan.working_frame,
                    RenderColorTransformGpuOptions {
                        output_residency: ColorFrameResidency::Gpu,
                        ..RenderColorTransformGpuOptions::default()
                    },
                    RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                        device: &self.device,
                        queue: &self.queue,
                        encoder: &mut encoder,
                        load_op: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    },
                )
                .map_err(format_input_stage_error)?;
            self.gpu_timing.mark_after_input_color(&mut encoder, &mut timing_probe);
            let color_stage_us = elapsed_us(color_started);
            let extract_started = Instant::now();
            let working = self
                .color_runtime
                .frame_table_mut()
                .remove(plan.working_frame.id())
                .ok_or_else(|| "OCIO input stage did not retain its working output".to_owned())?;
            self.color_runtime
                .frame_table_mut()
                .remove(plan.encoded_source_frame.id())
                .ok_or_else(|| "OCIO input stage lost its encoded source".to_owned())?;
            Ok((
                working,
                yuv_record_us,
                color_stage_us,
                elapsed_us(extract_started),
            ))
        })();

        let (working, yuv_record_us, color_stage_us, resource_extract_us) = match record_result {
            Ok(result) => result,
            Err(reason) => {
                self.gpu_timing.abandon_before_submit(timing_probe);
                self.color_runtime.frame_table_mut().remove(plan.working_frame.id());
                self.color_runtime.frame_table_mut().remove(plan.encoded_source_frame.id());
                if let Err(release_error) = self.release_and_retain_source(
                    completion_value,
                    source.texture.clone(),
                    native_frame.handle.clone(),
                    acquire_commands,
                    release_commands,
                ) {
                    return Err(rejected(format!(
                        "{reason}; source release also failed: {release_error}"
                    )));
                }
                return Err(rejected(reason));
            }
        };
        self.gpu_timing.finish_recording(&mut encoder, &mut timing_probe);

        let submit_started = Instant::now();
        let _submission = self.queue.submit(std::iter::once(encoder.finish()));
        self.retain_until_wgpu_completion(
            completion_value,
            source.texture.clone(),
            native_frame.handle.clone(),
            acquire_commands,
            release_commands,
        );
        self.gpu_timing.after_submit(timing_probe);
        drop((texture, luma, chroma));
        let submit_us = elapsed_us(submit_started);
        self.frame_cpu_timings.accumulate(NativeVideoImportCpuTimings {
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

impl GpuNativeDecodedFrameImportBackend for D3D12NativeVideoImportBackend {
    type NativeFrame = PreviewNativeDecodedFrame;
    type Resource = GpuColorFrameWgpuResource;

    fn support(&self) -> &GpuNativeDecodedFrameImportSupport {
        self.support()
    }

    fn import_native_decoded_frame(
        &mut self,
        plan: &GpuNativeDecodedFrameImportPlan,
        native_frame: &Self::NativeFrame,
    ) -> Result<GpuColorFrameResource<Self::Resource>, GpuNativeDecodedFrameImportError> {
        self.import_frame(plan, native_frame)
    }
}

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

fn decode_fence_ready_at_admission(fence: &ID3D12Fence, required: u64) -> Option<bool> {
    let completed = unsafe { fence.GetCompletedValue() };
    (completed != u64::MAX).then_some(completed >= required)
}

fn native_texture_error(error: D3D12NativeTextureError) -> GpuNativeDecodedFrameImportError {
    rejected(error.to_string())
}

fn rejected(reason: String) -> GpuNativeDecodedFrameImportError {
    GpuNativeDecodedFrameImportError::BackendRejected { reason }
}

fn format_input_stage_error(error: RenderGpuInputStageRuntimeRecordError) -> String {
    format!("OCIO native input stage failed: {error:?}")
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_bounds_same_device_source_residency() {
        let policy = D3D12NativeVideoImportBackendOptions::default();
        assert_eq!(policy.max_frames_in_flight_per_contract, 4);
        assert_eq!(policy.max_contract_pools, 1);
    }

    #[test]
    fn native_formats_require_complete_device_feature_contracts() {
        assert_eq!(
            conformed_decoder_surface_formats(wgpu::Features::TEXTURE_FORMAT_NV12)
                .expect("NV12 feature"),
            vec![GpuNativeDecodedFrameTextureFormat::Nv12]
        );
        assert!(matches!(
            conformed_decoder_surface_formats(wgpu::Features::TEXTURE_FORMAT_P010),
            Err(D3D12NativeVideoImportBackendCreateError::NoNativeYuvTextureFormats)
        ));
    }
}
