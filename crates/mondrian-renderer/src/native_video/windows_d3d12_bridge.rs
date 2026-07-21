//! Reusable D3D12VA decoder-copy to wgpu DX12 shared-texture bridge.

use super::sync_timeline::{
    NativeVideoFrameSyncPlan, NativeVideoSyncPhase, NativeVideoSyncTimeline,
};
use super::windows_adapter::NativeVideoAdapterLuid;
use super::windows_d3d12::{
    D3D12NativeDecodedFrameInspection, D3D12NativeDecodedFrameInspectionError,
    ValidatedD3D12NativeDecodedFrame,
};
use crate::GpuNativeDecodedFrameTextureFormat;
use std::mem::ManuallyDrop;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE};
use windows::Win32::Graphics::Direct3D12::{
    ID3D12CommandAllocator, ID3D12CommandList, ID3D12CommandQueue, ID3D12Device, ID3D12DeviceChild,
    ID3D12Fence, ID3D12GraphicsCommandList, ID3D12PipelineState, ID3D12Resource,
    D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_QUEUE_DESC, D3D12_COMMAND_QUEUE_FLAG_NONE,
    D3D12_COMMAND_QUEUE_PRIORITY_NORMAL, D3D12_FENCE_FLAG_SHARED, D3D12_HEAP_FLAG_SHARED,
    D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_BARRIER,
    D3D12_RESOURCE_BARRIER_0, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION, D3D12_RESOURCE_DESC,
    D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATES,
    D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE,
    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
    D3D12_RESOURCE_TRANSITION_BARRIER, D3D12_TEXTURE_LAYOUT_UNKNOWN,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;

const WGPU_RESOURCE_STATE: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATES(
    D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE.0 | D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE.0,
);

/// Plane views for one prepared D3D12VA NV12/P010 frame.
#[derive(Debug, Clone, Copy)]
pub struct D3D12VideoPlaneViews<'a> {
    /// Full-resolution luma plane.
    pub luma: &'a wgpu::TextureView,
    /// Half-resolution interleaved chroma plane.
    pub chroma: &'a wgpu::TextureView,
}

/// Token proving one bridge entry is prepared for exactly one renderer submit.
#[derive(Debug)]
#[must_use = "prepared native video frames must be submitted or explicitly discarded"]
pub struct D3D12PreparedVideoFrame {
    sync_plan: NativeVideoFrameSyncPlan,
}

/// Error creating or driving a D3D12VA-to-wgpu DX12 shared texture.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum D3D12SharedVideoTextureError {
    /// Source admission failed before any copy work was submitted.
    #[error(transparent)]
    SourceInspection(#[from] D3D12NativeDecodedFrameInspectionError),
    /// The supplied wgpu device or queue is not backed by DX12.
    #[error("wgpu {object} is not backed by DX12")]
    WgpuObjectIsNotDx12 {
        /// Device or queue.
        object: &'static str,
    },
    /// The device did not enable the native texture format required by the source.
    #[error("wgpu device did not enable required native video texture feature {feature}")]
    DeviceFeatureMissing {
        /// Stable feature name.
        feature: &'static str,
    },
    /// A Windows COM operation failed.
    #[error("D3D12 native video bridge failed during {operation}: HRESULT {hresult:#010x}")]
    WindowsApi {
        /// Stable operation name.
        operation: &'static str,
        /// Raw HRESULT value.
        hresult: i32,
    },
    /// A COM method succeeded without returning the required interface.
    #[error("D3D12 native video bridge returned no interface during {operation}")]
    MissingInterface {
        /// Stable operation name.
        operation: &'static str,
    },
    /// A frame came from another FFmpeg D3D12VA decoder device.
    #[error("native frame D3D12 device does not match this shared texture entry")]
    SourceDeviceChanged,
    /// A frame cannot reuse this entry because its storage contract changed.
    #[error("native frame storage contract does not match this shared texture entry")]
    SourceContractChanged,
    /// The previous renderer use has not completed, so reusing the entry would race.
    #[error("shared native video texture is busy until fence {required}, completed {completed}")]
    EntryBusy {
        /// Required renderer completion value.
        required: u64,
        /// Currently completed shared-fence value.
        completed: u64,
    },
    /// D3D12 reported device removal through the shared fence sentinel.
    #[error("D3D12 device was removed while waiting to reuse the shared video texture")]
    DeviceRemoved,
    /// The selected renderer device does not match the validated adapter.
    #[error(
        "wgpu DX12 device adapter LUID {device_luid:#018x} does not match selected adapter LUID {adapter_luid:#018x}"
    )]
    RendererDeviceAdapterMismatch {
        /// Actual wgpu device adapter.
        device_luid: u64,
        /// Selected and source-validated adapter.
        adapter_luid: u64,
    },
    /// The supplied wgpu queue belongs to another device.
    #[error("wgpu DX12 queue does not belong to the supplied device")]
    RendererQueueDeviceMismatch,
    /// The shared decoder texture exceeds the enabled wgpu device limit.
    #[error("native video texture {width}x{height} exceeds wgpu max_texture_dimension_2d {limit}")]
    DeviceTextureLimitExceeded {
        /// Storage width.
        width: u32,
        /// Storage height.
        height: u32,
        /// Enabled device limit.
        limit: u32,
    },
    /// The ownership token or phase does not match the requested operation.
    #[error("native video shared-texture sync protocol rejected the operation: {reason}")]
    SyncProtocol {
        /// Stable state-machine error.
        reason: String,
    },
    /// The bridge received a renderer texture format outside native NV12/P010.
    #[error("unsupported D3D12 shared video texture format {format:?}")]
    UnsupportedNativeTextureFormat {
        /// Unsupported renderer texture format.
        format: GpuNativeDecodedFrameTextureFormat,
    },
}

