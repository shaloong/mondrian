//! Same-device GPU-resident renderer output to hardware-encoder surfaces.
//!
//! The Windows implementation owns the D3D12 Video Process queue and every
//! cross-queue transition. Export and Media never reinterpret wgpu resources;
//! they exchange only move-only typed leases.

use mondrian_media::{
    D3D12ResidentEncodeInputFrame, D3D12ResidentEncodeReadyFrame, RendererHwAccelDeviceContext,
    ResidentEncodeBitDepth, ResidentEncodeColorimetry,
};

use crate::{GpuColorFrameTextureFormat, GpuResidentEncoderInputLease};

/// Exact resident RGB-to-encoder-surface conversion contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct D3D12ResidentEncodeAdapterContract {
    /// Coded width; 4:2:0 requires an even, non-zero value.
    pub width: u32,
    /// Coded height; 4:2:0 requires an even, non-zero value.
    pub height: u32,
    /// Cadence numerator.
    pub frame_rate_num: u32,
    /// Cadence denominator.
    pub frame_rate_den: u32,
    /// Renderer RGB and encoder YCbCr precision.
    pub bit_depth: ResidentEncodeBitDepth,
    /// Transfer/primaries/matrix identity already encoded into the RGB input.
    pub colorimetry: ResidentEncodeColorimetry,
    /// YCbCr range requested by the delivery contract.
    pub full_range: bool,
    /// Maximum native Video Process command submissions in flight.
    pub max_frames_in_flight: usize,
}

impl D3D12ResidentEncodeAdapterContract {
    /// Renderer output texture format required by this conversion.
    pub const fn source_texture_format(self) -> GpuColorFrameTextureFormat {
        match self.bit_depth {
            ResidentEncodeBitDepth::Eight => GpuColorFrameTextureFormat::Rgba8Unorm,
            ResidentEncodeBitDepth::Ten => GpuColorFrameTextureFormat::Rgba16Float,
        }
    }

    /// Validate platform-independent dimensions, cadence, and signal identity.
    pub fn validate_static(self) -> Result<(), D3D12ResidentEncodeAdapterCreateError> {
        if self.width == 0
            || self.height == 0
            || !self.width.is_multiple_of(2)
            || !self.height.is_multiple_of(2)
        {
            return Err(D3D12ResidentEncodeAdapterCreateError::InvalidDimensions {
                width: self.width,
                height: self.height,
            });
        }
        if self.frame_rate_num == 0 || self.frame_rate_den == 0 {
            return Err(D3D12ResidentEncodeAdapterCreateError::InvalidFrameRate {
                numerator: self.frame_rate_num,
                denominator: self.frame_rate_den,
            });
        }
        if self.max_frames_in_flight == 0 {
            return Err(D3D12ResidentEncodeAdapterCreateError::ZeroInFlightLimit);
        }
        if self.colorimetry == ResidentEncodeColorimetry::Rec2100Hlg {
            return Err(D3D12ResidentEncodeAdapterCreateError::UnsupportedSignal {
                reason: "D3D12 Video Process exposes no exact RGB HLG color-space identity",
            });
        }
        if self.full_range && self.colorimetry == ResidentEncodeColorimetry::Rec2100Pq {
            return Err(D3D12ResidentEncodeAdapterCreateError::UnsupportedSignal {
                reason: "D3D12 Video Process exposes no exact full-range PQ YCbCr identity",
            });
        }
        Ok(())
    }
}

