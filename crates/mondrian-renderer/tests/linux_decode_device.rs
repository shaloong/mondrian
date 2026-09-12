//! Device binding for the real Linux native-video admission boundary.
#![cfg(target_os = "linux")]

use mondrian_renderer::{
    GpuColorFrameWgpuResourcePool, GpuContext, VulkanNativeVideoImportBackend,
};
use std::sync::Arc;

#[test]
#[ignore = "requires a real Vulkan DMA-BUF adapter and DRM render node"]
fn vaapi_admission_binds_the_renderer_drm_device() {
    let context = pollster::block_on(GpuContext::new()).expect("native Vulkan adapter required");
    let backend = VulkanNativeVideoImportBackend::new_with_resource_pool(
        &context.adapter,
        &context.device,
        &context.queue,
        Arc::new(GpuColorFrameWgpuResourcePool::default()),
    )
    .expect("real VA-API import admission required");
    assert!(
        backend.support().hardware_decode_device_selector.is_some(),
        "native import must not let FFmpeg select an unrelated default GPU"
    );
}
