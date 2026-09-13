//! Vulkan RGB output to FFmpeg CUDA/NVENC surfaces without host pixels.

use std::ffi::c_void;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;

use ash::vk;
use mondrian_media::{
    CudaResidentEncodeInputFrame, CudaResidentEncodeReadyFrame, RendererHwAccelDeviceContext,
    ResidentEncodeBitDepth,
};

use super::{
    validate_resident_source_contract, D3D12ResidentEncodeAdapterCreateError,
    D3D12ResidentEncodeSubmissionError, ResidentEncodeAdapterContract,
};
use crate::native_video::{cuda_buffer_layout, cuda_driver as cu};
use crate::GpuResidentEncoderInputLease;

const GPU_COMPLETION_TIMEOUT: Duration = Duration::from_secs(30);

/// Cumulative evidence from one Vulkan-to-CUDA resident conversion Adapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VulkanCudaResidentEncodeAdapterDiagnostics {
    /// RGB-to-YCbCr compute submissions on the production Vulkan queue.
    pub color_conversion_submissions: u64,
    /// Same-device CUDA copies into FFmpeg-owned NV12/P010 surfaces.
    pub device_to_device_copies: u64,
    /// Bounded waits for the exact Vulkan conversion submission.
    pub vulkan_completion_waits: u64,
    /// Bounded waits for the CUDA device-to-device copy.
    pub cuda_completion_waits: u64,
    /// GPU pixel readbacks; qualified execution keeps this at zero.
    pub cpu_pixel_readbacks: u64,
    /// Rawvideo bytes; qualified execution keeps this at zero.
    pub rawvideo_pipe_bytes: u64,
    /// Host-to-device pixel uploads; qualified execution keeps this at zero.
    pub cpu_pixel_uploads: u64,
}

/// Same-device Vulkan compute and CUDA copy Adapter for FFmpeg NVENC surfaces.
pub struct VulkanCudaResidentEncodeAdapter {
    contract: ResidentEncodeAdapterContract,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bridge: BridgeAllocation,
    driver: Arc<cu::CudaDriver>,
    device_uuid: [u8; 16],
    encoder_device_root: RendererHwAccelDeviceContext,
    interop: Option<CudaInterop>,
    poisoned: bool,
    diagnostics: VulkanCudaResidentEncodeAdapterDiagnostics,
}