/// Failure to create an exact-device resident encoder Adapter.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum D3D12ResidentEncodeAdapterCreateError {
    /// The active wgpu objects are not D3D12 objects.
    #[error("resident encode requires a wgpu DX12 {object}")]
    WrongBackend { object: &'static str },
    /// 4:2:0 surfaces require non-zero even dimensions.
    #[error("resident encode requires non-zero even dimensions, got {width}x{height}")]
    InvalidDimensions { width: u32, height: u32 },
    /// Cadence must be positive.
    #[error("resident encode has invalid frame rate {numerator}/{denominator}")]
    InvalidFrameRate { numerator: u32, denominator: u32 },
    /// At least one command slot must exist.
    #[error("resident encode in-flight limit must be greater than zero")]
    ZeroInFlightLimit,
    /// The OS API cannot express the exact authored signal.
    #[error("resident encode signal is unsupported: {reason}")]
    UnsupportedSignal { reason: &'static str },
    /// The exact adapter/driver rejected the conversion.
    #[error("D3D12 Video Process does not support the exact resident encode conversion")]
    ConversionUnsupported,
    /// Native object construction failed.
    #[error("resident encode D3D12 stage {stage} failed: {reason}")]
    D3D12 { stage: &'static str, reason: String },
    /// Media could not adopt the exact renderer device.
    #[error("resident encode could not create the FFmpeg device root: {reason}")]
    MediaDeviceRoot { reason: String },
}

/// Failure while converting one resident renderer output.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum D3D12ResidentEncodeSubmissionError {
    /// Renderer output does not match the sealed Adapter contract.
    #[error("resident encode source contract mismatch: {reason}")]
    SourceContract { reason: String },
    /// Media destination precision differs from the sealed Adapter contract.
    #[error("resident encode destination precision mismatch")]
    DestinationContract,
    /// Every bounded command slot remained in flight past the wait deadline.
    #[error("resident encode Video Process queue remained backpressured")]
    Backpressure,
    /// Native queue recording or synchronization failed.
    #[error("resident encode D3D12 stage {stage} failed: {reason}")]
    D3D12 { stage: &'static str, reason: String },
}

/// Cumulative no-host-pixel-boundary evidence from one Adapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct D3D12ResidentEncodeAdapterDiagnostics {
    /// RGB-to-YCbCr Video Process submissions.
    pub video_process_submissions: u64,
    /// CPU waits caused only by bounded command-slot backpressure.
    pub command_slot_waits: u64,
    /// Highest simultaneous command submissions.
    pub in_flight_high_water: u64,
    /// GPU pixel readbacks; qualified execution keeps this at zero.
    pub cpu_pixel_readbacks: u64,
    /// Rawvideo bytes; qualified execution keeps this at zero.
    pub rawvideo_pipe_bytes: u64,
    /// CPU-to-encoder uploads; qualified execution keeps this at zero.
    pub cpu_pixel_uploads: u64,
}

/// Same-device D3D12 Video Processor that writes FFmpeg-owned NV12/P010 surfaces.
pub struct D3D12ResidentEncodeAdapter {
    contract: D3D12ResidentEncodeAdapterContract,
    #[cfg(target_os = "windows")]
    inner: windows_impl::AdapterInner,
}

impl D3D12ResidentEncodeAdapter {
    /// Qualify and create an Adapter for one exact wgpu device and queue.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        contract: D3D12ResidentEncodeAdapterContract,
    ) -> Result<Self, D3D12ResidentEncodeAdapterCreateError> {
        contract.validate_static()?;
        #[cfg(target_os = "windows")]
        {
            Ok(Self {
                contract,
                inner: windows_impl::AdapterInner::new(device, queue, contract)?,
            })
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (device, queue);
            Err(D3D12ResidentEncodeAdapterCreateError::WrongBackend { object: "device" })
        }
    }

    /// Exact renderer-qualified FFmpeg D3D12VA device root.
    pub fn encoder_device_root(&self) -> RendererHwAccelDeviceContext {
        #[cfg(target_os = "windows")]
        {
            self.inner.encoder_device_root.clone()
        }
        #[cfg(not(target_os = "windows"))]
        unreachable!("a non-Windows resident Adapter cannot be constructed")
    }

    /// Convert one move-only renderer output into one producer-ready Media frame.
    #[cfg(target_os = "windows")]
    pub fn process(
        &mut self,
        source: GpuResidentEncoderInputLease,
        destination: D3D12ResidentEncodeInputFrame,
    ) -> Result<D3D12ResidentEncodeReadyFrame, D3D12ResidentEncodeSubmissionError> {
        self.inner.process(self.contract, source, destination)
    }

    /// Return cumulative resident conversion evidence.
    pub fn diagnostics(&self) -> D3D12ResidentEncodeAdapterDiagnostics {
        #[cfg(target_os = "windows")]
        {
            self.inner.diagnostics
        }
        #[cfg(not(target_os = "windows"))]
        D3D12ResidentEncodeAdapterDiagnostics::default()
    }
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use std::mem::{size_of, ManuallyDrop};
    use std::ptr;
    use std::time::Duration;

    use windows::core::Interface;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, RECT, WAIT_OBJECT_0};
    use windows::Win32::Graphics::Direct3D12::{
        ID3D12CommandAllocator, ID3D12CommandList, ID3D12CommandQueue, ID3D12Device, ID3D12Fence,
        ID3D12PipelineState, ID3D12Resource, D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS,
        D3D12_COMMAND_QUEUE_DESC, D3D12_COMMAND_QUEUE_FLAG_NONE,
        D3D12_COMMAND_QUEUE_PRIORITY_NORMAL, D3D12_FENCE_FLAG_NONE, D3D12_RESOURCE_BARRIER,
        D3D12_RESOURCE_BARRIER_0, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
        D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        D3D12_RESOURCE_STATES, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_RENDER_TARGET,
        D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ, D3D12_RESOURCE_STATE_VIDEO_PROCESS_WRITE,
        D3D12_RESOURCE_TRANSITION_BARRIER,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
        DXGI_COLOR_SPACE_TYPE, DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_FORMAT, DXGI_FORMAT_NV12,
        DXGI_FORMAT_P010, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_R8G8B8A8_UNORM,
        DXGI_RATIONAL,
    };
    use windows::Win32::Media::MediaFoundation::{
        ID3D12VideoDevice, ID3D12VideoProcessCommandList, ID3D12VideoProcessor,
        D3D12_FEATURE_DATA_VIDEO_PROCESS_SUPPORT, D3D12_FEATURE_VIDEO_PROCESS_SUPPORT,
        D3D12_VIDEO_FIELD_TYPE_NONE, D3D12_VIDEO_FORMAT, D3D12_VIDEO_FRAME_STEREO_FORMAT_NONE,
        D3D12_VIDEO_PROCESS_ALPHA_FILL_MODE_OPAQUE, D3D12_VIDEO_PROCESS_INPUT_STREAM,
        D3D12_VIDEO_PROCESS_INPUT_STREAM_ARGUMENTS, D3D12_VIDEO_PROCESS_INPUT_STREAM_DESC,
        D3D12_VIDEO_PROCESS_INPUT_STREAM_FLAG_NONE, D3D12_VIDEO_PROCESS_ORIENTATION_DEFAULT,
        D3D12_VIDEO_PROCESS_OUTPUT_STREAM, D3D12_VIDEO_PROCESS_OUTPUT_STREAM_ARGUMENTS,
        D3D12_VIDEO_PROCESS_OUTPUT_STREAM_DESC, D3D12_VIDEO_PROCESS_SUPPORT_FLAG_SUPPORTED,
        D3D12_VIDEO_PROCESS_TRANSFORM, D3D12_VIDEO_SAMPLE, D3D12_VIDEO_SIZE_RANGE,
    };
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    use super::*;

    const COMMAND_SLOT_WAIT: Duration = Duration::from_secs(30);

    struct CommandSlot {
        allocator: ID3D12CommandAllocator,
        list: ID3D12VideoProcessCommandList,
        completion_value: u64,
    }

    pub(super) struct AdapterInner {
        raw_device: ID3D12Device,
        raw_direct_queue: ID3D12CommandQueue,
        video_queue: ID3D12CommandQueue,
        processor: ID3D12VideoProcessor,
        render_ready_fence: ID3D12Fence,
        completion_fence: ID3D12Fence,
        next_render_ready_value: u64,
        next_completion_value: u64,
        slots: Vec<CommandSlot>,
        max_slots: usize,
        poisoned_sources: Vec<GpuResidentEncoderInputLease>,
        poisoned_destinations: Vec<D3D12ResidentEncodeInputFrame>,
        pub(super) encoder_device_root: RendererHwAccelDeviceContext,
        pub(super) diagnostics: D3D12ResidentEncodeAdapterDiagnostics,
    }

    impl AdapterInner {
        pub(super) fn new(
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            contract: D3D12ResidentEncodeAdapterContract,
        ) -> Result<Self, D3D12ResidentEncodeAdapterCreateError> {
            let hal_device = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }
                .ok_or(D3D12ResidentEncodeAdapterCreateError::WrongBackend { object: "device" })?;
            let hal_queue = unsafe { queue.as_hal::<wgpu::hal::api::Dx12>() }
                .ok_or(D3D12ResidentEncodeAdapterCreateError::WrongBackend { object: "queue" })?;
            let raw_device = hal_device.raw_device().clone();
            let raw_direct_queue = hal_queue.as_raw().clone();
            drop(hal_queue);
            drop(hal_device);
            let video_device: ID3D12VideoDevice = raw_device
                .cast()
                .map_err(|error| create_error("QueryInterface<ID3D12VideoDevice>", error))?;
            let (input_desc, output_desc) = stream_descriptors(contract);
            qualify_conversion(&video_device, contract, input_desc, output_desc)?;
            let processor = unsafe {
                video_device.CreateVideoProcessor::<ID3D12VideoProcessor>(
                    0,
                    &output_desc,
                    &[input_desc],
                )
            }
            .map_err(|error| create_error("CreateVideoProcessor", error))?;
            let queue_desc = D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS,
                Priority: D3D12_COMMAND_QUEUE_PRIORITY_NORMAL.0,
                Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
                NodeMask: 0,
            };
            let video_queue =
                unsafe { raw_device.CreateCommandQueue::<ID3D12CommandQueue>(&queue_desc) }
                    .map_err(|error| create_error("CreateCommandQueue(VIDEO_PROCESS)", error))?;
            let render_ready_fence = unsafe { raw_device.CreateFence(0, D3D12_FENCE_FLAG_NONE) }
                .map_err(|error| create_error("CreateFence(render_ready)", error))?;
            let completion_fence = unsafe { raw_device.CreateFence(0, D3D12_FENCE_FLAG_NONE) }
                .map_err(|error| create_error("CreateFence(completion)", error))?;
            let encoder_device_root = RendererHwAccelDeviceContext::from_d3d12_device(
                raw_device.clone(),
            )
            .map_err(|error| {
                D3D12ResidentEncodeAdapterCreateError::MediaDeviceRoot { reason: error.to_string() }
            })?;
            Ok(Self {
                raw_device,
                raw_direct_queue,
                video_queue,
                processor,
                render_ready_fence,
                completion_fence,
                next_render_ready_value: 1,
                next_completion_value: 1,
                slots: Vec::new(),
                max_slots: contract.max_frames_in_flight,
                poisoned_sources: Vec::new(),
                poisoned_destinations: Vec::new(),
                encoder_device_root,
                diagnostics: D3D12ResidentEncodeAdapterDiagnostics::default(),
            })
        }

        pub(super) fn process(
            &mut self,
            contract: D3D12ResidentEncodeAdapterContract,
            source: GpuResidentEncoderInputLease,
            destination: D3D12ResidentEncodeInputFrame,
        ) -> Result<D3D12ResidentEncodeReadyFrame, D3D12ResidentEncodeSubmissionError> {
            validate_frame_contract(contract, &source, &destination)?;
            let render_ready_value = reserve_value(&mut self.next_render_ready_value, "render")?;
            let completion_value = reserve_value(&mut self.next_completion_value, "completion")?;
            let slot_index = self.acquire_slot()?;
            let source_resource = {
                let hal = unsafe { source.texture().as_hal::<wgpu::hal::api::Dx12>() }.ok_or(
                    D3D12ResidentEncodeSubmissionError::SourceContract {
                        reason: "source texture is not a DX12 resource".to_owned(),
                    },
                )?;
                let resource = unsafe { hal.raw_resource().clone() };
                drop(hal);
                resource
            };
            let destination_resource = destination.resource().clone();
            let mut destination_device = None;
            unsafe { destination_resource.GetDevice::<ID3D12Device>(&mut destination_device) }
                .map_err(|error| submit_error("encoder_surface.GetDevice", error))?;
            let destination_device = destination_device
                .ok_or(D3D12ResidentEncodeSubmissionError::DestinationContract)?;
            if destination_device.as_raw() != self.raw_device.as_raw() {
                return Err(D3D12ResidentEncodeSubmissionError::DestinationContract);
            }
            record_process(
                &self.slots[slot_index],
                &self.processor,
                contract,
                &source_resource,
                &destination_resource,
            )?;
            if let Err(error) = unsafe {
                self.raw_direct_queue.Signal(&self.render_ready_fence, render_ready_value)
            } {
                self.poisoned_sources.push(source);
                return Err(submit_error("direct_queue.Signal(render_ready)", error));
            }
            if let Err(error) =
                unsafe { self.video_queue.Wait(&self.render_ready_fence, render_ready_value) }
            {
                self.poisoned_sources.push(source);
                return Err(submit_error("video_queue.Wait(render_ready)", error));
            }
            let command_list: ID3D12CommandList = self.slots[slot_index]
                .list
                .cast()
                .map_err(|error| submit_error("QueryInterface<ID3D12CommandList>", error))?;
            unsafe { self.video_queue.ExecuteCommandLists(&[Some(command_list)]) };
            if let Err(error) =
                unsafe { self.video_queue.Signal(destination.fence(), destination.fence_value()) }
            {
                self.poisoned_sources.push(source);
                self.poisoned_destinations.push(destination);
                return Err(submit_error("video_queue.Signal(encoder_surface)", error));
            }
            if let Err(error) =
                unsafe { self.video_queue.Signal(&self.completion_fence, completion_value) }
            {
                self.poisoned_sources.push(source);
                self.poisoned_destinations.push(destination);
                return Err(submit_error("video_queue.Signal(completion)", error));
            }
            self.slots[slot_index].completion_value = completion_value;
            if let Err(error) =
                unsafe { self.raw_direct_queue.Wait(&self.completion_fence, completion_value) }
            {
                self.poisoned_sources.push(source);
                self.poisoned_destinations.push(destination);
                return Err(submit_error("direct_queue.Wait(completion)", error));
            }
            // The direct-queue wait precedes future wgpu work and the native
            // list restored RENDER_TARGET, so exact-generation pool return is safe.
            drop(source);
            self.diagnostics.video_process_submissions =
                self.diagnostics.video_process_submissions.saturating_add(1);
            let completed = unsafe { self.completion_fence.GetCompletedValue() };
            let in_flight =
                self.slots.iter().filter(|slot| slot.completion_value > completed).count();
            self.diagnostics.in_flight_high_water = self
                .diagnostics
                .in_flight_high_water
                .max(u64::try_from(in_flight).unwrap_or(u64::MAX));
            // SAFETY: destination is COMMON and the exact reserved signal is queued.
            Ok(unsafe { destination.assume_producer_signal_enqueued() })
        }

        fn acquire_slot(&mut self) -> Result<usize, D3D12ResidentEncodeSubmissionError> {
            let completed = unsafe { self.completion_fence.GetCompletedValue() };
            if completed == u64::MAX {
                return Err(D3D12ResidentEncodeSubmissionError::D3D12 {
                    stage: "completion_fence.GetCompletedValue",
                    reason: "device removed".to_owned(),
                });
            }
            if let Some(index) =
                self.slots.iter().position(|slot| slot.completion_value <= completed)
            {
                return Ok(index);
            }
            if self.slots.len() < self.max_slots {
                self.slots.push(create_slot(&self.raw_device)?);
                return Ok(self.slots.len() - 1);
            }
            let Some((index, target)) = self
                .slots
                .iter()
                .enumerate()
                .min_by_key(|(_, slot)| slot.completion_value)
                .map(|(index, slot)| (index, slot.completion_value))
            else {
                return Err(D3D12ResidentEncodeSubmissionError::Backpressure);
            };
            self.diagnostics.command_slot_waits =
                self.diagnostics.command_slot_waits.saturating_add(1);
            wait_for_fence(&self.completion_fence, target, COMMAND_SLOT_WAIT)?;
            Ok(index)
        }
    }

    fn create_slot(
        device: &ID3D12Device,
    ) -> Result<CommandSlot, D3D12ResidentEncodeSubmissionError> {
        let allocator = unsafe {
            device.CreateCommandAllocator::<ID3D12CommandAllocator>(
                D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS,
            )
        }
        .map_err(|error| submit_error("CreateCommandAllocator(VIDEO_PROCESS)", error))?;
        let list = unsafe {
            device.CreateCommandList::<_, _, ID3D12VideoProcessCommandList>(
                0,
                D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS,
                &allocator,
                None::<&ID3D12PipelineState>,
            )
        }
        .map_err(|error| submit_error("CreateCommandList(VIDEO_PROCESS)", error))?;
        unsafe { list.Close() }
            .map_err(|error| submit_error("VideoProcessCommandList.Close", error))?;
        Ok(CommandSlot { allocator, list, completion_value: 0 })
    }

    fn record_process(
        slot: &CommandSlot,
        processor: &ID3D12VideoProcessor,
        contract: D3D12ResidentEncodeAdapterContract,
        source: &ID3D12Resource,
        destination: &ID3D12Resource,
    ) -> Result<(), D3D12ResidentEncodeSubmissionError> {
        unsafe { slot.allocator.Reset() }
            .map_err(|error| submit_error("CommandAllocator.Reset", error))?;
        unsafe { slot.list.Reset(&slot.allocator) }
            .map_err(|error| submit_error("VideoProcessCommandList.Reset", error))?;
        let mut barriers = [
            transition_barrier(
                source,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ,
            ),
            transition_barrier(
                destination,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_WRITE,
            ),
        ];
        unsafe { slot.list.ResourceBarrier(&barriers) };
        release_barriers(&mut barriers);
        let rect = RECT {
            left: 0,
            top: 0,
            right: contract.width as i32,
            bottom: contract.height as i32,
        };
        let mut input = D3D12_VIDEO_PROCESS_INPUT_STREAM_ARGUMENTS::default();
        input.InputStream[0] = D3D12_VIDEO_PROCESS_INPUT_STREAM {
            pTexture2D: ManuallyDrop::new(Some(source.clone())),
            Subresource: 0,
            ..D3D12_VIDEO_PROCESS_INPUT_STREAM::default()
        };
        input.Transform = D3D12_VIDEO_PROCESS_TRANSFORM {
            SourceRectangle: rect,
            DestinationRectangle: rect,
            Orientation: D3D12_VIDEO_PROCESS_ORIENTATION_DEFAULT,
        };
        input.Flags = D3D12_VIDEO_PROCESS_INPUT_STREAM_FLAG_NONE;
        let mut output = D3D12_VIDEO_PROCESS_OUTPUT_STREAM_ARGUMENTS::default();
        output.OutputStream[0] = D3D12_VIDEO_PROCESS_OUTPUT_STREAM {
            pTexture2D: ManuallyDrop::new(Some(destination.clone())),
            Subresource: 0,
        };
        output.TargetRectangle = rect;
        unsafe { slot.list.ProcessFrames(processor, &output, std::slice::from_ref(&input)) };
        unsafe {
            ManuallyDrop::drop(&mut input.InputStream[0].pTexture2D);
            ManuallyDrop::drop(&mut output.OutputStream[0].pTexture2D);
        }
        let mut barriers = [
            transition_barrier(
                source,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            ),
            transition_barrier(
                destination,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_WRITE,
                D3D12_RESOURCE_STATE_COMMON,
            ),
        ];
        unsafe { slot.list.ResourceBarrier(&barriers) };
        release_barriers(&mut barriers);
        unsafe { slot.list.Close() }
            .map_err(|error| submit_error("VideoProcessCommandList.Close", error))
    }

    fn validate_frame_contract(
        contract: D3D12ResidentEncodeAdapterContract,
        source: &GpuResidentEncoderInputLease,
        destination: &D3D12ResidentEncodeInputFrame,
    ) -> Result<(), D3D12ResidentEncodeSubmissionError> {
        let actual = source.contract();
        if actual.descriptor.width != contract.width
            || actual.descriptor.height != contract.height
            || actual.texture_format != contract.source_texture_format()
        {
            return Err(D3D12ResidentEncodeSubmissionError::SourceContract {
                reason: format!(
                    "expected {}x{} {:?}, got {}x{} {:?}",
                    contract.width,
                    contract.height,
                    contract.source_texture_format(),
                    actual.descriptor.width,
                    actual.descriptor.height,
                    actual.texture_format
                ),
            });
        }
        if destination.bit_depth() != contract.bit_depth {
            return Err(D3D12ResidentEncodeSubmissionError::DestinationContract);
        }
        Ok(())
    }

    fn stream_descriptors(
        contract: D3D12ResidentEncodeAdapterContract,
    ) -> (
        D3D12_VIDEO_PROCESS_INPUT_STREAM_DESC,
        D3D12_VIDEO_PROCESS_OUTPUT_STREAM_DESC,
    ) {
        let (source_format, destination_format, source_space, destination_space) =
            native_formats(contract);
        let rate = DXGI_RATIONAL {
            Numerator: contract.frame_rate_num,
            Denominator: contract.frame_rate_den,
        };
        let size_range = D3D12_VIDEO_SIZE_RANGE {
            MaxWidth: contract.width,
            MaxHeight: contract.height,
            MinWidth: contract.width,
            MinHeight: contract.height,
        };
        let input = D3D12_VIDEO_PROCESS_INPUT_STREAM_DESC {
            Format: source_format,
            ColorSpace: source_space,
            SourceAspectRatio: DXGI_RATIONAL {
                Numerator: contract.width,
                Denominator: contract.height,
            },
            DestinationAspectRatio: DXGI_RATIONAL {
                Numerator: contract.width,
                Denominator: contract.height,
            },
            FrameRate: rate,
            SourceSizeRange: size_range,
            DestinationSizeRange: size_range,
            FieldType: D3D12_VIDEO_FIELD_TYPE_NONE,
            StereoFormat: D3D12_VIDEO_FRAME_STEREO_FORMAT_NONE,
            ..D3D12_VIDEO_PROCESS_INPUT_STREAM_DESC::default()
        };
        let output = D3D12_VIDEO_PROCESS_OUTPUT_STREAM_DESC {
            Format: destination_format,
            ColorSpace: destination_space,
            AlphaFillMode: D3D12_VIDEO_PROCESS_ALPHA_FILL_MODE_OPAQUE,
            FrameRate: rate,
            ..D3D12_VIDEO_PROCESS_OUTPUT_STREAM_DESC::default()
        };
        (input, output)
    }

    fn qualify_conversion(
        device: &ID3D12VideoDevice,
        contract: D3D12ResidentEncodeAdapterContract,
        input_desc: D3D12_VIDEO_PROCESS_INPUT_STREAM_DESC,
        output_desc: D3D12_VIDEO_PROCESS_OUTPUT_STREAM_DESC,
    ) -> Result<(), D3D12ResidentEncodeAdapterCreateError> {
        let mut support = D3D12_FEATURE_DATA_VIDEO_PROCESS_SUPPORT {
            InputSample: D3D12_VIDEO_SAMPLE {
                Width: contract.width,
                Height: contract.height,
                Format: D3D12_VIDEO_FORMAT {
                    Format: input_desc.Format,
                    ColorSpace: input_desc.ColorSpace,
                },
            },
            InputFieldType: D3D12_VIDEO_FIELD_TYPE_NONE,
            InputStereoFormat: D3D12_VIDEO_FRAME_STEREO_FORMAT_NONE,
            InputFrameRate: input_desc.FrameRate,
            OutputFormat: D3D12_VIDEO_FORMAT {
                Format: output_desc.Format,
                ColorSpace: output_desc.ColorSpace,
            },
            OutputStereoFormat: D3D12_VIDEO_FRAME_STEREO_FORMAT_NONE,
            OutputFrameRate: output_desc.FrameRate,
            ..D3D12_FEATURE_DATA_VIDEO_PROCESS_SUPPORT::default()
        };
        unsafe {
            device.CheckFeatureSupport(
                D3D12_FEATURE_VIDEO_PROCESS_SUPPORT,
                ptr::from_mut(&mut support).cast(),
                u32::try_from(size_of::<D3D12_FEATURE_DATA_VIDEO_PROCESS_SUPPORT>())
                    .unwrap_or(u32::MAX),
            )
        }
        .map_err(|error| create_error("CheckFeatureSupport(VIDEO_PROCESS)", error))?;
        if !support.SupportFlags.contains(D3D12_VIDEO_PROCESS_SUPPORT_FLAG_SUPPORTED) {
            return Err(D3D12ResidentEncodeAdapterCreateError::ConversionUnsupported);
        }
        Ok(())
    }

    fn native_formats(
        contract: D3D12ResidentEncodeAdapterContract,
    ) -> (
        DXGI_FORMAT,
        DXGI_FORMAT,
        DXGI_COLOR_SPACE_TYPE,
        DXGI_COLOR_SPACE_TYPE,
    ) {
        let (source_format, destination_format) = match contract.bit_depth {
            ResidentEncodeBitDepth::Eight => (DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_NV12),
            ResidentEncodeBitDepth::Ten => (DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_P010),
        };
        let (source_space, destination_space) = match contract.colorimetry {
            ResidentEncodeColorimetry::Rec709 => (
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                if contract.full_range {
                    DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709
                } else {
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709
                },
            ),
            ResidentEncodeColorimetry::Rec2100Pq => (
                DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020,
            ),
            ResidentEncodeColorimetry::Rec2100Hlg => unreachable!("HLG rejected by validation"),
        };
        (
            source_format,
            destination_format,
            source_space,
            destination_space,
        )
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

    fn release_barriers(barriers: &mut [D3D12_RESOURCE_BARRIER]) {
        for barrier in barriers {
            unsafe {
                let transition = &mut *barrier.Anonymous.Transition;
                ManuallyDrop::drop(&mut transition.pResource);
            }
        }
    }

    fn reserve_value(
        next: &mut u64,
        stage: &'static str,
    ) -> Result<u64, D3D12ResidentEncodeSubmissionError> {
        let value = *next;
        *next = value.checked_add(1).ok_or_else(|| D3D12ResidentEncodeSubmissionError::D3D12 {
            stage,
            reason: "fence value exhausted".to_owned(),
        })?;
        Ok(value)
    }

    fn wait_for_fence(
        fence: &ID3D12Fence,
        value: u64,
        timeout: Duration,
    ) -> Result<(), D3D12ResidentEncodeSubmissionError> {
        if unsafe { fence.GetCompletedValue() } >= value {
            return Ok(());
        }
        let event = unsafe { CreateEventW(None, false, false, None) }
            .map_err(|error| submit_error("CreateEventW", error))?;
        let event = EventHandle(event);
        unsafe { fence.SetEventOnCompletion(value, event.0) }
            .map_err(|error| submit_error("Fence.SetEventOnCompletion", error))?;
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        if unsafe { WaitForSingleObject(event.0, timeout_ms) } != WAIT_OBJECT_0 {
            return Err(D3D12ResidentEncodeSubmissionError::Backpressure);
        }
        Ok(())
    }

    struct EventHandle(HANDLE);

    impl Drop for EventHandle {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    fn create_error(
        stage: &'static str,
        error: windows::core::Error,
    ) -> D3D12ResidentEncodeAdapterCreateError {
        D3D12ResidentEncodeAdapterCreateError::D3D12 { stage, reason: error.to_string() }
    }

    fn submit_error(
        stage: &'static str,
        error: windows::core::Error,
    ) -> D3D12ResidentEncodeSubmissionError {
        D3D12ResidentEncodeSubmissionError::D3D12 { stage, reason: error.to_string() }
    }
}