/// One reusable NV12/P010 texture shared between FFmpeg's D3D12 device and wgpu DX12.
///
/// The decoder queue waits for FFmpeg's per-frame decode fence, copies the
/// decoded resource into a renderer-created shared texture, and signals a
/// shared timeline fence. The renderer queue waits, samples through wgpu, then
/// restores COMMON and signals completion. All waits are GPU queue waits.
pub struct D3D12SharedVideoTexture {
    source_device_identity: usize,
    source_contract: D3D12BridgeSourceContract,
    decoder_queue: ID3D12CommandQueue,
    decoder_shared_texture: ID3D12Resource,
    decoder_shared_fence: ID3D12Fence,
    renderer_resource: ID3D12Resource,
    renderer_fence: ID3D12Fence,
    renderer_queue: ID3D12CommandQueue,
    copy_commands: D3D12CopyCommands,
    acquire_commands: D3D12TransitionCommands,
    release_commands: D3D12TransitionCommands,
    _texture: wgpu::Texture,
    luma_view: wgpu::TextureView,
    chroma_view: wgpu::TextureView,
    queue: wgpu::Queue,
    timeline: NativeVideoSyncTimeline,
    in_flight_source: Option<ID3D12Resource>,
    in_flight_decode_fence: Option<ID3D12Fence>,
    in_flight_copy_ready: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct D3D12BridgeSourceContract {
    storage_width: u32,
    storage_height: u32,
    source_texture_format: GpuNativeDecodedFrameTextureFormat,
}

struct D3D12CopyCommands {
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
}

struct D3D12TransitionCommands {
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
}

impl D3D12SharedVideoTexture {
    pub(super) fn new_from_validated_source(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &ValidatedD3D12NativeDecodedFrame,
    ) -> Result<Self, D3D12SharedVideoTextureError> {
        validate_device_feature(device, source.inspection.source_texture_format)?;
        validate_device_limits(device, source.inspection)?;
        // SAFETY: guards are used only to clone wgpu-owned raw interfaces.
        let hal_device = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }
            .ok_or(D3D12SharedVideoTextureError::WgpuObjectIsNotDx12 { object: "device" })?;
        // SAFETY: the queue guard remains alive through cloning.
        let hal_queue = unsafe { queue.as_hal::<wgpu::hal::api::Dx12>() }
            .ok_or(D3D12SharedVideoTextureError::WgpuObjectIsNotDx12 { object: "queue" })?;
        let renderer_device = hal_device.raw_device().clone();
        let renderer_queue = hal_queue.as_raw().clone();
        validate_renderer_device_and_queue(
            &renderer_device,
            &renderer_queue,
            source.inspection.adapter_luid,
        )?;

        let renderer_resource = create_renderer_shared_texture(&renderer_device, source)?;
        let decoder_shared_texture = open_shared_resource(
            &renderer_device,
            &source.device,
            &renderer_resource,
            "ID3D12Device::OpenSharedHandle(texture)",
        )?;
        let renderer_fence = create_renderer_shared_fence(&renderer_device)?;
        let decoder_shared_fence = open_shared_resource(
            &renderer_device,
            &source.device,
            &renderer_fence,
            "ID3D12Device::OpenSharedHandle(fence)",
        )?;
        let decoder_queue = create_direct_queue(&source.device)?;
        let copy_commands = D3D12CopyCommands::new(&source.device)?;
        let acquire_commands = D3D12TransitionCommands::new(&renderer_device)?;
        let release_commands = D3D12TransitionCommands::new(&renderer_device)?;
        let (texture, luma_view, chroma_view) =
            adopt_wgpu_video_texture(device, renderer_resource.clone(), source.inspection)?;

