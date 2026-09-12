//! CUDA -> Vulkan storage-buffer bridge. One GPU pixel copy, no CPU pixels.
use super::cuda_driver::{self as cu, CudaDriver};
use super::direct_backend::{DirectNativeBufferSynchronization, DirectNativeYuvBuffer};
use crate::{
    GpuNativeDecodedFrameImportError, GpuNativeDecodedFrameImportSupport,
    GpuNativeDecodedFrameTextureFormat,
};
use ash::vk;
use mondrian_media::{
    DecodedGpuFrameHandleKind, FfmpegNativeDecodedFrameResource, PreviewNativeDecodedFrame,
    PreviewNativeDecodedFrameHandle,
};
use std::ffi::c_void;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn rejected(reason: impl ToString) -> GpuNativeDecodedFrameImportError {
    GpuNativeDecodedFrameImportError::BackendRejected { reason: reason.to_string() }
}

// This is a declared allocation policy, not an assumed driver requirement.
// vkGetBufferMemoryRequirements must fit it before any memory is allocated.
pub(crate) fn cuda_buffer_layout(
    width: u32,
    height: u32,
    component_bytes: u32,
) -> Option<(u32, u32, u64)> {
    if width == 0 || height == 0 || !matches!(component_bytes, 1 | 2) {
        return None;
    }
    let row = width.div_ceil(2).checked_mul(2)?.checked_mul(component_bytes)?;
    let row = row.checked_add(255)? & !255;
    let offset = row.checked_mul(height)?;
    let bytes = u64::from(offset).checked_add(u64::from(row) * u64::from(height.div_ceil(2)))?;
    let capacity = bytes.checked_add(65535)? & !65535;
    if capacity > u64::from(u32::MAX) {
        return None;
    }
    Some((row, offset, capacity))
}