impl VulkanCudaResidentEncodeAdapter {
    /// Qualify the active Vulkan adapter and create one exact-device CUDA root.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        contract: ResidentEncodeAdapterContract,
    ) -> Result<Self, D3D12ResidentEncodeAdapterCreateError> {
        contract.validate_vulkan_cuda()?;
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }.ok_or(
            D3D12ResidentEncodeAdapterCreateError::WrongPlatformBackend {
                required: "wgpu Vulkan",
                object: "device",
            },
        )?;
        if !hal.enabled_device_extensions().contains(&ash::khr::external_memory_fd::NAME) {
            return Err(D3D12ResidentEncodeAdapterCreateError::Native {
                backend: "Vulkan/CUDA",
                stage: "Vulkan external-memory admission",
                reason: "VK_KHR_external_memory_fd is unavailable".to_owned(),
            });
        }
        let instance = hal.shared_instance().raw_instance();
        let physical = hal.raw_physical_device();
        let mut id = vk::PhysicalDeviceIDProperties::default();
        let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
        unsafe { instance.get_physical_device_properties2(physical, &mut properties) };
        let driver = cu::CudaDriver::load().map_err(create_error("load CUDA driver"))?;
        let ordinal = u16::try_from(
            driver
                .matching_ordinal(id.device_uuid)
                .map_err(create_error("match Vulkan/CUDA UUID"))?,
        )
        .map_err(|_| D3D12ResidentEncodeAdapterCreateError::MediaDeviceRoot {
            reason: "CUDA ordinal exceeds the media selector contract".to_owned(),
        })?;
        let encoder_device_root = RendererHwAccelDeviceContext::from_cuda_device_ordinal(ordinal)
            .map_err(|error| {
            D3D12ResidentEncodeAdapterCreateError::MediaDeviceRoot { reason: error.to_string() }
        })?;
        let component_bytes = match contract.bit_depth {
            ResidentEncodeBitDepth::Eight => 1,
            ResidentEncodeBitDepth::Ten => 2,
        };
        let (row_pitch, chroma_offset, capacity) =
            cuda_buffer_layout(contract.width, contract.height, component_bytes).ok_or_else(
                || D3D12ResidentEncodeAdapterCreateError::Native {
                    backend: "Vulkan/CUDA",
                    stage: "resident buffer layout",
                    reason: "coded extent exceeds the bridge-buffer contract".to_owned(),
                },
            )?;
        if capacity > device.limits().max_storage_buffer_binding_size
            || capacity > device.limits().max_buffer_size
        {
            return Err(D3D12ResidentEncodeAdapterCreateError::Native {
                backend: "Vulkan/CUDA",
                stage: "resident buffer limits",
                reason: "bridge allocation exceeds active wgpu storage limits".to_owned(),
            });
        }
        let bridge = BridgeAllocation::new(
            device,
            instance,
            hal.raw_device(),
            physical,
            capacity,
            row_pitch,
            chroma_offset,
        )?;
        drop(hal);
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian.resident-cuda-convert-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mondrian.resident-cuda-convert-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.resident-cuda-rgb-to-yuv"),
            source: wgpu::ShaderSource::Wgsl(
                conversion_shader(contract, row_pitch, chroma_offset).into(),
            ),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("mondrian.resident-cuda-rgb-to-yuv"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Ok(Self {
            contract,
            device: device.clone(),
            queue: queue.clone(),
            pipeline,
            bind_group_layout,
            bridge,
            driver,
            device_uuid: id.device_uuid,
            encoder_device_root,
            interop: None,
            poisoned: false,
            diagnostics: VulkanCudaResidentEncodeAdapterDiagnostics::default(),
        })
    }

    /// Exact renderer-qualified FFmpeg CUDA device root.
    pub fn encoder_device_root(&self) -> RendererHwAccelDeviceContext {
        self.encoder_device_root.clone()
    }

    /// Convert one renderer output and copy it into one FFmpeg CUDA surface.
    pub fn process(
        &mut self,
        source: GpuResidentEncoderInputLease,
        destination: CudaResidentEncodeInputFrame,
    ) -> Result<CudaResidentEncodeReadyFrame, D3D12ResidentEncodeSubmissionError> {
        if self.poisoned {
            return Err(submit_error(
                "CUDA lifecycle",
                "Adapter stopped after an unproven native completion",
            ));
        }
        validate_resident_source_contract(self.contract, source.contract())?;
        let surface = destination.surface();
        if (surface.width, surface.height, surface.bit_depth)
            != (
                self.contract.width,
                self.contract.height,
                self.contract.bit_depth,
            )
        {
            return Err(D3D12ResidentEncodeSubmissionError::DestinationContract);
        }
        self.ensure_interop(surface.context)?;
        if self.interop.as_ref().is_none_or(|interop| interop.context != surface.context) {
            return Err(D3D12ResidentEncodeSubmissionError::DestinationContract);
        }
        let view = source.texture().create_view(&wgpu::TextureViewDescriptor::default());
        let bridge_buffer = self
            .bridge
            .buffer
            .as_ref()
            .ok_or_else(|| submit_error("Vulkan bridge", "bridge buffer already retired"))?;
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.resident-cuda-convert-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: bridge_buffer.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.resident-cuda-rgb-to-yuv"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mondrian.resident-cuda-rgb-to-yuv"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            let columns = match self.contract.bit_depth {
                ResidentEncodeBitDepth::Eight => self.contract.width.div_ceil(4),
                ResidentEncodeBitDepth::Ten => self.contract.width.div_ceil(2),
            };
            pass.dispatch_workgroups(
                columns.div_ceil(8),
                (self.contract.height / 2).div_ceil(8),
                1,
            );
        }
        let submission = self.queue.submit([encoder.finish()]);
        self.diagnostics.color_conversion_submissions =
            self.diagnostics.color_conversion_submissions.saturating_add(1);
        self.diagnostics.vulkan_completion_waits =
            self.diagnostics.vulkan_completion_waits.saturating_add(1);
        if let Err(error) = self.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(GPU_COMPLETION_TIMEOUT),
        }) {
            self.quarantine_native_parents();
            std::mem::forget(source);
            std::mem::forget(destination);
            return Err(submit_error("wait Vulkan conversion", error));
        }
        let copy_result = self.copy_to_cuda_surface(surface);
        if let Err(error) = copy_result {
            if self.synchronize_cuda().is_err() {
                self.poisoned = true;
                std::mem::forget(source);
                std::mem::forget(destination);
            }
            return Err(error);
        }
        drop(source);
        self.diagnostics.device_to_device_copies =
            self.diagnostics.device_to_device_copies.saturating_add(1);
        self.diagnostics.cuda_completion_waits =
            self.diagnostics.cuda_completion_waits.saturating_add(1);
        // SAFETY: copy_to_cuda_surface synchronizes the producer stream before success.
        Ok(unsafe { destination.assume_producer_copy_complete() })
    }

    /// Return cumulative no-host-pixel evidence.
    pub fn diagnostics(&self) -> VulkanCudaResidentEncodeAdapterDiagnostics {
        self.diagnostics
    }

    fn ensure_interop(
        &mut self,
        context: *mut c_void,
    ) -> Result<(), D3D12ResidentEncodeSubmissionError> {
        if self.interop.is_some() {
            return Ok(());
        }
        let fd =
            self.bridge.memory_fd.take().ok_or_else(|| {
                submit_error("import external memory", "export FD is unavailable")
            })?;
        let guard = unsafe { self.driver.enter(context) }
            .map_err(|error| submit_error("enter CUDA context", error))?;
        if let Err(error) = self.driver.require_current_uuid(self.device_uuid) {
            if let Err(restore) = guard.finish() {
                self.quarantine_native_parents();
                return Err(submit_error("restore rejected CUDA context", restore));
            }
            return Err(submit_error("match FFmpeg CUDA context", error));
        }
        let mut cuda_memory = std::ptr::null_mut();
        let descriptor = cu::MemoryHandle {
            kind: 1,
            handle: cu::ExternalHandle { fd: std::os::fd::AsRawFd::as_raw_fd(&fd) },
            size: self.bridge.capacity,
            flags: u32::from(self.bridge.dedicated),
            reserved: [0; 16],
        };
        if let Err(error) = cu::acquire(
            "cuImportExternalMemory",
            &mut cuda_memory,
            |output| unsafe { (self.driver.import_memory)(output, &descriptor) },
        ) {
            if let Err(restore) = guard.finish() {
                self.quarantine_native_parents();
                return Err(submit_error("restore failed CUDA import context", restore));
            }
            return Err(submit_error("import Vulkan memory into CUDA", error));
        }
        let _transferred_fd = fd.into_raw_fd();
        let mut mapped = 0usize;
        if let Err(error) = cu::acquire(
            "cuExternalMemoryGetMappedBuffer",
            &mut mapped,
            |output| unsafe {
                (self.driver.map_buffer)(
                    output,
                    cuda_memory,
                    &cu::BufferDesc {
                        offset: 0,
                        size: self.bridge.capacity,
                        flags: 0,
                        reserved: [0; 16],
                    },
                )
            },
        ) {
            let cleanup = cu::check("cuDestroyExternalMemory(partial map)", unsafe {
                (self.driver.destroy_memory)(cuda_memory)
            });
            let restore = guard.finish();
            if cleanup.is_err() || restore.is_err() {
                self.quarantine_native_parents();
                return Err(submit_error(
                    "cleanup failed CUDA map",
                    cleanup.err().map_or_else(
                        || {
                            restore
                                .err()
                                .map_or_else(|| error.to_string(), |value| value.to_string())
                        },
                        |value| value.to_string(),
                    ),
                ));
            }
            return Err(submit_error("map Vulkan memory in CUDA", error));
        }
        let mut stream = std::ptr::null_mut();
        if let Err(error) = cu::acquire("cuStreamCreate", &mut stream, |output| unsafe {
            (self.driver.stream_create)(output, 1)
        }) {
            let free = cu::check("cuMemFree(partial stream)", unsafe {
                (self.driver.free)(mapped)
            });
            let destroy = if free.is_ok() {
                cu::check("cuDestroyExternalMemory(partial stream)", unsafe {
                    (self.driver.destroy_memory)(cuda_memory)
                })
            } else {
                Ok(())
            };
            let restore = guard.finish();
            if free.is_err() || destroy.is_err() || restore.is_err() {
                self.quarantine_native_parents();
                let reason = free
                    .err()
                    .or_else(|| destroy.err())
                    .or_else(|| restore.err())
                    .map_or_else(|| error.to_string(), |value| value.to_string());
                return Err(submit_error("cleanup failed CUDA stream creation", reason));
            }
            return Err(submit_error("create CUDA copy stream", error));
        }
        if let Err(error) = guard.finish() {
            self.interop = Some(CudaInterop { context, stream, cuda_memory, mapped });
            self.poisoned = true;
            self.quarantine_native_parents();
            return Err(submit_error("restore CUDA context", error));
        }
        self.interop = Some(CudaInterop { context, stream, cuda_memory, mapped });
        Ok(())
    }

    fn copy_to_cuda_surface(
        &self,
        surface: mondrian_media::CudaResidentEncodeSurfaceView<'_>,
    ) -> Result<(), D3D12ResidentEncodeSubmissionError> {
        let interop = self
            .interop
            .as_ref()
            .ok_or_else(|| submit_error("CUDA copy", "interop is not initialized"))?;
        let component = match surface.bit_depth {
            ResidentEncodeBitDepth::Eight => 1usize,
            ResidentEncodeBitDepth::Ten => 2usize,
        };
        let guard = unsafe { self.driver.enter(interop.context) }
            .map_err(|error| submit_error("enter CUDA copy context", error))?;
        for plane in 0..2 {
            let copy = cu::Copy2d {
                src_x: 0,
                src_y: 0,
                src_kind: 2,
                src_host: std::ptr::null(),
                src_device: interop.mapped
                    + if plane == 0 {
                        0
                    } else {
                        self.bridge.chroma_offset as usize
                    },
                src_array: std::ptr::null_mut(),
                src_pitch: self.bridge.row_pitch as usize,
                dst_x: 0,
                dst_y: 0,
                dst_kind: 2,
                dst_host: std::ptr::null_mut(),
                dst_device: surface.planes[plane].0,
                dst_array: std::ptr::null_mut(),
                dst_pitch: surface.planes[plane].1,
                width_bytes: surface.width as usize * component,
                height: if plane == 0 {
                    surface.height as usize
                } else {
                    surface.height as usize / 2
                },
            };
            cu::check("cuMemcpy2DAsync", unsafe {
                (self.driver.copy_2d)(&copy, interop.stream)
            })
            .map_err(|error| submit_error("copy YCbCr plane to NVENC surface", error))?;
        }
        cu::check("cuStreamSynchronize", unsafe {
            (self.driver.stream_sync)(interop.stream)
        })
        .map_err(|error| submit_error("wait CUDA producer copy", error))?;
        guard
            .finish()
            .map_err(|error| submit_error("restore CUDA copy context", error))?;
        Ok(())
    }

    fn synchronize_cuda(&self) -> Result<(), cu::CudaError> {
        let Some(interop) = self.interop.as_ref() else {
            return Ok(());
        };
        let guard = unsafe { self.driver.enter(interop.context) }?;
        cu::check("cuStreamSynchronize(error cleanup)", unsafe {
            (self.driver.stream_sync)(interop.stream)
        })?;
        guard.finish()
    }
}

