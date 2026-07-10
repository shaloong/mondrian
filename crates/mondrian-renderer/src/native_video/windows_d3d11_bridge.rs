//! Reusable D3D11 decoder-copy to wgpu DX12 shared-texture bridge.

use super::sync_timeline::{
    NativeVideoFrameSyncPlan, NativeVideoSyncPhase, NativeVideoSyncTimeline,
};
use super::windows_d3d11::{
    validated_d3d11_native_decoded_frame, D3D11NativeDecodedFrameInspection,
    D3D11NativeDecodedFrameInspectionError, NativeVideoAdapterLuid,
    ValidatedD3D11NativeDecodedFrame,
};
use crate::GpuNativeDecodedFrameTextureFormat;
use mondrian_media::PreviewNativeDecodedFrame;
use std::mem::ManuallyDrop;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device5, ID3D11DeviceContext4, ID3D11Fence, ID3D11Texture2D, D3D11_BIND_SHADER_RESOURCE,
    D3D11_FENCE_FLAG_SHARED, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
    D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Direct3D12::{
    ID3D12CommandAllocator, ID3D12CommandList, ID3D12Device, ID3D12Fence,
    ID3D12GraphicsCommandList, ID3D12PipelineState, ID3D12Resource, D3D12_COMMAND_LIST_TYPE_DIRECT,
    D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
    D3D12_RESOURCE_STATES, D3D12_RESOURCE_STATE_COMMON,
    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
    D3D12_RESOURCE_TRANSITION_BARRIER,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Graphics::Dxgi::{
    IDXGIResource1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE,
};

const WGPU_RESOURCE_STATE: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATES(
    D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE.0 | D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE.0,
);

/// Plane views for one prepared native NV12/P010 frame.
#[derive(Debug, Clone, Copy)]
pub struct D3D11Dx12VideoPlaneViews<'a> {
    /// Full-resolution luma plane.
    pub luma: &'a wgpu::TextureView,
    /// Half-resolution interleaved chroma plane.
    pub chroma: &'a wgpu::TextureView,
}

/// Token proving one bridge entry is prepared for exactly one renderer submit.
#[derive(Debug)]
#[must_use = "prepared native video frames must be submitted or explicitly discarded"]
pub struct D3D11Dx12PreparedVideoFrame {
    sync_plan: NativeVideoFrameSyncPlan,
    inspection: D3D11NativeDecodedFrameInspection,
}

impl D3D11Dx12PreparedVideoFrame {
    /// Validated visible/storage/source facts for this prepared frame.
    pub fn inspection(&self) -> D3D11NativeDecodedFrameInspection {
        self.inspection
    }
}

/// Error creating or driving a D3D11-to-DX12 shared native video texture.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum D3D11Dx12SharedVideoTextureError {
    /// Source admission failed before any cross-API work was submitted.
    #[error(transparent)]
    SourceInspection(#[from] D3D11NativeDecodedFrameInspectionError),
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
    #[error("D3D11/DX12 native video bridge failed during {operation}: HRESULT {hresult:#010x}")]
    WindowsApi {
        /// Stable operation name.
        operation: &'static str,
        /// Raw HRESULT value.
        hresult: i32,
    },
    /// A COM method succeeded without returning the required interface.
    #[error("D3D11/DX12 native video bridge returned no interface during {operation}")]
    MissingInterface {
        /// Stable operation name.
        operation: &'static str,
    },
    /// A frame came from another D3D11 decoder device.
    #[error("native frame D3D11 device does not match this shared texture entry")]
    SourceDeviceChanged,
    /// A frame cannot reuse this entry because its storage contract changed.
    #[error("native frame storage contract does not match this shared texture entry")]
    SourceContractChanged,
    /// The previous renderer use has not completed, so reusing the entry would stall or race.
    #[error("shared native video texture is busy until fence {required}, completed {completed}")]
    EntryBusy {
        /// Required renderer completion value.
        required: u64,
        /// Currently completed shared-fence value.
        completed: u64,
    },
    /// DX12 reported device removal through the shared fence completion sentinel.
    #[error("DX12 device was removed while waiting to reuse the shared video texture")]
    DeviceRemoved,
    /// The selected wgpu device does not match the validated adapter.
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
    #[error("unsupported D3D11/DX12 shared video texture format {format:?}")]
    UnsupportedNativeTextureFormat {
        /// Unsupported renderer texture format.
        format: GpuNativeDecodedFrameTextureFormat,
    },
}