pub(super) struct CudaPlaneAdapter {
    pub support: GpuNativeDecodedFrameImportSupport,
    driver: Arc<CudaDriver>,
    raw: ash::Device,
    memory_fd: ash::khr::external_memory_fd::Device,
    semaphore_fd: ash::khr::external_semaphore_fd::Device,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    family: u32,
    uuid: [u8; 16],
    dedicated: bool,
    owners: Arc<AtomicUsize>,
}
impl CudaPlaneAdapter {
    pub fn new(device: &wgpu::Device) -> Result<Self, GpuNativeDecodedFrameImportError> {
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }
            .ok_or_else(|| rejected("CUDA import requires Vulkan"))?;
        let required = [
            ash::khr::external_memory_fd::NAME,
            ash::khr::external_semaphore_fd::NAME,
        ];
        if !required.iter().all(|name| hal.enabled_device_extensions().contains(name)) {
            return Err(rejected(
                "Vulkan device lacks CUDA external-memory/semaphore FD extensions",
            ));
        }
        let instance = hal.shared_instance().raw_instance();
        let physical = hal.raw_physical_device();
        let mut id = vk::PhysicalDeviceIDProperties::default();
        let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
        unsafe { instance.get_physical_device_properties2(physical, &mut props) };
        let driver = CudaDriver::load().map_err(rejected)?;
        let ordinal = u16::try_from(driver.matching_ordinal(id.device_uuid).map_err(rejected)?)
            .map_err(|_| rejected("CUDA ordinal exceeds the device-selector contract"))?;
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
            return Err(rejected(
                "Vulkan storage-buffer memory is not exportable as OPAQUE_FD",
            ));
        }
        let sem_info = vk::PhysicalDeviceExternalSemaphoreInfo::default()
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD);
        let mut sem_props = vk::ExternalSemaphoreProperties::default();
        unsafe {
            instance.get_physical_device_external_semaphore_properties(
                physical,
                &sem_info,
                &mut sem_props,
            )
        };
        if !sem_props
            .external_semaphore_features
            .contains(vk::ExternalSemaphoreFeatureFlags::EXPORTABLE)
        {
            return Err(rejected("Vulkan semaphore cannot be exported to CUDA"));
        }
        let raw = hal.raw_device().clone();
        Ok(Self {
            support: GpuNativeDecodedFrameImportSupport::ready_gpu_bridge_copy(
                vec![DecodedGpuFrameHandleKind::CudaDeviceMemory],
                vec![
                    GpuNativeDecodedFrameTextureFormat::Nv12,
                    GpuNativeDecodedFrameTextureFormat::P010,
                ],
            )
            .with_renderer_backend_label("wgpu Vulkan CUDA buffer bridge + shared YUV/OCIO")
            .with_hardware_decode_device_selector(
                mondrian_media::HwAccelDeviceSelector::CudaDeviceOrdinal(ordinal),
            ),
            memory_fd: ash::khr::external_memory_fd::Device::new(instance, &raw),
            semaphore_fd: ash::khr::external_semaphore_fd::Device::new(instance, &raw),
            memory_properties: unsafe { instance.get_physical_device_memory_properties(physical) },
            raw,
            driver,
            family: hal.queue_family_index(),
            uuid: id.device_uuid,
            dedicated: external
                .external_memory_properties
                .external_memory_features
                .contains(vk::ExternalMemoryFeatureFlags::DEDICATED_ONLY),
            owners: Arc::new(AtomicUsize::new(0)),
        })
    }
    pub fn retained_owners(&self) -> usize {
        self.owners.load(Ordering::Acquire)
    }

    pub fn import(
        &self,
        device: &wgpu::Device,
        native: &PreviewNativeDecodedFrame,
    ) -> Result<DirectNativeYuvBuffer, GpuNativeDecodedFrameImportError> {
        let resource = native
            .handle
            .resource::<FfmpegNativeDecodedFrameResource>()
            .ok_or_else(|| rejected("CUDA frame lacks retained FFmpeg ownership"))?;
        let view = resource.cuda_frame().map_err(rejected)?;
        if (view.width, view.height, view.format)
            != (native.width, native.height, native.surface_format)
        {
            return Err(rejected(
                "CUDA physical surface differs from the admitted native frame",
            ));
        }
        let component = if view.format == mondrian_media::DecodedVideoSurfaceFormat::P010 {
            2
        } else {
            1
        };
        let (row, offset, capacity) = cuda_buffer_layout(view.width, view.height, component)
            .ok_or_else(|| rejected("CUDA buffer extent overflow"))?;
        if capacity > device.limits().max_storage_buffer_binding_size
            || capacity > device.limits().max_buffer_size
        {
            return Err(rejected(
                "CUDA buffer exceeds the active renderer's storage limits",
            ));
        }
        self.owners
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map_err(|_| rejected("CUDA transfer owner counter exhausted"))?;
        let mut owner = TransferOwner {
            raw: self.raw.clone(),
            driver: Arc::clone(&self.driver),
            context: view.context,
            stream: std::ptr::null_mut(),
            event: std::ptr::null_mut(),
            cuda_memory: std::ptr::null_mut(),
            cuda_semaphore: std::ptr::null_mut(),
            mapped: 0,
            buffer: vk::Buffer::null(),
            memory: vk::DeviceMemory::null(),
            semaphore: vk::Semaphore::null(),
            _source: native.handle.clone(),
            owners: Arc::clone(&self.owners),
        };
        let mut external = vk::ExternalMemoryBufferCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let info = vk::BufferCreateInfo::default()
            .size(capacity)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external);
        owner.buffer = unsafe { self.raw.create_buffer(&info, None) }.map_err(rejected)?;
        let requirements = unsafe { self.raw.get_buffer_memory_requirements(owner.buffer) };
        if requirements.size > capacity {
            return Err(rejected(
                "native buffer allocation exceeds its admitted byte capacity",
            ));
        }
        let memory_type = (0..self.memory_properties.memory_type_count)
            .find(|index| {
                requirements.memory_type_bits & (1 << index) != 0
                    && self.memory_properties.memory_types[*index as usize]
                        .property_flags
                        .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            })
            .ok_or_else(|| rejected("no device-local CUDA bridge memory type"))?;
        let mut export = vk::ExportMemoryAllocateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().buffer(owner.buffer);
        let mut allocate = vk::MemoryAllocateInfo::default()
            .allocation_size(capacity)
            .memory_type_index(memory_type)
            .push_next(&mut export);
        if self.dedicated {
            allocate = allocate.push_next(&mut dedicated);
        }
        owner.memory = unsafe { self.raw.allocate_memory(&allocate, None) }.map_err(rejected)?;
        unsafe { self.raw.bind_buffer_memory(owner.buffer, owner.memory, 0) }.map_err(rejected)?;
        let fd = unsafe {
            self.memory_fd.get_memory_fd(
                &vk::MemoryGetFdInfoKHR::default()
                    .memory(owner.memory)
                    .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD),
            )
        }
        .map_err(rejected)?;
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let guard = unsafe { self.driver.enter(view.context) }.map_err(rejected)?;
        self.driver.require_current_uuid(self.uuid).map_err(rejected)?;
        let descriptor = cu::MemoryHandle {
            kind: 1,
            handle: cu::ExternalHandle { fd: std::os::fd::AsRawFd::as_raw_fd(&fd) },
            size: capacity,
            flags: u32::from(self.dedicated),
            reserved: [0; 16],
        };
        cu::check("cuImportExternalMemory", unsafe {
            (self.driver.import_memory)(&mut owner.cuda_memory, &descriptor)
        })
        .map_err(rejected)?;
        let _transferred_fd = fd.into_raw_fd(); // CUDA consumes the fd on success only.
        cu::check("cuExternalMemoryGetMappedBuffer", unsafe {
            (self.driver.map_buffer)(
                &mut owner.mapped,
                owner.cuda_memory,
                &cu::BufferDesc {
                    offset: 0,
                    size: capacity,
                    flags: 0,
                    reserved: [0; 16],
                },
            )
        })
        .map_err(rejected)?;
        let mut export_sem = vk::ExportSemaphoreCreateInfo::default()
            .handle_types(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD);
        owner.semaphore = unsafe {
            self.raw.create_semaphore(
                &vk::SemaphoreCreateInfo::default().push_next(&mut export_sem),
                None,
            )
        }
        .map_err(rejected)?;
        let fd = unsafe {
            self.semaphore_fd.get_semaphore_fd(
                &vk::SemaphoreGetFdInfoKHR::default()
                    .semaphore(owner.semaphore)
                    .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD),
            )
        }
        .map_err(rejected)?;
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let descriptor = cu::SemaphoreHandle {
            kind: 1,
            handle: cu::ExternalHandle { fd: std::os::fd::AsRawFd::as_raw_fd(&fd) },
            flags: 0,
            reserved: [0; 16],
        };
        cu::check("cuImportExternalSemaphore", unsafe {
            (self.driver.import_semaphore)(&mut owner.cuda_semaphore, &descriptor)
        })
        .map_err(rejected)?;
        let _transferred_fd = fd.into_raw_fd();
        cu::check("cuStreamCreate", unsafe {
            (self.driver.stream_create)(&mut owner.stream, 1)
        })
        .map_err(rejected)?;
        cu::check("cuEventCreate", unsafe {
            (self.driver.event_create)(&mut owner.event, 2)
        })
        .map_err(rejected)?;
        cu::check("cuEventRecord", unsafe {
            (self.driver.event_record)(owner.event, view.stream)
        })
        .map_err(rejected)?;
        cu::check("cuStreamWaitEvent", unsafe {
            (self.driver.stream_wait_event)(owner.stream, owner.event, 0)
        })
        .map_err(rejected)?;
        // Initialize allocation padding as required by wgpu's imported-buffer contract.
        cu::check("cuMemsetD8Async", unsafe {
            (self.driver.memset)(owner.mapped, 0, capacity as usize, owner.stream)
        })
        .map_err(rejected)?;
        for plane in 0..2 {
            let columns = if plane == 0 {
                view.width
            } else {
                view.width.div_ceil(2) * 2
            };
            let copy = cu::Copy2d {
                src_x: 0,
                src_y: 0,
                src_kind: 2,
                src_host: std::ptr::null(),
                src_device: view.planes[plane].0,
                src_array: std::ptr::null_mut(),
                src_pitch: view.planes[plane].1,
                dst_x: 0,
                dst_y: 0,
                dst_kind: 2,
                dst_host: std::ptr::null_mut(),
                dst_device: owner.mapped + if plane == 0 { 0 } else { offset as usize },
                dst_array: std::ptr::null_mut(),
                dst_pitch: row as usize,
                width_bytes: columns as usize * component as usize,
                height: if plane == 0 {
                    view.height as usize
                } else {
                    view.height.div_ceil(2) as usize
                },
            };
            cu::check("cuMemcpy2DAsync", unsafe {
                (self.driver.copy_2d)(&copy, owner.stream)
            })
            .map_err(rejected)?;
        }
        cu::check("cuSignalExternalSemaphoresAsync", unsafe {
            (self.driver.signal)(
                &owner.cuda_semaphore,
                &cu::SignalParams::default(),
                1,
                owner.stream,
            )
        })
        .map_err(rejected)?;
        guard.finish().map_err(rejected)?;
        let owner = Arc::new(owner);
        let retained = Arc::clone(&owner);
        // SAFETY: exact active device, fully initialized storage after its staged
        // semaphore wait; the callback consumes native handles after last GPU use.
        let hal_buffer = unsafe {
            wgpu::hal::vulkan::Buffer::from_raw_externally_owned(
                owner.buffer,
                Box::new(move || drop(retained)),
            )
        };
        let buffer = unsafe {
            device.create_buffer_from_hal::<wgpu::hal::api::Vulkan>(
                hal_buffer,
                &wgpu::BufferDescriptor {
                    label: Some("mondrian.cuda-native-yuv"),
                    size: capacity,
                    usage: wgpu::BufferUsages::STORAGE,
                    mapped_at_creation: false,
                },
            )
        };
        Ok(DirectNativeYuvBuffer {
            buffer,
            row_pitch: row,
            chroma_offset: offset,
            synchronization: Box::new(TransferSubmission { owner, family: self.family }),
        })
    }
}