        Ok(Self {
            source_device_identity: source.device.as_raw() as usize,
            source_contract: source_contract(source.inspection),
            decoder_queue,
            decoder_shared_texture,
            decoder_shared_fence,
            renderer_resource,
            renderer_fence,
            renderer_queue,
            copy_commands,
            acquire_commands,
            release_commands,
            _texture: texture,
            luma_view,
            chroma_view,
            queue: queue.clone(),
            timeline: NativeVideoSyncTimeline::default(),
            in_flight_source: None,
            in_flight_decode_fence: None,
            in_flight_copy_ready: None,
        })
    }

    pub(super) fn begin_validated_frame(
        &mut self,
        source: &ValidatedD3D12NativeDecodedFrame,
    ) -> Result<D3D12PreparedVideoFrame, D3D12SharedVideoTextureError> {
        self.validate_reuse(source)?;
        if let Some(required) = self.timeline.reusable_after() {
            // SAFETY: renderer_fence is live; this is a lock-free completion query.
            let completed = unsafe { self.renderer_fence.GetCompletedValue() };
            if completed == u64::MAX {
                return Err(D3D12SharedVideoTextureError::DeviceRemoved);
            }
            if completed < required {
                return Err(D3D12SharedVideoTextureError::EntryBusy { required, completed });
            }
            // Renderer completion is ordered after the decoder-side copy. Retire
            // the source independently of entry reuse so the decoder can reclaim
            // its surface before it is asked to produce the next frame.
            self.retire_completed_source()?;
        }

        let plan = self.timeline.begin_copy().map_err(sync_error)?;
        if let Err(error) = self.publish_decoder_copy(source, plan) {
            self.timeline.poison();
            return Err(error);
        }
        self.timeline.publish_copy(plan).map_err(sync_error)?;
        if let Err(error) = self.acquire_renderer_resource(plan) {
            self.timeline.poison();
            return Err(error);
        }
        self.timeline.acquire_renderer(plan).map_err(sync_error)?;
        Ok(D3D12PreparedVideoFrame { sync_plan: plan })
    }