impl Drop for VulkanCudaResidentEncodeAdapter {
    fn drop(&mut self) {
        if let Some(interop) = self.interop.take()
            && let Err(error) = cleanup_cuda_interop(&self.driver, &interop)
        {
            tracing::error!(%error, "resident CUDA cleanup quarantined native resources");
            self.quarantine_native_parents();
        }
    }
}

impl VulkanCudaResidentEncodeAdapter {
    fn quarantine_native_parents(&mut self) {
        self.poisoned = true;
        self.bridge.quarantine();
        std::mem::forget(self.device.clone());
        std::mem::forget(self.queue.clone());
        std::mem::forget(self.encoder_device_root.clone());
        std::mem::forget(Arc::clone(&self.driver));
    }
}

struct CudaInterop {
    context: *mut c_void,
    stream: *mut c_void,
    cuda_memory: *mut c_void,
    mapped: usize,
}

fn cleanup_cuda_interop(
    driver: &cu::CudaDriver,
    interop: &CudaInterop,
) -> Result<(), cu::CudaError> {
    let guard = unsafe { driver.enter(interop.context) }?;
    if !interop.stream.is_null() {
        cu::check("cuStreamSynchronize(cleanup)", unsafe {
            (driver.stream_sync)(interop.stream)
        })?;
        cu::check("cuStreamDestroy(cleanup)", unsafe {
            (driver.stream_destroy)(interop.stream)
        })?;
    }
    if interop.mapped != 0 {
        cu::check("cuMemFree(cleanup)", unsafe {
            (driver.free)(interop.mapped)
        })?;
    }
    if !interop.cuda_memory.is_null() {
        cu::check("cuDestroyExternalMemory(cleanup)", unsafe {
            (driver.destroy_memory)(interop.cuda_memory)
        })?;
    }
    guard.finish()
}

