//! Immutable physical-memory facts for the active renderer device generation.

/// Device-local memory capacity reported by the exact active GPU generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GpuDeviceMemoryCapacity {
    device_local_bytes: u64,
}

impl GpuDeviceMemoryCapacity {
    /// Bytes in the primary device-local Vulkan heap, excluding a separately
    /// mapped host-visible aperture when the driver exposes one.
    pub const fn device_local_bytes(self) -> u64 {
        self.device_local_bytes
    }
}

/// Query immutable physical-memory capacity from the exact active wgpu Device.
///
/// Backends without a native capacity query return `None`; callers must retain
/// that absence instead of inventing a capacity from system memory.
pub fn query_gpu_device_memory_capacity(device: &wgpu::Device) -> Option<GpuDeviceMemoryCapacity> {
    query_backend_gpu_device_memory_capacity(device)
}

#[cfg(target_os = "linux")]
fn query_backend_gpu_device_memory_capacity(
    device: &wgpu::Device,
) -> Option<GpuDeviceMemoryCapacity> {
    // SAFETY: the HAL borrow belongs to this exact public Device. The query
    // reads immutable physical-device heap properties and no native handle
    // escapes this function.
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }?;
    let instance = hal.shared_instance().raw_instance();
    let physical = hal.raw_physical_device();
    // SAFETY: `physical` belongs to the live instance retained by `hal`.
    let properties = unsafe { instance.get_physical_device_memory_properties(physical) };
    let device_local_bytes = primary_device_local_capacity(&properties);
    (device_local_bytes > 0).then_some(GpuDeviceMemoryCapacity { device_local_bytes })
}

#[cfg(target_os = "linux")]
fn primary_device_local_capacity(properties: &ash::vk::PhysicalDeviceMemoryProperties) -> u64 {
    use ash::vk;

    let mut primary_heaps = [false; vk::MAX_MEMORY_HEAPS];
    for memory_type in &properties.memory_types[..properties.memory_type_count as usize] {
        if memory_type.property_flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            && !memory_type.property_flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
        {
            primary_heaps[memory_type.heap_index as usize] = true;
        }
    }
    let heaps = &properties.memory_heaps[..properties.memory_heap_count as usize];
    let preferred = heaps
        .iter()
        .enumerate()
        .filter(|(index, heap)| {
            primary_heaps[*index] && heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL)
        })
        .map(|(_, heap)| heap.size)
        .max();
    preferred.unwrap_or_else(|| {
        heaps
            .iter()
            .filter(|heap| heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
            .map(|heap| heap.size)
            .max()
            .unwrap_or(0)
    })
}

#[cfg(target_os = "windows")]
fn query_backend_gpu_device_memory_capacity(
    device: &wgpu::Device,
) -> Option<GpuDeviceMemoryCapacity> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ERROR_NOT_FOUND};

    // SAFETY: the HAL guard retains the exact public DX12 Device for this
    // immutable identity query; no native handle escapes the function.
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }?;
    // SAFETY: the HAL guard keeps the raw D3D12 device alive for this POD query.
    let target_luid = luid_as_u64(unsafe { hal.raw_device().GetAdapterLuid() });
    // SAFETY: the returned COM factory remains owned for this enumeration.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    for index in 0..u32::MAX {
        // SAFETY: the live factory returns an owned adapter reference.
        let adapter = match unsafe { factory.EnumAdapters1(index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(_) => return None,
        };
        // SAFETY: the owned adapter remains live for this descriptor query.
        let descriptor = unsafe { adapter.GetDesc1() }.ok()?;
        if luid_as_u64(descriptor.AdapterLuid) == target_luid {
            let device_local_bytes = u64::try_from(descriptor.DedicatedVideoMemory).ok()?;
            return (device_local_bytes > 0)
                .then_some(GpuDeviceMemoryCapacity { device_local_bytes });
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn luid_as_u64(value: windows::Win32::Foundation::LUID) -> u64 {
    (u64::from(value.HighPart as u32) << 32) | u64::from(value.LowPart)
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn query_backend_gpu_device_memory_capacity(
    _device: &wgpu::Device,
) -> Option<GpuDeviceMemoryCapacity> {
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{primary_device_local_capacity, query_gpu_device_memory_capacity};
    use ash::vk;

    #[test]
    fn mapped_device_local_heap_does_not_double_count_discrete_render_memory() {
        let mut properties = vk::PhysicalDeviceMemoryProperties {
            memory_heap_count: 3,
            memory_type_count: 2,
            ..Default::default()
        };
        properties.memory_heaps[0] = vk::MemoryHeap {
            size: 2 * 1024 * 1024 * 1024,
            flags: vk::MemoryHeapFlags::DEVICE_LOCAL,
        };
        properties.memory_heaps[1] = vk::MemoryHeap {
            size: 12 * 1024 * 1024 * 1024,
            flags: vk::MemoryHeapFlags::empty(),
        };
        properties.memory_heaps[2] = vk::MemoryHeap {
            size: 246 * 1024 * 1024,
            flags: vk::MemoryHeapFlags::DEVICE_LOCAL,
        };
        properties.memory_types[0] = vk::MemoryType {
            property_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            heap_index: 0,
        };
        properties.memory_types[1] = vk::MemoryType {
            property_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL
                | vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            heap_index: 2,
        };

        assert_eq!(
            primary_device_local_capacity(&properties),
            2 * 1024 * 1024 * 1024
        );
    }

    #[test]
    #[ignore = "manual Linux Vulkan qualification; requires a physical GPU adapter"]
    fn active_vulkan_device_reports_physical_device_local_capacity() {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            ..wgpu::RequestAdapterOptions::default()
        }))
        .expect("physical Linux GPU adapter");
        assert_eq!(adapter.get_info().backend, wgpu::Backend::Vulkan);
        let (device, _queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("Vulkan Device");

        let capacity =
            query_gpu_device_memory_capacity(&device).expect("device-local Vulkan heap capacity");
        assert!(capacity.device_local_bytes() >= 128 * 1024 * 1024);
        eprintln!(
            "device_local_memory_bytes={}",
            capacity.device_local_bytes()
        );
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use super::query_gpu_device_memory_capacity;

    #[test]
    #[ignore = "manual Windows DX12 qualification; requires a physical GPU adapter"]
    fn active_dx12_device_reports_dedicated_video_memory() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            ..wgpu::RequestAdapterOptions::default()
        }))
        .expect("physical Windows GPU adapter");
        assert_eq!(adapter.get_info().backend, wgpu::Backend::Dx12);
        let (device, _queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("DX12 Device");

        let capacity = query_gpu_device_memory_capacity(&device)
            .expect("dedicated DXGI video-memory capacity");
        assert!(capacity.device_local_bytes() >= 128 * 1024 * 1024);
        eprintln!(
            "device_local_memory_bytes={}",
            capacity.device_local_bytes()
        );
    }
}