    /// Borrow plane views while recording renderer commands for `prepared`.
    pub fn plane_views(
        &self,
        prepared: &D3D12PreparedVideoFrame,
    ) -> Result<D3D12VideoPlaneViews<'_>, D3D12SharedVideoTextureError> {
        self.timeline
            .validate_renderer_submission(prepared.sync_plan)
            .map_err(sync_error)?;
        Ok(D3D12VideoPlaneViews { luma: &self.luma_view, chroma: &self.chroma_view })
    }

    /// Submit renderer work, restore COMMON, and publish completion.
    pub fn submit_renderer_commands<I>(
        &mut self,
        prepared: D3D12PreparedVideoFrame,
        commands: I,
    ) -> Result<wgpu::SubmissionIndex, D3D12SharedVideoTextureError>
    where
        I: IntoIterator<Item = wgpu::CommandBuffer>,
    {
        self.timeline
            .validate_renderer_submission(prepared.sync_plan)
            .map_err(sync_error)?;
        let submission = self.queue.submit(commands);
        if let Err(error) = self.release_renderer_resource(prepared.sync_plan) {
            self.timeline.poison();
            return Err(error);
        }
        self.timeline.release_renderer(prepared.sync_plan).map_err(sync_error)?;
        Ok(submission)
    }

    /// Release a prepared entry after renderer command recording fails.
    pub fn discard_prepared_frame(
        &mut self,
        prepared: D3D12PreparedVideoFrame,
    ) -> Result<wgpu::SubmissionIndex, D3D12SharedVideoTextureError> {
        self.submit_renderer_commands(prepared, std::iter::empty())
    }

    /// Whether this entry is idle, in flight, or permanently poisoned.
    pub fn sync_phase(&self) -> &'static str {
        match self.timeline.phase() {
            NativeVideoSyncPhase::Idle => "idle",
            NativeVideoSyncPhase::CopyReserved => "copy_reserved",
            NativeVideoSyncPhase::CopyPublished => "copy_published",
            NativeVideoSyncPhase::RendererAcquired => "renderer_acquired",
            NativeVideoSyncPhase::Poisoned => "poisoned",
        }
    }

    /// Lock-free completion query used by bounded contract-pool eviction.
    pub fn renderer_work_completed(&self) -> Result<bool, D3D12SharedVideoTextureError> {
        if self.timeline.phase() != NativeVideoSyncPhase::Idle {
            return Ok(false);
        }
        let Some(required) = self.timeline.reusable_after() else {
            return Ok(true);
        };
        // SAFETY: renderer_fence is live; this is a lock-free completion query.
        let completed = unsafe { self.renderer_fence.GetCompletedValue() };
        if completed == u64::MAX {
            return Err(D3D12SharedVideoTextureError::DeviceRemoved);
        }
        Ok(completed >= required)
    }

    /// Drop the decoder surface retained for command execution once its copy
    /// fence is complete, without waiting for this bridge entry to be reused.
    ///
    /// This is a lock-free completion query. Delaying release until the next
    /// imported frame can deadlock a bounded decoder pool: decoding that next
    /// frame may itself require the retained surface.
    pub(super) fn retire_completed_source(&mut self) -> Result<bool, D3D12SharedVideoTextureError> {
        let Some(required) = self.in_flight_copy_ready else {
            debug_assert!(self.in_flight_source.is_none());
            debug_assert!(self.in_flight_decode_fence.is_none());
            return Ok(false);
        };
        // SAFETY: decoder_shared_fence is live and GetCompletedValue is a
        // non-blocking observation of the shared copy/render timeline.
        let completed = unsafe { self.decoder_shared_fence.GetCompletedValue() };
        if completed == u64::MAX {
            return Err(D3D12SharedVideoTextureError::DeviceRemoved);
        }
        if completed < required {
            return Ok(false);
        }
        self.in_flight_source = None;
        self.in_flight_decode_fence = None;
        self.in_flight_copy_ready = None;
        Ok(true)
    }

    /// Whether one decoder source is retained until its copy fence completes.
    pub(super) fn has_retained_source(&self) -> bool {
        self.in_flight_source.is_some()
    }

    fn validate_reuse(
        &self,
        source: &ValidatedD3D12NativeDecodedFrame,
    ) -> Result<(), D3D12SharedVideoTextureError> {
        if source.device.as_raw() as usize != self.source_device_identity {
            return Err(D3D12SharedVideoTextureError::SourceDeviceChanged);
        }
        if source_contract(source.inspection) != self.source_contract {
            return Err(D3D12SharedVideoTextureError::SourceContractChanged);
        }
        Ok(())
    }

    fn publish_decoder_copy(
        &mut self,
        source: &ValidatedD3D12NativeDecodedFrame,
        plan: NativeVideoFrameSyncPlan,
    ) -> Result<(), D3D12SharedVideoTextureError> {
        // SAFETY: source fence and queue belong to the same FFmpeg device. This
        // GPU wait orders copy after decode without blocking the CPU.
        unsafe {
            self.decoder_queue
                .Wait(&source.decode_fence, source.inspection.decode_fence_value)
        }
        .map_err(|error| windows_error("decoder ID3D12CommandQueue::Wait", error))?;
        self.copy_commands.record_copy(&source.texture, &self.decoder_shared_texture)?;
        self.copy_commands.execute(&self.decoder_queue)?;
        self.in_flight_source = Some(source.texture.clone());
        self.in_flight_decode_fence = Some(source.decode_fence.clone());
        self.in_flight_copy_ready = Some(plan.copy_ready);
        // SAFETY: signal is FIFO after the copy and both fence/queue belong to
        // the FFmpeg device view of the shared timeline.
        unsafe { self.decoder_queue.Signal(&self.decoder_shared_fence, plan.copy_ready) }
            .map_err(|error| windows_error("decoder ID3D12CommandQueue::Signal", error))
    }

    fn acquire_renderer_resource(
        &mut self,
        plan: NativeVideoFrameSyncPlan,
    ) -> Result<(), D3D12SharedVideoTextureError> {
        // SAFETY: the raw queue is the same wgpu queue; FIFO Wait gates the
        // transition and sampling until decoder copy publication.
        unsafe { self.renderer_queue.Wait(&self.renderer_fence, plan.copy_ready) }
            .map_err(|error| windows_error("renderer ID3D12CommandQueue::Wait", error))?;
        self.acquire_commands.record_transition(
            &self.renderer_resource,
            D3D12_RESOURCE_STATE_COMMON,
            WGPU_RESOURCE_STATE,
        )?;
        self.acquire_commands.execute(&self.renderer_queue)
    }

    fn release_renderer_resource(
        &mut self,
        plan: NativeVideoFrameSyncPlan,
    ) -> Result<(), D3D12SharedVideoTextureError> {
        self.release_commands.record_transition(
            &self.renderer_resource,
            WGPU_RESOURCE_STATE,
            D3D12_RESOURCE_STATE_COMMON,
        )?;
        self.release_commands.execute(&self.renderer_queue)?;
        // SAFETY: signal is FIFO after wgpu work and the release barrier.
        unsafe { self.renderer_queue.Signal(&self.renderer_fence, plan.renderer_complete) }
            .map_err(|error| windows_error("renderer ID3D12CommandQueue::Signal", error))
    }
}