struct TransferOwner {
    raw: ash::Device,
    driver: Arc<CudaDriver>,
    context: *mut c_void,
    stream: *mut c_void,
    event: *mut c_void,
    cuda_memory: *mut c_void,
    cuda_semaphore: *mut c_void,
    mapped: usize,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    semaphore: vk::Semaphore,
    _source: PreviewNativeDecodedFrameHandle,
    owners: Arc<AtomicUsize>,
}
// SAFETY: publication occurs only after immutable initialization. The final
// owner enters its retained FFmpeg CUDA context on the dropping thread. Native
// work is stream-ordered and Vulkan use retains this owner through completion.
unsafe impl Send for TransferOwner {}
unsafe impl Sync for TransferOwner {}
impl Drop for TransferOwner {
    fn drop(&mut self) {
        let mut failed = false;
        match unsafe { self.driver.enter(self.context) } {
            Ok(guard) => {
                let mut release = |name, code| {
                    if code != 0 {
                        failed = true;
                        tracing::error!(operation = name, code, "CUDA bridge cleanup failed");
                    }
                };
                unsafe {
                    if !self.stream.is_null() {
                        release("stream synchronize", (self.driver.stream_sync)(self.stream));
                    }
                    if !self.cuda_semaphore.is_null() {
                        release(
                            "semaphore destroy",
                            (self.driver.destroy_semaphore)(self.cuda_semaphore),
                        );
                    }
                    if self.mapped != 0 {
                        release("mapped buffer free", (self.driver.free)(self.mapped));
                    }
                    if !self.cuda_memory.is_null() {
                        release(
                            "external memory destroy",
                            (self.driver.destroy_memory)(self.cuda_memory),
                        );
                    }
                    if !self.event.is_null() {
                        release("event destroy", (self.driver.event_destroy)(self.event));
                    }
                    if !self.stream.is_null() {
                        release("stream destroy", (self.driver.stream_destroy)(self.stream));
                    }
                }
                if let Err(error) = guard.finish() {
                    failed = true;
                    tracing::error!(%error,"CUDA cleanup context restoration failed");
                }
            }
            Err(error) => {
                failed = true;
                tracing::error!(%error,"CUDA cleanup context unavailable");
            }
        }
        unsafe {
            if self.semaphore != vk::Semaphore::null() {
                self.raw.destroy_semaphore(self.semaphore, None);
            }
            if self.buffer != vk::Buffer::null() {
                self.raw.destroy_buffer(self.buffer, None);
            }
            if self.memory != vk::DeviceMemory::null() {
                self.raw.free_memory(self.memory, None);
            }
        }
        // Failed cleanup remains an unclosed owner in the runtime's closure evidence.
        if !failed {
            self.owners.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
struct TransferSubmission {
    owner: Arc<TransferOwner>,
    family: u32,
}
impl TransferSubmission {
    fn record_acquire(
        &self,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), GpuNativeDecodedFrameImportError> {
        // SAFETY: exclusive command-encoder borrow, exact device and private buffer.
        unsafe {
            encoder.as_hal_mut::<wgpu::hal::api::Vulkan, _, _>(|hal| {
                let hal = hal.ok_or_else(|| rejected("CUDA bridge encoder is not Vulkan"))?;
                let barrier = vk::BufferMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::empty())
                    .dst_access_mask(vk::AccessFlags::SHADER_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
                    .dst_queue_family_index(self.family)
                    .buffer(self.owner.buffer)
                    .offset(0)
                    .size(vk::WHOLE_SIZE);
                self.owner.raw.cmd_pipeline_barrier(
                    hal.raw_handle(),
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[barrier],
                    &[],
                );
                Ok(())
            })
        }
    }
}
impl DirectNativeBufferSynchronization for TransferSubmission {
    fn submit(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        command: wgpu::CommandBuffer,
    ) -> Result<(), GpuNativeDecodedFrameImportError> {
        let mut acquire = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.cuda-native-acquire"),
        });
        self.record_acquire(&mut acquire)?;
        let acquire = acquire.finish();
        let hal = unsafe { queue.as_hal::<wgpu::hal::api::Vulkan>() }
            .ok_or_else(|| rejected("CUDA bridge queue is not Vulkan"))?;
        hal.add_wait_semaphore(
            self.owner.semaphore,
            None,
            vk::PipelineStageFlags::TOP_OF_PIPE,
        );
        drop(hal);
        let _pending = PendingWait {
            queue: queue.clone(),
            owner: Arc::clone(&self.owner),
        };
        queue.submit([acquire, command]);
        Ok(())
    }
}
struct PendingWait {
    queue: wgpu::Queue,
    owner: Arc<TransferOwner>,
}
impl Drop for PendingWait {
    fn drop(&mut self) {
        let removed = unsafe { self.queue.as_hal::<wgpu::hal::api::Vulkan>() }
            .is_some_and(|hal| hal.remove_wait_semaphore(self.owner.semaphore));
        if !removed {
            // Another concurrent submit may have consumed the wait. Preserve its
            // semaphore owner until actual queue completion, including unwind.
            let owner = Arc::clone(&self.owner);
            self.queue.on_submitted_work_done(move || drop(owner));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cuda_buffer_layout;

    #[test]
    fn transfer_storage_charges_both_planes_and_alignment() {
        assert_eq!(cuda_buffer_layout(5, 3, 1), Some((256, 768, 65536)));
        assert_eq!(
            cuda_buffer_layout(3840, 2160, 2),
            Some((7680, 16_588_800, 24_903_680))
        );
        for (width, height, bytes) in [
            (0, 1, 1),
            (1, 0, 1),
            (1, 1, 3),
            (u32::MAX, 1, 2),
            (1, u32::MAX, 1),
            (65536, 65536, 2),
        ] {
            assert_eq!(cuda_buffer_layout(width, height, bytes), None);
        }
    }
}