struct BridgeAllocation {
    device: wgpu::Device,
    raw: ash::Device,
    buffer: Option<wgpu::Buffer>,
    raw_buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    memory_fd: Option<OwnedFd>,
    capacity: u64,
    row_pitch: u32,
    chroma_offset: u32,
    dedicated: bool,
    quarantined: bool,
}

impl BridgeAllocation {
    fn new(
        device: &wgpu::Device,
        instance: &ash::Instance,
        raw: &ash::Device,
        physical: vk::PhysicalDevice,
        capacity: u64,
        row_pitch: u32,
        chroma_offset: u32,
    ) -> Result<Self, D3D12ResidentEncodeAdapterCreateError> {
        let info = vk::PhysicalDeviceExternalBufferInfo::default()
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let mut external = vk::ExternalBufferProperties::default();
        unsafe {
            instance.get_physical_device_external_buffer_properties(physical, &info, &mut external)
        };
        if !external
            .external_memory_properties
            .external_memory_features
            .contains(vk::ExternalMemoryFeatureFlags::EXPORTABLE)
        {
            return Err(create_error("qualify Vulkan external buffer")(
                "storage-buffer memory is not exportable as OPAQUE_FD",
            ));
        }
        let dedicated = external
            .external_memory_properties
            .external_memory_features
            .contains(vk::ExternalMemoryFeatureFlags::DEDICATED_ONLY);
        let mut external_info = vk::ExternalMemoryBufferCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let buffer_info = vk::BufferCreateInfo::default()
            .size(capacity)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_info);
        let raw_buffer = unsafe { raw.create_buffer(&buffer_info, None) }
            .map_err(create_error("create Vulkan bridge buffer"))?;
        let requirements = unsafe { raw.get_buffer_memory_requirements(raw_buffer) };
        if requirements.size > capacity {
            unsafe { raw.destroy_buffer(raw_buffer, None) };
            return Err(create_error("validate Vulkan memory requirements")(
                "driver allocation exceeds admitted capacity",
            ));
        }
        let properties = unsafe { instance.get_physical_device_memory_properties(physical) };
        let memory_type = (0..properties.memory_type_count).find(|index| {
            requirements.memory_type_bits & (1 << index) != 0
                && properties.memory_types[*index as usize]
                    .property_flags
                    .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        });
        let Some(memory_type) = memory_type else {
            unsafe { raw.destroy_buffer(raw_buffer, None) };
            return Err(create_error("select Vulkan bridge memory")(
                "no device-local memory type",
            ));
        };
        let mut export = vk::ExportMemoryAllocateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default().buffer(raw_buffer);
        let mut allocation = vk::MemoryAllocateInfo::default()
            .allocation_size(capacity)
            .memory_type_index(memory_type)
            .push_next(&mut export);
        if dedicated {
            allocation = allocation.push_next(&mut dedicated_info);
        }
        let memory = match unsafe { raw.allocate_memory(&allocation, None) } {
            Ok(memory) => memory,
            Err(error) => {
                unsafe { raw.destroy_buffer(raw_buffer, None) };
                return Err(create_error("allocate Vulkan bridge memory")(error));
            }
        };
        if let Err(error) = unsafe { raw.bind_buffer_memory(raw_buffer, memory, 0) } {
            unsafe {
                raw.free_memory(memory, None);
                raw.destroy_buffer(raw_buffer, None);
            }
            return Err(create_error("bind Vulkan bridge memory")(error));
        }
        let memory_fd = ash::khr::external_memory_fd::Device::new(instance, raw);
        let fd = match unsafe {
            memory_fd.get_memory_fd(
                &vk::MemoryGetFdInfoKHR::default()
                    .memory(memory)
                    .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD),
            )
        } {
            Ok(fd) => fd,
            Err(error) => {
                unsafe {
                    raw.destroy_buffer(raw_buffer, None);
                    raw.free_memory(memory, None);
                }
                return Err(create_error("export Vulkan bridge memory FD")(error));
            }
        };
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let hal_buffer = unsafe {
            wgpu::hal::vulkan::Buffer::from_raw_externally_owned(raw_buffer, Box::new(|| {}))
        };
        let buffer = unsafe {
            device.create_buffer_from_hal::<wgpu::hal::api::Vulkan>(
                hal_buffer,
                &wgpu::BufferDescriptor {
                    label: Some("mondrian.resident-cuda-yuv-bridge"),
                    size: capacity,
                    usage: wgpu::BufferUsages::STORAGE,
                    mapped_at_creation: false,
                },
            )
        };
        Ok(Self {
            device: device.clone(),
            raw: raw.clone(),
            buffer: Some(buffer),
            raw_buffer,
            memory,
            memory_fd: Some(fd),
            capacity,
            row_pitch,
            chroma_offset,
            dedicated,
            quarantined: false,
        })
    }

    fn quarantine(&mut self) {
        self.quarantined = true;
        if let Some(buffer) = self.buffer.take() {
            std::mem::forget(buffer);
        }
        std::mem::forget(self.device.clone());
    }
}