impl D3D12CopyCommands {
    fn new(device: &ID3D12Device) -> Result<Self, D3D12SharedVideoTextureError> {
        let (allocator, list) = create_command_objects(device)?;
        Ok(Self { allocator, list })
    }

    fn record_copy(
        &mut self,
        source: &ID3D12Resource,
        destination: &ID3D12Resource,
    ) -> Result<(), D3D12SharedVideoTextureError> {
        self.reset()?;
        let mut barriers = [
            transition_barrier(
                source,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            ),
            transition_barrier(
                destination,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_DEST,
            ),
        ];
        // SAFETY: barriers retain both resources for this synchronous record call.
        unsafe { self.list.ResourceBarrier(&barriers) };
        release_barrier_resource_references(&mut barriers);
        // SAFETY: admission and bridge creation proved identical format/extents.
        unsafe { self.list.CopyResource(destination, source) };
        let mut barriers = [
            transition_barrier(
                source,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            ),
            transition_barrier(
                destination,
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_COMMON,
            ),
        ];
        // SAFETY: same live resources and command-list recording interval.
        unsafe { self.list.ResourceBarrier(&barriers) };
        release_barrier_resource_references(&mut barriers);
        // SAFETY: list is recording and contains a complete copy transaction.
        unsafe { self.list.Close() }
            .map_err(|error| windows_error("copy ID3D12GraphicsCommandList::Close", error))
    }

    fn reset(&mut self) -> Result<(), D3D12SharedVideoTextureError> {
        // SAFETY: callers reset only after renderer completion proves prior copy completion.
        unsafe { self.allocator.Reset() }
            .map_err(|error| windows_error("copy ID3D12CommandAllocator::Reset", error))?;
        // SAFETY: allocator is reset and copy needs no pipeline state.
        unsafe { self.list.Reset(&self.allocator, None::<&ID3D12PipelineState>) }
            .map_err(|error| windows_error("copy ID3D12GraphicsCommandList::Reset", error))
    }

    fn execute(&self, queue: &ID3D12CommandQueue) -> Result<(), D3D12SharedVideoTextureError> {
        execute_command_list(queue, &self.list)
    }
}

impl D3D12TransitionCommands {
    fn new(device: &ID3D12Device) -> Result<Self, D3D12SharedVideoTextureError> {
        let (allocator, list) = create_command_objects(device)?;
        Ok(Self { allocator, list })
    }

    fn record_transition(
        &mut self,
        resource: &ID3D12Resource,
        before: D3D12_RESOURCE_STATES,
        after: D3D12_RESOURCE_STATES,
    ) -> Result<(), D3D12SharedVideoTextureError> {
        // SAFETY: callers reset only after the prior renderer-complete fence is observed.
        unsafe { self.allocator.Reset() }
            .map_err(|error| windows_error("transition ID3D12CommandAllocator::Reset", error))?;
        // SAFETY: allocator is reset and barriers need no pipeline state.
        unsafe { self.list.Reset(&self.allocator, None::<&ID3D12PipelineState>) }
            .map_err(|error| windows_error("transition ID3D12GraphicsCommandList::Reset", error))?;
        let mut barrier = transition_barrier(resource, before, after);
        // SAFETY: the barrier holds a live resource reference for this call.
        unsafe { self.list.ResourceBarrier(std::slice::from_ref(&barrier)) };
        release_barrier_resource_reference(&mut barrier);
        // SAFETY: the list contains one complete transition.
        unsafe { self.list.Close() }
            .map_err(|error| windows_error("transition ID3D12GraphicsCommandList::Close", error))
    }

