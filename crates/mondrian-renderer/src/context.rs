//! wgpu GPU 上下文初始化

use mondrian_core::Result;
use std::sync::Arc;

/// Optional wgpu features required to sample native NV12/P010 video textures.
///
/// Only features advertised by the selected adapter are returned, so callers
/// can add the result to `DeviceDescriptor::required_features` without turning
/// an unsupported native-video format into device creation failure.
pub fn native_video_texture_device_features(adapter_features: wgpu::Features) -> wgpu::Features {
    adapter_features & (wgpu::Features::TEXTURE_FORMAT_NV12 | wgpu::Features::TEXTURE_FORMAT_P010)
}
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

        let device_descriptor = wgpu::DeviceDescriptor {
            required_features: native_video_texture_device_features(adapter.features()),
            ..wgpu::DeviceDescriptor::default()
        };
        let (device, queue) = adapter
            .request_device(&device_descriptor)
            .await
            .map_err(|e| mondrian_core::MondrianError::GpuInitFailed { reason: e.to_string() })?;

        Ok(Arc::new(Self {
            device: Arc::new(device),
            queue: Arc::new(queue),
            adapter,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::native_video_texture_device_features;

    #[test]
    fn native_video_device_features_request_only_supported_formats() {
        let unrelated = wgpu::Features::TIMESTAMP_QUERY;
        assert_eq!(
            native_video_texture_device_features(unrelated),
            wgpu::Features::empty()
        );
        assert_eq!(
            native_video_texture_device_features(
                unrelated
                    | wgpu::Features::TEXTURE_FORMAT_NV12
                    | wgpu::Features::TEXTURE_FORMAT_P010,
            ),
            wgpu::Features::TEXTURE_FORMAT_NV12 | wgpu::Features::TEXTURE_FORMAT_P010
        );
    }
}