/// One reusable low-copy NV12/P010 texture shared by D3D11 and wgpu DX12.
///
/// The bridge copies a decoder array slice into a single-slice shareable D3D11
/// texture. A shared timeline fence transfers ownership to DX12, explicit raw
/// barriers move the resource between COMMON and wgpu RESOURCE state, and the
/// renderer submission returns ownership to D3D11 without a CPU wait or Flush.
pub struct D3D11Dx12SharedVideoTexture {
    source_device_identity: usize,
    source_contract: D3D11BridgeSourceContract,
    d3d11_context: ID3D11DeviceContext4,
    d3d11_texture: ID3D11Texture2D,
    d3d11_fence: ID3D11Fence,
    d3d12_resource: ID3D12Resource,
    d3d12_fence: ID3D12Fence,
    d3d12_queue: windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue,
    acquire_commands: D3D12TransitionCommands,
    release_commands: D3D12TransitionCommands,
    _texture: wgpu::Texture,
    luma_view: wgpu::TextureView,
    chroma_view: wgpu::TextureView,
    queue: wgpu::Queue,
    timeline: NativeVideoSyncTimeline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct D3D11BridgeSourceContract {
    storage_width: u32,
    storage_height: u32,
    source_texture_format: GpuNativeDecodedFrameTextureFormat,
}

struct D3D12TransitionCommands {
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
}

impl D3D11Dx12SharedVideoTexture {
    /// Create one reusable bridge entry from the first validated decoder frame.
    pub fn new(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &PreviewNativeDecodedFrame,
    ) -> Result<Self, D3D11Dx12SharedVideoTextureError> {
        let source = validated_d3d11_native_decoded_frame(adapter, frame)?;
        Self::new_from_validated_source(device, queue, &source)
    }

    fn new_from_validated_source(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &ValidatedD3D11NativeDecodedFrame,
    ) -> Result<Self, D3D11Dx12SharedVideoTextureError> {
        validate_device_feature(device, source.inspection.source_texture_format)?;
        validate_device_limits(device, source.inspection)?;
        let device5: ID3D11Device5 = source
            .device
            .cast()
            .map_err(|error| windows_error("ID3D11Device::QueryInterface<ID3D11Device5>", error))?;
        // SAFETY: device5 is live and returns an owned immediate-context reference.
        let immediate_context = unsafe { device5.GetImmediateContext() }
            .map_err(|error| windows_error("ID3D11Device5::GetImmediateContext", error))?;
        let d3d11_context: ID3D11DeviceContext4 = immediate_context.cast().map_err(|error| {
            windows_error(
                "ID3D11DeviceContext::QueryInterface<ID3D11DeviceContext4>",
                error,
            )
        })?;
        let d3d11_texture = create_shared_d3d11_texture(source)?;
        let d3d11_fence = create_shared_d3d11_fence(&device5)?;

        // SAFETY: guards are used only to clone the wgpu-owned raw interfaces.
        let hal_device = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }
            .ok_or(D3D11Dx12SharedVideoTextureError::WgpuObjectIsNotDx12 { object: "device" })?;
        // SAFETY: same as above; the returned queue guard stays alive through cloning.
        let hal_queue = unsafe { queue.as_hal::<wgpu::hal::api::Dx12>() }
            .ok_or(D3D11Dx12SharedVideoTextureError::WgpuObjectIsNotDx12 { object: "queue" })?;
        let raw_device = hal_device.raw_device().clone();
        let d3d12_queue = hal_queue.as_raw().clone();
        validate_renderer_device_and_queue(
            &raw_device,
            &d3d12_queue,
            source.inspection.adapter_luid,
        )?;
        let d3d12_resource = open_shared_texture_on_d3d12(&raw_device, &d3d11_texture)?;
        let d3d12_fence = open_shared_fence_on_d3d12(&raw_device, &d3d11_fence)?;
        let acquire_commands = D3D12TransitionCommands::new(&raw_device)?;
        let release_commands = D3D12TransitionCommands::new(&raw_device)?;
        let (texture, luma_view, chroma_view) =
            adopt_wgpu_video_texture(device, d3d12_resource.clone(), source.inspection)?;