    fn execute(&self, queue: &ID3D12CommandQueue) -> Result<(), D3D12SharedVideoTextureError> {
        execute_command_list(queue, &self.list)
    }
}

fn create_command_objects(
    device: &ID3D12Device,
) -> Result<(ID3D12CommandAllocator, ID3D12GraphicsCommandList), D3D12SharedVideoTextureError> {
    // SAFETY: device is live and creates owned direct-command objects.
    let allocator = unsafe {
        device.CreateCommandAllocator::<ID3D12CommandAllocator>(D3D12_COMMAND_LIST_TYPE_DIRECT)
    }
    .map_err(|error| windows_error("ID3D12Device::CreateCommandAllocator", error))?;
    // SAFETY: allocator belongs to device and no pipeline state is required.
    let list = unsafe {
        device.CreateCommandList::<_, _, ID3D12GraphicsCommandList>(
            0,
            D3D12_COMMAND_LIST_TYPE_DIRECT,
            &allocator,
            None::<&ID3D12PipelineState>,
        )
    }
    .map_err(|error| windows_error("ID3D12Device::CreateCommandList", error))?;
    // SAFETY: newly created lists are recording; close before first reset.
    unsafe { list.Close() }
        .map_err(|error| windows_error("ID3D12GraphicsCommandList::Close", error))?;
    Ok((allocator, list))
}

fn execute_command_list(
    queue: &ID3D12CommandQueue,
    list: &ID3D12GraphicsCommandList,
) -> Result<(), D3D12SharedVideoTextureError> {
    let command_list: ID3D12CommandList = list.cast().map_err(|error| {
        windows_error(
            "ID3D12GraphicsCommandList::QueryInterface<ID3D12CommandList>",
            error,
        )
    })?;
    // SAFETY: list is closed and its allocator remains alive in the entry.
    unsafe { queue.ExecuteCommandLists(&[Some(command_list)]) };
    Ok(())
}

fn create_direct_queue(
    device: &ID3D12Device,
) -> Result<ID3D12CommandQueue, D3D12SharedVideoTextureError> {
    let descriptor = D3D12_COMMAND_QUEUE_DESC {
        Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
        Priority: D3D12_COMMAND_QUEUE_PRIORITY_NORMAL.0,
        Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
        NodeMask: 0,
    };
    // SAFETY: descriptor is complete and returns an owned queue.
    unsafe { device.CreateCommandQueue(&descriptor) }
        .map_err(|error| windows_error("decoder ID3D12Device::CreateCommandQueue", error))
}

fn create_renderer_shared_texture(
    device: &ID3D12Device,
    source: &ValidatedD3D12NativeDecodedFrame,
) -> Result<ID3D12Resource, D3D12SharedVideoTextureError> {
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        ..Default::default()
    };
    let descriptor = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: u64::from(source.inspection.storage_width),
        Height: source.inspection.storage_height,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: source.dxgi_format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: D3D12_RESOURCE_FLAG_NONE,
    };
    let mut resource = None;
    // SAFETY: resource is renderer-owned, shareable, and starts in COMMON.
    unsafe {
        device.CreateCommittedResource(
            &heap,
            D3D12_HEAP_FLAG_SHARED,
            &descriptor,
            D3D12_RESOURCE_STATE_COMMON,
            None,
            &mut resource,
        )
    }
    .map_err(|error| windows_error("renderer ID3D12Device::CreateCommittedResource", error))?;
    resource.ok_or(D3D12SharedVideoTextureError::MissingInterface {
        operation: "renderer ID3D12Device::CreateCommittedResource",
    })
}

fn create_renderer_shared_fence(
    device: &ID3D12Device,
) -> Result<ID3D12Fence, D3D12SharedVideoTextureError> {
    // SAFETY: device is live and returns an owned shareable fence.
    unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_SHARED) }
        .map_err(|error| windows_error("renderer ID3D12Device::CreateFence", error))
}

fn open_shared_resource<T: Interface>(
    owner_device: &ID3D12Device,
    target_device: &ID3D12Device,
    object: &T,
    operation: &'static str,
) -> Result<T, D3D12SharedVideoTextureError> {
    let device_child: ID3D12DeviceChild = object
        .cast()
        .map_err(|error| windows_error("QueryInterface<ID3D12DeviceChild>", error))?;
    // SAFETY: object was created with a D3D12 shared resource/fence flag.
    let handle = unsafe {
        owner_device.CreateSharedHandle(&device_child, None, GENERIC_ALL.0, PCWSTR::null())
    }
    .map_err(|error| windows_error("ID3D12Device::CreateSharedHandle", error))?;
    let handle = OwnedHandle(handle);
    let mut opened = None;
    // SAFETY: handle names a same-adapter D3D12 object and output is owned.
    unsafe { target_device.OpenSharedHandle(handle.0, &mut opened) }
        .map_err(|error| windows_error(operation, error))?;
    opened.ok_or(D3D12SharedVideoTextureError::MissingInterface { operation })
}

