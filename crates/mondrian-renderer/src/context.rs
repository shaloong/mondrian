//! wgpu GPU 上下文初始化

use mondrian_core::Result;
use std::sync::Arc;
use wgpu;

pub struct GpuContext {
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    pub adapter: wgpu::Adapter,
}

impl GpuContext {
    /// Create a GpuContext from an existing wgpu device and queue.
    /// Used when sharing the eframe-created device.
    pub fn from_device_queue(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        adapter: wgpu::Adapter,
    ) -> Arc<Self> {
        Arc::new(Self { device, queue, adapter })
    }

    /// Creates a new independent GPU context (standalone device).
    /// Used when no external device is available (e.g., tests, export).
    pub async fn new() -> Result<Arc<Self>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
        });

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|e| mondrian_core::MondrianError::GpuInitFailed {
                reason: format!("找不到合适的 GPU 适配器: {e}"),
            })?;

        tracing::info!("GPU Adapter: {:?}", adapter.get_info());

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .map_err(|e| mondrian_core::MondrianError::GpuInitFailed { reason: e.to_string() })?;

        Ok(Arc::new(Self {
            device: Arc::new(device),
            queue: Arc::new(queue),
            adapter,
        }))
    }
}