        Ok(Self {
            source_device_identity: source.device.as_raw() as usize,
            source_contract: source_contract(source.inspection),
            d3d11_context,
            d3d11_texture,
            d3d11_fence,
            d3d12_resource,
            d3d12_fence,
            d3d12_queue,
            acquire_commands,
            release_commands,
            _texture: texture,
            luma_view,
            chroma_view,
            queue: queue.clone(),
            timeline: NativeVideoSyncTimeline::default(),
        })
    }

    /// Copy one decoder surface into the bridge and acquire it for renderer use.
    pub fn begin_frame(
        &mut self,
        adapter: &wgpu::Adapter,
        frame: &PreviewNativeDecodedFrame,
    ) -> Result<D3D11Dx12PreparedVideoFrame, D3D11Dx12SharedVideoTextureError> {
        let source = validated_d3d11_native_decoded_frame(adapter, frame)?;
        self.begin_validated_frame(&source)
    }

    fn begin_validated_frame(
        &mut self,
        source: &ValidatedD3D11NativeDecodedFrame,
    ) -> Result<D3D11Dx12PreparedVideoFrame, D3D11Dx12SharedVideoTextureError> {
        self.validate_reuse(source)?;
        if let Some(required) = self.timeline.reusable_after() {
            // SAFETY: d3d12_fence is live; this is a lock-free completion query.
            let completed = unsafe { self.d3d12_fence.GetCompletedValue() };
            if completed == u64::MAX {
                return Err(D3D11Dx12SharedVideoTextureError::DeviceRemoved);
            }
            if completed < required {
                return Err(D3D11Dx12SharedVideoTextureError::EntryBusy { required, completed });
            }
        }

        let plan = self.timeline.begin_copy().map_err(sync_error)?;
        if let Err(error) = self.publish_d3d11_copy(source, plan) {
            self.timeline.poison();
            return Err(error);
        }
        self.timeline.publish_copy(plan).map_err(sync_error)?;
        if let Err(error) = self.acquire_dx12_resource(plan) {
            self.timeline.poison();
            return Err(error);
        }
        self.timeline.acquire_renderer(plan).map_err(sync_error)?;

        Ok(D3D11Dx12PreparedVideoFrame { sync_plan: plan, inspection: source.inspection })
    }