impl Drop for BridgeAllocation {
    fn drop(&mut self) {
        if self.quarantined {
            return;
        }
        if let Err(error) = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(GPU_COMPLETION_TIMEOUT),
        }) {
            tracing::error!(%error, "resident Vulkan bridge completion is unproven; quarantining allocation");
            self.quarantine();
            return;
        }
        drop(self.buffer.take());
        unsafe {
            self.raw.destroy_buffer(self.raw_buffer, None);
            self.raw.free_memory(self.memory, None);
        }
    }
}

fn create_error<E: ToString>(
    stage: &'static str,
) -> impl FnOnce(E) -> D3D12ResidentEncodeAdapterCreateError {
    move |error| D3D12ResidentEncodeAdapterCreateError::Native {
        backend: "Vulkan/CUDA",
        stage,
        reason: error.to_string(),
    }
}

fn submit_error(stage: &'static str, error: impl ToString) -> D3D12ResidentEncodeSubmissionError {
    D3D12ResidentEncodeSubmissionError::Native {
        backend: "Vulkan/CUDA",
        stage,
        reason: error.to_string(),
    }
}

fn conversion_shader(
    contract: ResidentEncodeAdapterContract,
    row_pitch: u32,
    chroma_offset: u32,
) -> String {
    let (kr, kb) = match contract.colorimetry {
        mondrian_media::ResidentEncodeColorimetry::Rec709 => (0.2126_f32, 0.0722_f32),
        mondrian_media::ResidentEncodeColorimetry::Rec2100Pq
        | mondrian_media::ResidentEncodeColorimetry::Rec2100Hlg => (0.2627_f32, 0.0593_f32),
    };
    let (y_offset, y_scale, c_offset, c_scale, max_code, shift) =
        match (contract.bit_depth, contract.full_range) {
            (ResidentEncodeBitDepth::Eight, false) => (16.0, 219.0, 128.0, 224.0, 255.0, 0),
            (ResidentEncodeBitDepth::Eight, true) => (0.0, 255.0, 127.5, 255.0, 255.0, 0),
            (ResidentEncodeBitDepth::Ten, false) => (64.0, 876.0, 512.0, 896.0, 1023.0, 6),
            (ResidentEncodeBitDepth::Ten, true) => (0.0, 1023.0, 511.5, 1023.0, 1023.0, 6),
        };
    let header = format!(
        "@group(0) @binding(0) var source: texture_2d<f32>;\n\
         @group(0) @binding(1) var<storage, read_write> output: array<u32>;\n\
         const WIDTH: u32 = {width}u;\nconst HEIGHT: u32 = {height}u;\n\
         const ROW_WORDS: u32 = {row_words}u;\nconst CHROMA_WORD: u32 = {chroma_word}u;\n\
         const KR: f32 = {kr};\nconst KB: f32 = {kb};\nconst KG: f32 = 1.0 - KR - KB;\n\
         const Y_OFFSET: f32 = {y_offset};\nconst Y_SCALE: f32 = {y_scale};\n\
         const C_OFFSET: f32 = {c_offset};\nconst C_SCALE: f32 = {c_scale};\n\
         const MAX_CODE: f32 = {max_code};\n\
         fn rgb(x: u32, y: u32) -> vec3<f32> {{\n  return clamp(textureLoad(source, vec2<i32>(i32(min(x, WIDTH - 1u)), i32(min(y, HEIGHT - 1u))), 0).rgb, vec3<f32>(0.0), vec3<f32>(1.0));\n}}\n\
         fn yuv(value: vec3<f32>) -> vec3<f32> {{\n  let y = dot(value, vec3<f32>(KR, KG, KB));\n  let cb = (value.b - y) / (2.0 * (1.0 - KB));\n  let cr = (value.r - y) / (2.0 * (1.0 - KR));\n  return vec3<f32>(y, cb, cr);\n}}\n\
         fn quant_y(value: f32) -> u32 {{ return u32(round(clamp(Y_OFFSET + Y_SCALE * value, 0.0, MAX_CODE))); }}\n\
         fn quant_c(value: f32) -> u32 {{ return u32(round(clamp(C_OFFSET + C_SCALE * value, 0.0, MAX_CODE))); }}\n",
        width = contract.width,
        height = contract.height,
        row_words = row_pitch / 4,
        chroma_word = chroma_offset / 4,
    );
    let body = match (contract.bit_depth, contract.chroma_location) {
        (ResidentEncodeBitDepth::Eight, mondrian_media::ResidentEncodeChromaLocation::Center) => {
            r#"
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let x = gid.x * 4u;
  let y0 = gid.y * 2u;
  if (x >= WIDTH || y0 >= HEIGHT) { return; }
  let y1 = min(y0 + 1u, HEIGHT - 1u);
  let a0 = yuv(rgb(x, y0)); let b0 = yuv(rgb(x + 1u, y0));
  let c0 = yuv(rgb(x + 2u, y0)); let d0 = yuv(rgb(x + 3u, y0));
  let a1 = yuv(rgb(x, y1)); let b1 = yuv(rgb(x + 1u, y1));
  let c1 = yuv(rgb(x + 2u, y1)); let d1 = yuv(rgb(x + 3u, y1));
  output[y0 * ROW_WORDS + x / 4u] = quant_y(a0.x) | (quant_y(b0.x) << 8u) | (quant_y(c0.x) << 16u) | (quant_y(d0.x) << 24u);
  output[y1 * ROW_WORDS + x / 4u] = quant_y(a1.x) | (quant_y(b1.x) << 8u) | (quant_y(c1.x) << 16u) | (quant_y(d1.x) << 24u);
  let uv0 = (a0.yz + b0.yz + a1.yz + b1.yz) * 0.25;
  let uv1 = (c0.yz + d0.yz + c1.yz + d1.yz) * 0.25;
  output[CHROMA_WORD + (y0 / 2u) * ROW_WORDS + x / 4u] = quant_c(uv0.x) | (quant_c(uv0.y) << 8u) | (quant_c(uv1.x) << 16u) | (quant_c(uv1.y) << 24u);
}
"#
        }
        (ResidentEncodeBitDepth::Ten, mondrian_media::ResidentEncodeChromaLocation::Center) => {
            r#"
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let x = gid.x * 2u;
  let y0 = gid.y * 2u;
  if (x >= WIDTH || y0 >= HEIGHT) { return; }
  let y1 = min(y0 + 1u, HEIGHT - 1u);
  let a0 = yuv(rgb(x, y0)); let b0 = yuv(rgb(x + 1u, y0));
  let a1 = yuv(rgb(x, y1)); let b1 = yuv(rgb(x + 1u, y1));
  output[y0 * ROW_WORDS + x / 2u] = (quant_y(a0.x) << 6u) | (quant_y(b0.x) << 22u);
  output[y1 * ROW_WORDS + x / 2u] = (quant_y(a1.x) << 6u) | (quant_y(b1.x) << 22u);
  let uv = (a0.yz + b0.yz + a1.yz + b1.yz) * 0.25;
  output[CHROMA_WORD + (y0 / 2u) * ROW_WORDS + x / 2u] = (quant_c(uv.x) << 6u) | (quant_c(uv.y) << 22u);
}
"#
        }
        (ResidentEncodeBitDepth::Eight, mondrian_media::ResidentEncodeChromaLocation::Left) => {
            r#"
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let x = gid.x * 4u;
  let y0 = gid.y * 2u;
  if (x >= WIDTH || y0 >= HEIGHT) { return; }
  let y1 = min(y0 + 1u, HEIGHT - 1u);
  let left_x = max(x, 1u) - 1u;
  let p0 = yuv(rgb(left_x, y0)); let a0 = yuv(rgb(x, y0)); let b0 = yuv(rgb(x + 1u, y0));
  let c0 = yuv(rgb(x + 2u, y0)); let d0 = yuv(rgb(x + 3u, y0));
  let p1 = yuv(rgb(left_x, y1)); let a1 = yuv(rgb(x, y1)); let b1 = yuv(rgb(x + 1u, y1));
  let c1 = yuv(rgb(x + 2u, y1)); let d1 = yuv(rgb(x + 3u, y1));
  output[y0 * ROW_WORDS + x / 4u] = quant_y(a0.x) | (quant_y(b0.x) << 8u) | (quant_y(c0.x) << 16u) | (quant_y(d0.x) << 24u);
  output[y1 * ROW_WORDS + x / 4u] = quant_y(a1.x) | (quant_y(b1.x) << 8u) | (quant_y(c1.x) << 16u) | (quant_y(d1.x) << 24u);
  let uv0 = (p0.yz + 2.0 * a0.yz + b0.yz + p1.yz + 2.0 * a1.yz + b1.yz) * 0.125;
  let uv1 = (b0.yz + 2.0 * c0.yz + d0.yz + b1.yz + 2.0 * c1.yz + d1.yz) * 0.125;
  output[CHROMA_WORD + (y0 / 2u) * ROW_WORDS + x / 4u] = quant_c(uv0.x) | (quant_c(uv0.y) << 8u) | (quant_c(uv1.x) << 16u) | (quant_c(uv1.y) << 24u);
}
"#
        }
        (ResidentEncodeBitDepth::Ten, mondrian_media::ResidentEncodeChromaLocation::Left) => {
            r#"
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let x = gid.x * 2u;
  let y0 = gid.y * 2u;
  if (x >= WIDTH || y0 >= HEIGHT) { return; }
  let y1 = min(y0 + 1u, HEIGHT - 1u);
  let left_x = max(x, 1u) - 1u;
  let p0 = yuv(rgb(left_x, y0)); let a0 = yuv(rgb(x, y0)); let b0 = yuv(rgb(x + 1u, y0));
  let p1 = yuv(rgb(left_x, y1)); let a1 = yuv(rgb(x, y1)); let b1 = yuv(rgb(x + 1u, y1));
  output[y0 * ROW_WORDS + x / 2u] = (quant_y(a0.x) << 6u) | (quant_y(b0.x) << 22u);
  output[y1 * ROW_WORDS + x / 2u] = (quant_y(a1.x) << 6u) | (quant_y(b1.x) << 22u);
  let uv = (p0.yz + 2.0 * a0.yz + b0.yz + p1.yz + 2.0 * a1.yz + b1.yz) * 0.125;
  output[CHROMA_WORD + (y0 / 2u) * ROW_WORDS + x / 2u] = (quant_c(uv.x) << 6u) | (quant_c(uv.y) << 22u);
}
"#
        }
    };
    let _ = shift;
    header + body
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_media::ResidentEncodeColorimetry;

    fn contract(
        bit_depth: ResidentEncodeBitDepth,
        full_range: bool,
    ) -> ResidentEncodeAdapterContract {
        ResidentEncodeAdapterContract {
            width: 1920,
            height: 1080,
            frame_rate_num: 30_000,
            frame_rate_den: 1_001,
            bit_depth,
            colorimetry: ResidentEncodeColorimetry::Rec709,
            full_range,
            chroma_location: mondrian_media::ResidentEncodeChromaLocation::Left,
            max_frames_in_flight: 3,
        }
    }

    #[test]
    fn shaders_encode_exact_legal_and_full_range_constants() {
        let legal = conversion_shader(
            contract(ResidentEncodeBitDepth::Eight, false),
            2048,
            2048 * 1080,
        );
        assert!(legal.contains("const Y_OFFSET: f32 = 16"));
        assert!(legal.contains("const Y_SCALE: f32 = 219"));
        assert!(legal.contains("const C_SCALE: f32 = 224"));
        let full = conversion_shader(
            contract(ResidentEncodeBitDepth::Ten, true),
            4096,
            4096 * 1080,
        );
        assert!(full.contains("const Y_SCALE: f32 = 1023"));
        assert!(full.contains("const C_OFFSET: f32 = 511.5"));
        assert!(full.contains("<< 22u"));
        assert!(full.contains("let left_x = max(x, 1u) - 1u"));
        assert!(full.contains("2.0 * a0.yz"));
        assert!(full.contains("* 0.125"));

        let centered = conversion_shader(
            ResidentEncodeAdapterContract {
                chroma_location: mondrian_media::ResidentEncodeChromaLocation::Center,
                ..contract(ResidentEncodeBitDepth::Eight, false)
            },
            2048,
            2048 * 1080,
        );
        assert!(centered.contains("(a0.yz + b0.yz + a1.yz + b1.yz) * 0.25"));
    }
}
