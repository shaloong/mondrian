//! wgpu GPU 上下文初始化

use mondrian_core::Result;
use std::sync::Arc;
use wgpu;

pub struct GpuContext {
    pub device:  Arc<wgpu::Device>,
    pub queue:   Arc<wgpu::Queue>,
    pub adapter: wgpu::Adapter,
}

impl GpuContext {
    /// 异步初始化 GPU 上下文（选择最优适配器）
    pub async fn new() -> Result<Arc<Self>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| mondrian_core::MondrianError::GpuInitFailed {
                reason: "找不到合适的 GPU 适配器".to_string(),
            })?;

        tracing::info!("GPU Adapter: {:?}", adapter.get_info());

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default(), None)
            .await
            .map_err(|e| mondrian_core::MondrianError::GpuInitFailed {
                reason: e.to_string(),
            })?;

        Ok(Arc::new(Self {
            device: Arc::new(device),
            queue:  Arc::new(queue),
            adapter,
        }))
    }
}