    /// Borrow plane views while recording the renderer commands for `prepared`.
    pub fn plane_views(
        &self,
        prepared: &D3D11Dx12PreparedVideoFrame,
    ) -> Result<D3D11Dx12VideoPlaneViews<'_>, D3D11Dx12SharedVideoTextureError> {
        self.timeline
            .validate_renderer_submission(prepared.sync_plan)
            .map_err(sync_error)?;
        Ok(D3D11Dx12VideoPlaneViews { luma: &self.luma_view, chroma: &self.chroma_view })
    }

    /// Submit renderer work, return the texture to COMMON, and publish completion.
    pub fn submit_renderer_commands<I>(
        &mut self,
        prepared: D3D11Dx12PreparedVideoFrame,
        commands: I,
    ) -> Result<wgpu::SubmissionIndex, D3D11Dx12SharedVideoTextureError>
    where
        I: IntoIterator<Item = wgpu::CommandBuffer>,
    {
        self.timeline
            .validate_renderer_submission(prepared.sync_plan)
            .map_err(sync_error)?;
        let submission = self.queue.submit(commands);
        if let Err(error) = self.release_dx12_resource(prepared.sync_plan) {
            self.timeline.poison();
            return Err(error);
        }
        self.timeline.release_renderer(prepared.sync_plan).map_err(sync_error)?;
        Ok(submission)
    }

    /// Release a prepared frame without renderer work after command recording fails.
    pub fn discard_prepared_frame(
        &mut self,
        prepared: D3D11Dx12PreparedVideoFrame,
    ) -> Result<wgpu::SubmissionIndex, D3D11Dx12SharedVideoTextureError> {
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

    fn validate_reuse(
        &self,
        source: &ValidatedD3D11NativeDecodedFrame,
    ) -> Result<(), D3D11Dx12SharedVideoTextureError> {
        if source.device.as_raw() as usize != self.source_device_identity {
            return Err(D3D11Dx12SharedVideoTextureError::SourceDeviceChanged);
        }
        if source_contract(source.inspection) != self.source_contract {
            return Err(D3D11Dx12SharedVideoTextureError::SourceContractChanged);
        }
        Ok(())
    }

    fn publish_d3d11_copy(
        &self,
        source: &ValidatedD3D11NativeDecodedFrame,
        plan: NativeVideoFrameSyncPlan,
    ) -> Result<(), D3D11Dx12SharedVideoTextureError> {
        if let Some(wait) = plan.wait_before_copy {
            // SAFETY: both interfaces share the same timeline fence and D3D11 device.
            unsafe { self.d3d11_context.Wait(&self.d3d11_fence, wait) }
                .map_err(|error| windows_error("ID3D11DeviceContext4::Wait", error))?;
        }
        // SAFETY: admission proved identical storage format/extent and valid
        // source array slice. Destination has one slice and one mip.
        unsafe {
            self.d3d11_context.CopySubresourceRegion(
                &self.d3d11_texture,
                0,
                0,
                0,
                0,
                &source.texture,
                source.inspection.array_slice,
                None,
            )
        };
        // SAFETY: signal is ordered after the copy on the immediate context.
        unsafe { self.d3d11_context.Signal(&self.d3d11_fence, plan.copy_ready) }
            .map_err(|error| windows_error("ID3D11DeviceContext4::Signal", error))
    }

    fn acquire_dx12_resource(
        &mut self,
        plan: NativeVideoFrameSyncPlan,
    ) -> Result<(), D3D11Dx12SharedVideoTextureError> {
        // SAFETY: the raw queue belongs to the same wgpu DX12 queue. FIFO Wait
        // gates the transition until D3D11 publishes copy_ready.
        unsafe { self.d3d12_queue.Wait(&self.d3d12_fence, plan.copy_ready) }
            .map_err(|error| windows_error("ID3D12CommandQueue::Wait", error))?;
        self.acquire_commands.record_transition(
            &self.d3d12_resource,
            D3D12_RESOURCE_STATE_COMMON,
            WGPU_RESOURCE_STATE,
        )?;
        self.acquire_commands.execute(&self.d3d12_queue)?;
        Ok(())
    }

    fn release_dx12_resource(
        &mut self,
        plan: NativeVideoFrameSyncPlan,
    ) -> Result<(), D3D11Dx12SharedVideoTextureError> {
        self.release_commands.record_transition(
            &self.d3d12_resource,
            WGPU_RESOURCE_STATE,
            D3D12_RESOURCE_STATE_COMMON,
        )?;
        self.release_commands.execute(&self.d3d12_queue)?;
        // SAFETY: signal is FIFO after renderer submit and the release barrier.
        unsafe { self.d3d12_queue.Signal(&self.d3d12_fence, plan.renderer_complete) }
            .map_err(|error| windows_error("ID3D12CommandQueue::Signal", error))
    }
}

impl D3D12TransitionCommands {
    fn new(
        device: &windows::Win32::Graphics::Direct3D12::ID3D12Device,
    ) -> Result<Self, D3D11Dx12SharedVideoTextureError> {
        // SAFETY: device is live and creates owned command objects.
        let allocator = unsafe {
            device.CreateCommandAllocator::<ID3D12CommandAllocator>(D3D12_COMMAND_LIST_TYPE_DIRECT)
        }
        .map_err(|error| windows_error("ID3D12Device::CreateCommandAllocator", error))?;
        // SAFETY: allocator belongs to device; no initial pipeline state is required.
        let list = unsafe {
            device.CreateCommandList::<_, _, ID3D12GraphicsCommandList>(
                0,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                &allocator,
                None::<&ID3D12PipelineState>,
            )
        }
        .map_err(|error| windows_error("ID3D12Device::CreateCommandList", error))?;
        // SAFETY: newly created lists are recording; close before the first reset.
        unsafe { list.Close() }
            .map_err(|error| windows_error("ID3D12GraphicsCommandList::Close", error))?;
        Ok(Self { allocator, list })
    }