fn adopt_wgpu_video_texture(
    device: &wgpu::Device,
    resource: ID3D12Resource,
    inspection: D3D12NativeDecodedFrameInspection,
) -> Result<(wgpu::Texture, wgpu::TextureView, wgpu::TextureView), D3D12SharedVideoTextureError> {
    let format = wgpu_texture_format(inspection.source_texture_format)?;
    let extent = wgpu::Extent3d {
        width: inspection.storage_width,
        height: inspection.storage_height,
        depth_or_array_layers: 1,
    };
    // SAFETY: resource was created on this exact DX12 device and remains alive
    // through both the HAL texture and explicit bridge-owned COM clone.
    let hal_texture = unsafe {
        wgpu::hal::dx12::Device::texture_from_raw(
            resource,
            format,
            wgpu::TextureDimension::D2,
            extent,
            1,
            1,
        )
    };
    let descriptor = wgpu::TextureDescriptor {
        label: Some("mondrian.native-video.d3d12-shared"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    };
    // SAFETY: bridge transitions COMMON -> RESOURCE before wgpu use and
    // restores COMMON after each renderer submission.
    let texture = unsafe {
        device.create_texture_from_hal::<wgpu::hal::api::Dx12>(
            hal_texture,
            &descriptor,
            wgpu::wgt::TextureUses::RESOURCE,
        )
    };
    let luma_view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("mondrian.native-video.d3d12-luma"),
        format: Some(plane_format(format, wgpu::TextureAspect::Plane0)?),
        aspect: wgpu::TextureAspect::Plane0,
        ..wgpu::TextureViewDescriptor::default()
    });
    let chroma_view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("mondrian.native-video.d3d12-chroma"),
        format: Some(plane_format(format, wgpu::TextureAspect::Plane1)?),
        aspect: wgpu::TextureAspect::Plane1,
        ..wgpu::TextureViewDescriptor::default()
    });
    Ok((texture, luma_view, chroma_view))
}

fn validate_device_feature(
    device: &wgpu::Device,
    format: GpuNativeDecodedFrameTextureFormat,
) -> Result<(), D3D12SharedVideoTextureError> {
    let (feature, name) = match format {
        GpuNativeDecodedFrameTextureFormat::Nv12 => {
            (wgpu::Features::TEXTURE_FORMAT_NV12, "TEXTURE_FORMAT_NV12")
        }
        GpuNativeDecodedFrameTextureFormat::P010 => {
            (wgpu::Features::TEXTURE_FORMAT_P010, "TEXTURE_FORMAT_P010")
        }
        GpuNativeDecodedFrameTextureFormat::Rgba8Unorm
        | GpuNativeDecodedFrameTextureFormat::Bgra8Unorm => {
            return Err(D3D12SharedVideoTextureError::SourceContractChanged);
        }
    };
    if !device.features().contains(feature) {
        return Err(D3D12SharedVideoTextureError::DeviceFeatureMissing { feature: name });
    }
    Ok(())
}

fn validate_device_limits(
    device: &wgpu::Device,
    inspection: D3D12NativeDecodedFrameInspection,
) -> Result<(), D3D12SharedVideoTextureError> {
    let limit = device.limits().max_texture_dimension_2d;
    if inspection.storage_width > limit || inspection.storage_height > limit {
        return Err(D3D12SharedVideoTextureError::DeviceTextureLimitExceeded {
            width: inspection.storage_width,
            height: inspection.storage_height,
            limit,
        });
    }
    Ok(())
}

