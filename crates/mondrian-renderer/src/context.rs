//! wgpu GPU 上下文初始化

use mondrian_core::Result;
use std::sync::Arc;

#[cfg(test)]
const TEST_GPU_CONTEXT_CAPACITY: usize = 2;

#[cfg(test)]
static TEST_GPU_CONTEXT_ADMISSION: (std::sync::Mutex<usize>, std::sync::Condvar) =
    (std::sync::Mutex::new(0), std::sync::Condvar::new());

/// Process-local admission for unit tests that own independent native devices.
///
/// The Rust test harness otherwise provisions dozens of DX12 devices at once.
/// That is not representative of product execution and can terminate the test
/// process in the Windows driver before an assertion is reported. The permit
/// remains attached to the context so device destruction, not test scheduling,
/// releases capacity.
#[cfg(test)]
struct TestGpuContextPermit;

#[cfg(test)]
impl TestGpuContextPermit {
    fn acquire() -> Self {
        let (admission, changed) = &TEST_GPU_CONTEXT_ADMISSION;
        let active = match admission.lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut active =
            match changed.wait_while(active, |active| *active >= TEST_GPU_CONTEXT_CAPACITY) {
                Ok(active) => active,
                Err(poisoned) => poisoned.into_inner(),
            };
        *active += 1;
        Self
    }
}

#[cfg(test)]
impl Drop for TestGpuContextPermit {
    fn drop(&mut self) {
        let (admission, changed) = &TEST_GPU_CONTEXT_ADMISSION;
        let mut active = match admission.lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        *active = active.saturating_sub(1);
        changed.notify_one();
    }
}

/// Optional wgpu features required to sample native NV12/P010 video textures.
///
/// Only features advertised by the selected adapter are returned, so callers
/// can add the result to `DeviceDescriptor::required_features` without turning
/// an unsupported native-video format into device creation failure.
pub fn native_video_texture_device_features(adapter_features: wgpu::Features) -> wgpu::Features {
    let mut required = adapter_features & wgpu::Features::TEXTURE_FORMAT_NV12;
    let p010_requirements =
        wgpu::Features::TEXTURE_FORMAT_P010 | wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;
    if adapter_features.contains(p010_requirements) {
        required |= p010_requirements;
    }
    required
}

/// Optional wgpu feature required for OCIO LUTs that request hardware filtering.
///
/// OCIO supplies LUT payloads as 32-bit float textures. Nearest-only LUTs do
/// not need this feature; linear/default/best interpolation does. Requesting it
/// whenever the adapter advertises support keeps one device compatible with
/// every cached color transform without weakening sampler correctness.
pub fn ocio_lut_filtering_device_features(adapter_features: wgpu::Features) -> wgpu::Features {
    adapter_features & wgpu::Features::FLOAT32_FILTERABLE
}

/// Request an adapter while preserving native-video import on platforms where
/// the renderer has a backend-specific bridge.
///
/// The [`wgpu::Instance`] already reflects any `WGPU_BACKEND` restriction, so
/// an explicit environment override remains authoritative. On Windows, when
/// more than one backend represents the same physical GPU, a DX12 adapter with
/// native NV12/P010 support is preferred over a Vulkan representation that
/// cannot participate in the D3D11/DX12 shared-texture bridge. If enumeration
/// produces no admissible adapter, wgpu's normal request path remains the
/// fallback.
pub async fn request_adapter_with_native_video_preference(
    instance: &wgpu::Instance,
    options: &wgpu::RequestAdapterOptions<'_, '_>,
) -> Result<wgpu::Adapter, wgpu::RequestAdapterError> {
    if !options.force_fallback_adapter {
        let adapters = instance.enumerate_adapters(wgpu::Backends::all()).await;
        if let Some(adapter) = adapters
            .into_iter()
            .filter(|adapter| {
                options
                    .compatible_surface
                    .is_none_or(|surface| adapter.is_surface_supported(surface))
            })
            .max_by_key(|adapter| {
                let info = adapter.get_info();
                native_video_adapter_priority(
                    info.backend,
                    adapter.features(),
                    info.device_type,
                    options.power_preference,
                )
            })
        {
            return Ok(adapter);
        }
    }

    instance.request_adapter(options).await
}

fn native_video_adapter_priority(
    backend: wgpu::Backend,
    features: wgpu::Features,
    device_type: wgpu::DeviceType,
    power_preference: wgpu::PowerPreference,
) -> (u8, u8) {
    #[cfg(target_os = "windows")]
    let native_backend = match backend {
        wgpu::Backend::Dx12
            if native_video_texture_device_features(features).intersects(
                wgpu::Features::TEXTURE_FORMAT_NV12 | wgpu::Features::TEXTURE_FORMAT_P010,
            ) =>
        {
            2
        }
        wgpu::Backend::Dx12 => 1,
        _ => 0,
    };
    #[cfg(not(target_os = "windows"))]
    let native_backend = {
        let _ = (backend, features);
        0
    };

    let power = match (power_preference, device_type) {
        (wgpu::PowerPreference::HighPerformance, wgpu::DeviceType::DiscreteGpu)
        | (wgpu::PowerPreference::LowPower, wgpu::DeviceType::IntegratedGpu) => 4,
        (wgpu::PowerPreference::HighPerformance, wgpu::DeviceType::IntegratedGpu)
        | (wgpu::PowerPreference::LowPower, wgpu::DeviceType::DiscreteGpu) => 3,
        (_, wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu) => 2,
        (_, wgpu::DeviceType::Other | wgpu::DeviceType::VirtualGpu) => 1,
        (_, wgpu::DeviceType::Cpu) => 0,
    };
    (native_backend, power)
}
use wgpu;