    fn record_transition(
        &mut self,
        resource: &ID3D12Resource,
        before: D3D12_RESOURCE_STATES,
        after: D3D12_RESOURCE_STATES,
    ) -> Result<(), D3D11Dx12SharedVideoTextureError> {
        // SAFETY: callers only reset after the prior renderer-complete fence is observed.
        unsafe { self.allocator.Reset() }
            .map_err(|error| windows_error("ID3D12CommandAllocator::Reset", error))?;
        // SAFETY: allocator is reset and no pipeline state is required for barriers.
        unsafe { self.list.Reset(&self.allocator, None::<&ID3D12PipelineState>) }
            .map_err(|error| windows_error("ID3D12GraphicsCommandList::Reset", error))?;
        let mut barrier = transition_barrier(resource, before, after);
        // SAFETY: the barrier holds a live resource reference for this call.
        unsafe { self.list.ResourceBarrier(std::slice::from_ref(&barrier)) };
        release_barrier_resource_reference(&mut barrier);
        // SAFETY: the list is recording and contains one complete transition.
        unsafe { self.list.Close() }
            .map_err(|error| windows_error("ID3D12GraphicsCommandList::Close", error))
    }

    fn execute(
        &self,
        queue: &windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue,
    ) -> Result<(), D3D11Dx12SharedVideoTextureError> {
        let command_list: ID3D12CommandList = self.list.cast().map_err(|error| {
            windows_error(
                "ID3D12GraphicsCommandList::QueryInterface<ID3D12CommandList>",
                error,
            )
        })?;
        // SAFETY: list is closed and its allocator remains alive in this entry.
        unsafe { queue.ExecuteCommandLists(&[Some(command_list)]) };
        Ok(())
    }
}

fn create_shared_d3d11_texture(
    source: &ValidatedD3D11NativeDecodedFrame,
) -> Result<ID3D11Texture2D, D3D11Dx12SharedVideoTextureError> {
    let descriptor = D3D11_TEXTURE2D_DESC {
        Width: source.inspection.storage_width,
        Height: source.inspection.storage_height,
        MipLevels: 1,
        ArraySize: 1,
        Format: source.dxgi_format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0)
            as u32,
    };
    let mut texture = None;
    // SAFETY: descriptor is fully initialized; output receives an owned interface.
    unsafe { source.device.CreateTexture2D(&descriptor, None, Some(&mut texture)) }
        .map_err(|error| windows_error("ID3D11Device::CreateTexture2D", error))?;
    texture.ok_or(D3D11Dx12SharedVideoTextureError::MissingInterface {
        operation: "ID3D11Device::CreateTexture2D",
    })
}

fn create_shared_d3d11_fence(
    device: &ID3D11Device5,
) -> Result<ID3D11Fence, D3D11Dx12SharedVideoTextureError> {
    let mut fence = None;
    // SAFETY: output receives an owned shared timeline-fence interface.
    unsafe { device.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut fence) }
        .map_err(|error| windows_error("ID3D11Device5::CreateFence", error))?;
    fence.ok_or(D3D11Dx12SharedVideoTextureError::MissingInterface {
        operation: "ID3D11Device5::CreateFence",
    })
}

fn open_shared_texture_on_d3d12(
    device: &windows::Win32::Graphics::Direct3D12::ID3D12Device,
    texture: &ID3D11Texture2D,
) -> Result<ID3D12Resource, D3D11Dx12SharedVideoTextureError> {
    let resource: IDXGIResource1 = texture
        .cast()
        .map_err(|error| windows_error("ID3D11Texture2D::QueryInterface<IDXGIResource1>", error))?;
    // SAFETY: texture was created with SHARED_NTHANDLE; handle is process-owned.
    let handle = unsafe {
        resource.CreateSharedHandle(
            None,
            DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
            PCWSTR::null(),
        )
    }
    .map_err(|error| windows_error("IDXGIResource1::CreateSharedHandle", error))?;
    let handle = OwnedHandle(handle);
    let mut result = None;
    // SAFETY: handle names the shareable D3D11 texture on this adapter.
    unsafe { device.OpenSharedHandle(handle.0, &mut result) }
        .map_err(|error| windows_error("ID3D12Device::OpenSharedHandle(texture)", error))?;
    result.ok_or(D3D11Dx12SharedVideoTextureError::MissingInterface {
        operation: "ID3D12Device::OpenSharedHandle(texture)",
    })
}