fn validate_renderer_device_and_queue(
    device: &ID3D12Device,
    queue: &ID3D12CommandQueue,
    adapter_luid: NativeVideoAdapterLuid,
) -> Result<(), D3D12SharedVideoTextureError> {
    // SAFETY: device is live and GetAdapterLuid returns POD identity.
    let device_luid = NativeVideoAdapterLuid::from_windows(unsafe { device.GetAdapterLuid() });
    if device_luid != adapter_luid {
        return Err(
            D3D12SharedVideoTextureError::RendererDeviceAdapterMismatch {
                device_luid: device_luid.as_u64(),
                adapter_luid: adapter_luid.as_u64(),
            },
        );
    }
    let mut queue_device = None;
    // SAFETY: queue is live and returns an owned creating-device reference.
    unsafe { queue.GetDevice(&mut queue_device) }
        .map_err(|error| windows_error("renderer ID3D12CommandQueue::GetDevice", error))?;
    let queue_device: ID3D12Device =
        queue_device.ok_or(D3D12SharedVideoTextureError::MissingInterface {
            operation: "renderer ID3D12CommandQueue::GetDevice",
        })?;
    if queue_device.as_raw() != device.as_raw() {
        return Err(D3D12SharedVideoTextureError::RendererQueueDeviceMismatch);
    }
    Ok(())
}

fn wgpu_texture_format(
    format: GpuNativeDecodedFrameTextureFormat,
) -> Result<wgpu::TextureFormat, D3D12SharedVideoTextureError> {
    match format {
        GpuNativeDecodedFrameTextureFormat::Nv12 => Ok(wgpu::TextureFormat::NV12),
        GpuNativeDecodedFrameTextureFormat::P010 => Ok(wgpu::TextureFormat::P010),
        GpuNativeDecodedFrameTextureFormat::Rgba8Unorm
        | GpuNativeDecodedFrameTextureFormat::Bgra8Unorm => {
            Err(D3D12SharedVideoTextureError::UnsupportedNativeTextureFormat { format })
        }
    }
}

fn plane_format(
    format: wgpu::TextureFormat,
    aspect: wgpu::TextureAspect,
) -> Result<wgpu::TextureFormat, D3D12SharedVideoTextureError> {
    format.aspect_specific_format(aspect).ok_or(
        D3D12SharedVideoTextureError::UnsupportedNativeTextureFormat {
            format: match format {
                wgpu::TextureFormat::P010 => GpuNativeDecodedFrameTextureFormat::P010,
                _ => GpuNativeDecodedFrameTextureFormat::Nv12,
            },
        },
    )
}

fn source_contract(inspection: D3D12NativeDecodedFrameInspection) -> D3D12BridgeSourceContract {
    D3D12BridgeSourceContract {
        storage_width: inspection.storage_width,
        storage_height: inspection.storage_height,
        source_texture_format: inspection.source_texture_format,
    }
}

fn transition_barrier(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

fn release_barrier_resource_reference(barrier: &mut D3D12_RESOURCE_BARRIER) {
    // SAFETY: transition_barrier always initializes the Transition union arm.
    unsafe {
        let transition = &mut *barrier.Anonymous.Transition;
        ManuallyDrop::drop(&mut transition.pResource);
    }
}

fn release_barrier_resource_references(barriers: &mut [D3D12_RESOURCE_BARRIER]) {
    for barrier in barriers {
        release_barrier_resource_reference(barrier);
    }
}

fn windows_error(
    operation: &'static str,
    error: windows::core::Error,
) -> D3D12SharedVideoTextureError {
    D3D12SharedVideoTextureError::WindowsApi { operation, hresult: error.code().0 }
}

fn sync_error(error: impl std::fmt::Display) -> D3D12SharedVideoTextureError {
    D3D12SharedVideoTextureError::SyncProtocol { reason: error.to_string() }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: constructed only from successful D3D12 CreateSharedHandle calls.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_contract_uses_storage_extent_and_native_format() {
        let inspection = D3D12NativeDecodedFrameInspection {
            visible_width: 3840,
            visible_height: 2160,
            storage_width: 3840,
            storage_height: 2176,
            source_texture_format: GpuNativeDecodedFrameTextureFormat::P010,
            adapter_luid: NativeVideoAdapterLuid::from_raw_for_test(5),
            decode_fence_value: 9,
        };
        assert_eq!(
            source_contract(inspection),
            D3D12BridgeSourceContract {
                storage_width: 3840,
                storage_height: 2176,
                source_texture_format: GpuNativeDecodedFrameTextureFormat::P010,
            }
        );
    }

    #[test]
    fn plane_formats_match_native_video_layout() {
        assert_eq!(
            plane_format(wgpu::TextureFormat::P010, wgpu::TextureAspect::Plane0)
                .expect("P010 luma plane"),
            wgpu::TextureFormat::R16Unorm
        );
        assert_eq!(
            plane_format(wgpu::TextureFormat::P010, wgpu::TextureAspect::Plane1)
                .expect("P010 chroma plane"),
            wgpu::TextureFormat::Rg16Unorm
        );
    }
}