pub struct GpuContext {
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    pub adapter: wgpu::Adapter,
    #[cfg(test)]
    _test_permit: Option<TestGpuContextPermit>,
}

impl GpuContext {
    /// Create a GpuContext from an existing wgpu device and queue.
    /// Used when sharing the eframe-created device.
    pub fn from_device_queue(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        adapter: wgpu::Adapter,
    ) -> Arc<Self> {
        Arc::new(Self {
            device,
            queue,
            adapter,
            #[cfg(test)]
            _test_permit: None,
        })
    }

    /// Creates a new independent GPU context (standalone device).
    /// Used when no external device is available (e.g., tests, export).
    pub async fn new() -> Result<Arc<Self>> {
        #[cfg(test)]
        let test_permit = TestGpuContextPermit::acquire();

        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());

        let adapter = request_adapter_with_native_video_preference(
            &instance,
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            },
        )
        .await
        .map_err(|e| mondrian_core::MondrianError::GpuInitFailed {
            reason: format!("找不到合适的 GPU 适配器: {e}"),
        })?;

        tracing::info!("GPU Adapter: {:?}", adapter.get_info());

        let device_descriptor = wgpu::DeviceDescriptor {
            required_features: native_video_texture_device_features(adapter.features())
                | ocio_lut_filtering_device_features(adapter.features()),
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
            #[cfg(test)]
            _test_permit: Some(test_permit),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        native_video_adapter_priority, native_video_texture_device_features,
        ocio_lut_filtering_device_features,
    };

    #[test]
    fn ocio_lut_filtering_feature_is_requested_only_when_supported() {
        assert!(ocio_lut_filtering_device_features(wgpu::Features::empty()).is_empty());
        assert_eq!(
            ocio_lut_filtering_device_features(
                wgpu::Features::FLOAT32_FILTERABLE | wgpu::Features::TIMESTAMP_QUERY,
            ),
            wgpu::Features::FLOAT32_FILTERABLE
        );
    }

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
                    | wgpu::Features::TEXTURE_FORMAT_P010
                    | wgpu::Features::TEXTURE_FORMAT_16BIT_NORM,
            ),
            wgpu::Features::TEXTURE_FORMAT_NV12
                | wgpu::Features::TEXTURE_FORMAT_P010
                | wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
        );
    }

    #[test]
    fn p010_is_not_enabled_without_required_16_bit_plane_view_feature() {
        assert_eq!(
            native_video_texture_device_features(wgpu::Features::TEXTURE_FORMAT_P010),
            wgpu::Features::empty()
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_dx12_native_video_adapter_outranks_other_backend_representations() {
        let native_dx12 = native_video_adapter_priority(
            wgpu::Backend::Dx12,
            wgpu::Features::TEXTURE_FORMAT_P010 | wgpu::Features::TEXTURE_FORMAT_16BIT_NORM,
            wgpu::DeviceType::IntegratedGpu,
            wgpu::PowerPreference::HighPerformance,
        );
        let discrete_vulkan = native_video_adapter_priority(
            wgpu::Backend::Vulkan,
            wgpu::Features::empty(),
            wgpu::DeviceType::DiscreteGpu,
            wgpu::PowerPreference::HighPerformance,
        );
        assert!(native_dx12 > discrete_vulkan);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_native_video_formats_outrank_dx12_without_import_support() {
        let native_dx12 = native_video_adapter_priority(
            wgpu::Backend::Dx12,
            wgpu::Features::TEXTURE_FORMAT_NV12,
            wgpu::DeviceType::IntegratedGpu,
            wgpu::PowerPreference::HighPerformance,
        );
        let plain_dx12 = native_video_adapter_priority(
            wgpu::Backend::Dx12,
            wgpu::Features::empty(),
            wgpu::DeviceType::DiscreteGpu,
            wgpu::PowerPreference::HighPerformance,
        );
        assert!(native_dx12 > plain_dx12);
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn real_adapter_selection_uses_dx12_when_native_video_formats_are_available() {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let has_native_dx12 = instance
            .enumerate_adapters(wgpu::Backends::all())
            .await
            .into_iter()
            .any(|adapter| {
                adapter.get_info().backend == wgpu::Backend::Dx12
                    && adapter.features().intersects(
                        wgpu::Features::TEXTURE_FORMAT_NV12 | wgpu::Features::TEXTURE_FORMAT_P010,
                    )
            });
        let adapter = super::request_adapter_with_native_video_preference(
            &instance,
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            },
        )
        .await
        .expect("real GPU adapter");

        if has_native_dx12 {
            assert_eq!(adapter.get_info().backend, wgpu::Backend::Dx12);
            assert!(adapter.features().intersects(
                wgpu::Features::TEXTURE_FORMAT_NV12 | wgpu::Features::TEXTURE_FORMAT_P010
            ));
        }
    }
}