fn open_shared_fence_on_d3d12(
    device: &windows::Win32::Graphics::Direct3D12::ID3D12Device,
    fence: &ID3D11Fence,
) -> Result<ID3D12Fence, D3D11Dx12SharedVideoTextureError> {
    // SAFETY: fence was created with D3D11_FENCE_FLAG_SHARED.
    let handle = unsafe { fence.CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null()) }
        .map_err(|error| windows_error("ID3D11Fence::CreateSharedHandle", error))?;
    let handle = OwnedHandle(handle);
    let mut result = None;
    // SAFETY: handle names the D3D11 timeline fence on this adapter.
    unsafe { device.OpenSharedHandle(handle.0, &mut result) }
        .map_err(|error| windows_error("ID3D12Device::OpenSharedHandle(fence)", error))?;
    result.ok_or(D3D11Dx12SharedVideoTextureError::MissingInterface {
        operation: "ID3D12Device::OpenSharedHandle(fence)",
    })
}

fn adopt_wgpu_video_texture(
    device: &wgpu::Device,
    resource: ID3D12Resource,
    inspection: D3D11NativeDecodedFrameInspection,
) -> Result<(wgpu::Texture, wgpu::TextureView, wgpu::TextureView), D3D11Dx12SharedVideoTextureError>
{
    let format = wgpu_texture_format(inspection.source_texture_format)?;
    let extent = wgpu::Extent3d {
        width: inspection.storage_width,
        height: inspection.storage_height,
        depth_or_array_layers: 1,
    };
    // SAFETY: resource was opened on this exact DX12 device, matches the
    // descriptor below, and remains alive through the returned HAL texture.
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
        label: Some("mondrian.native-video.d3d11-shared"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    };
    // SAFETY: the bridge transitions COMMON -> RESOURCE before exposing plane
    // views, and restores COMMON after each renderer submission. wgpu's tracker
    // therefore observes RESOURCE at every point where it can use the texture.
    let texture = unsafe {
        device.create_texture_from_hal::<wgpu::hal::api::Dx12>(
            hal_texture,
            &descriptor,
            wgpu::wgt::TextureUses::RESOURCE,
        )
    };
    let luma_view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("mondrian.native-video.luma"),
        format: Some(plane_format(format, wgpu::TextureAspect::Plane0)?),
        aspect: wgpu::TextureAspect::Plane0,
        ..wgpu::TextureViewDescriptor::default()
    });
    let chroma_view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("mondrian.native-video.chroma"),
        format: Some(plane_format(format, wgpu::TextureAspect::Plane1)?),
        aspect: wgpu::TextureAspect::Plane1,
        ..wgpu::TextureViewDescriptor::default()
    });
    Ok((texture, luma_view, chroma_view))
}

fn validate_device_feature(
    device: &wgpu::Device,
    format: GpuNativeDecodedFrameTextureFormat,
) -> Result<(), D3D11Dx12SharedVideoTextureError> {
    let (feature, name) = match format {
        GpuNativeDecodedFrameTextureFormat::Nv12 => {
            (wgpu::Features::TEXTURE_FORMAT_NV12, "TEXTURE_FORMAT_NV12")
        }
        GpuNativeDecodedFrameTextureFormat::P010 => {
            (wgpu::Features::TEXTURE_FORMAT_P010, "TEXTURE_FORMAT_P010")
        }
        GpuNativeDecodedFrameTextureFormat::Rgba8Unorm
        | GpuNativeDecodedFrameTextureFormat::Bgra8Unorm => {
            return Err(D3D11Dx12SharedVideoTextureError::SourceContractChanged);
        }
    };
    if !device.features().contains(feature) {
        return Err(D3D11Dx12SharedVideoTextureError::DeviceFeatureMissing { feature: name });
    }
    Ok(())
}

fn validate_device_limits(
    device: &wgpu::Device,
    inspection: D3D11NativeDecodedFrameInspection,
) -> Result<(), D3D11Dx12SharedVideoTextureError> {
    let limit = device.limits().max_texture_dimension_2d;
    if inspection.storage_width > limit || inspection.storage_height > limit {
        return Err(
            D3D11Dx12SharedVideoTextureError::DeviceTextureLimitExceeded {
                width: inspection.storage_width,
                height: inspection.storage_height,
                limit,
            },
        );
    }
    Ok(())
}

fn validate_renderer_device_and_queue(
    device: &ID3D12Device,
    queue: &windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue,
    adapter_luid: NativeVideoAdapterLuid,
) -> Result<(), D3D11Dx12SharedVideoTextureError> {
    // SAFETY: device is live and GetAdapterLuid returns POD adapter identity.
    let device_luid = NativeVideoAdapterLuid::from_windows(unsafe { device.GetAdapterLuid() });
    if device_luid != adapter_luid {
        return Err(
            D3D11Dx12SharedVideoTextureError::RendererDeviceAdapterMismatch {
                device_luid: device_luid.as_u64(),
                adapter_luid: adapter_luid.as_u64(),
            },
        );
    }
    let mut queue_device: Option<ID3D12Device> = None;
    // SAFETY: queue is live and returns an owned reference to its creating device.
    unsafe { queue.GetDevice(&mut queue_device) }
        .map_err(|error| windows_error("ID3D12CommandQueue::GetDevice", error))?;
    let queue_device = queue_device.ok_or(D3D11Dx12SharedVideoTextureError::MissingInterface {
        operation: "ID3D12CommandQueue::GetDevice",
    })?;
    if queue_device.as_raw() != device.as_raw() {
        return Err(D3D11Dx12SharedVideoTextureError::RendererQueueDeviceMismatch);
    }
    Ok(())
}

fn wgpu_texture_format(
    format: GpuNativeDecodedFrameTextureFormat,
) -> Result<wgpu::TextureFormat, D3D11Dx12SharedVideoTextureError> {
    match format {
        GpuNativeDecodedFrameTextureFormat::Nv12 => Ok(wgpu::TextureFormat::NV12),
        GpuNativeDecodedFrameTextureFormat::P010 => Ok(wgpu::TextureFormat::P010),
        GpuNativeDecodedFrameTextureFormat::Rgba8Unorm
        | GpuNativeDecodedFrameTextureFormat::Bgra8Unorm => {
            Err(D3D11Dx12SharedVideoTextureError::UnsupportedNativeTextureFormat { format })
        }
    }
}

fn plane_format(
    format: wgpu::TextureFormat,
    aspect: wgpu::TextureAspect,
) -> Result<wgpu::TextureFormat, D3D11Dx12SharedVideoTextureError> {
    format.aspect_specific_format(aspect).ok_or(
        D3D11Dx12SharedVideoTextureError::UnsupportedNativeTextureFormat {
            format: match format {
                wgpu::TextureFormat::P010 => GpuNativeDecodedFrameTextureFormat::P010,
                _ => GpuNativeDecodedFrameTextureFormat::Nv12,
            },
        },
    )
}

fn source_contract(inspection: D3D11NativeDecodedFrameInspection) -> D3D11BridgeSourceContract {
    D3D11BridgeSourceContract {
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
    // ResourceBarrier has consumed the borrowed descriptor synchronously, so
    // the temporary COM clone can be released now.
    unsafe {
        let transition = &mut *barrier.Anonymous.Transition;
        ManuallyDrop::drop(&mut transition.pResource);
    }
}

fn windows_error(
    operation: &'static str,
    error: windows::core::Error,
) -> D3D11Dx12SharedVideoTextureError {
    D3D11Dx12SharedVideoTextureError::WindowsApi { operation, hresult: error.code().0 }
}

fn sync_error(error: impl std::fmt::Display) -> D3D11Dx12SharedVideoTextureError {
    D3D11Dx12SharedVideoTextureError::SyncProtocol { reason: error.to_string() }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: OwnedHandle is constructed only from successful CreateSharedHandle calls.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
    };
    use windows::Win32::Graphics::Dxgi::IDXGIAdapter;

    #[test]
    fn source_contract_uses_storage_extent_and_native_format() {
        let inspection = D3D11NativeDecodedFrameInspection {
            visible_width: 1920,
            visible_height: 1080,
            storage_width: 1920,
            storage_height: 1088,
            array_slice: 7,
            source_texture_format: GpuNativeDecodedFrameTextureFormat::P010,
            adapter_luid: super::super::windows_d3d11::NativeVideoAdapterLuid::from_raw_for_test(5),
        };
        assert_eq!(
            source_contract(inspection),
            D3D11BridgeSourceContract {
                storage_width: 1920,
                storage_height: 1088,
                source_texture_format: GpuNativeDecodedFrameTextureFormat::P010,
            }
        );
    }

    #[test]
    fn plane_formats_match_native_video_layout() {
        assert_eq!(
            plane_format(wgpu::TextureFormat::NV12, wgpu::TextureAspect::Plane0)
                .expect("NV12 luma plane"),
            wgpu::TextureFormat::R8Unorm
        );
        assert_eq!(
            plane_format(wgpu::TextureFormat::NV12, wgpu::TextureAspect::Plane1)
                .expect("NV12 chroma plane"),
            wgpu::TextureFormat::Rg8Unorm
        );
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

    #[test]
    #[ignore = "manual D3D11/DX12 shared texture smoke; requires a DX12 adapter with NV12"]
    fn d3d11_dx12_shared_texture_and_fence_smoke() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("manual smoke requires a DX12 adapter");
        assert!(
            adapter.features().contains(wgpu::Features::TEXTURE_FORMAT_NV12),
            "manual smoke requires wgpu NV12 texture support"
        );
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::TEXTURE_FORMAT_NV12,
            ..wgpu::DeviceDescriptor::default()
        }))
        .expect("manual smoke requires a wgpu DX12 device");

        // SAFETY: the guard is borrowed only to clone the selected DXGI adapter.
        let hal_adapter = unsafe { adapter.as_hal::<wgpu::hal::api::Dx12>() }
            .expect("selected adapter must expose DX12 HAL");
        let dxgi_adapter: IDXGIAdapter = hal_adapter
            .raw_adapter()
            .cast()
            .expect("DX12 adapter must implement IDXGIAdapter");
        let adapter_luid = NativeVideoAdapterLuid::from_windows(
            unsafe { hal_adapter.raw_adapter().GetDesc2() }
                .expect("DXGI adapter descriptor must be available")
                .AdapterLuid,
        );
        drop(hal_adapter);

        let mut d3d11_device = None;
        // SAFETY: output receives an owned D3D11 device on the selected adapter.
        unsafe {
            D3D11CreateDevice(
                &dxgi_adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d11_device),
                None,
                None,
            )
        }
        .expect("D3D11 device creation must succeed on the selected adapter");
        let d3d11_device = d3d11_device.expect("D3D11CreateDevice must return a device");
        let descriptor = D3D11_TEXTURE2D_DESC {
            Width: 64,
            Height: 64,
            MipLevels: 1,
            ArraySize: 1,
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut source_texture = None;
        // SAFETY: descriptor is fully initialized; output owns the texture.
        unsafe { d3d11_device.CreateTexture2D(&descriptor, None, Some(&mut source_texture)) }
            .expect("manual smoke source NV12 texture creation must succeed");
        let source = ValidatedD3D11NativeDecodedFrame {
            inspection: D3D11NativeDecodedFrameInspection {
                visible_width: 64,
                visible_height: 64,
                storage_width: 64,
                storage_height: 64,
                array_slice: 0,
                source_texture_format: GpuNativeDecodedFrameTextureFormat::Nv12,
                adapter_luid,
            },
            texture: source_texture.expect("CreateTexture2D must return a source texture"),
            device: d3d11_device,
            dxgi_format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12,
        };

        let mut bridge =
            D3D11Dx12SharedVideoTexture::new_from_validated_source(&device, &queue, &source)
                .expect("shared texture and fence creation must succeed");
        let prepared = bridge
            .begin_validated_frame(&source)
            .expect("D3D11 copy and DX12 acquire must succeed");
        let views = bridge
            .plane_views(&prepared)
            .expect("prepared frame must expose both plane views");
        let _ = (views.luma, views.chroma);
        let _submission = bridge
            .discard_prepared_frame(prepared)
            .expect("empty renderer submit must return ownership to D3D11");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // SAFETY: shared fence stays alive in bridge for this query.
            let completed = unsafe { bridge.d3d12_fence.GetCompletedValue() };
            if completed >= 2 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "shared fence did not reach renderer completion"
            );
            device.poll(wgpu::PollType::Poll).expect("wgpu device poll must succeed");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(bridge.sync_phase(), "idle");
    }
}
