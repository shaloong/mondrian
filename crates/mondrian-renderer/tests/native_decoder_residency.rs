#![cfg(target_os = "windows")]

use mondrian_media::HwDeviceContextPool;
use mondrian_renderer::{GpuContext, GpuNativeDecodedFrameImportMode, ViewerGpuExecutionRuntime};

#[path = "support/viewer_retirement.rs"]
mod retirement_support;

#[test]
fn dx12_renderer_publishes_one_installable_zero_copy_decoder_generation() {
    let context = pollster::block_on(GpuContext::new()).expect("real GPU context");
    if context.adapter.get_info().backend != wgpu::Backend::Dx12 {
        eprintln!("skipped: selected adapter is not the Windows DX12 production backend");
        return;
    }
    let runtime = ViewerGpuExecutionRuntime::new(
        &context.adapter,
        context.device.as_ref(),
        context.queue.as_ref(),
    )
    .expect("Viewer runtime");
    let support = runtime.native_import_support();
    if !support.renderer_backend_ready {
        eprintln!(
            "skipped: adapter lacks the required native NV12/P010 feature contract: {:?}",
            support.unavailable_reason
        );
        retirement_support::retire_runtime(&context.device, runtime)
            .expect("retire skipped runtime");
        return;
    }

    assert_eq!(
        support.import_mode,
        Some(GpuNativeDecodedFrameImportMode::ZeroCopy)
    );
    assert!(
        support.renderer_backend_label.as_deref().is_some_and(|label| {
            label.contains("same-device D3D12VA") && !label.contains("shared")
        })
    );
    let selector = support.hardware_decode_device_selector.expect("decoder selector");
    let root = runtime.native_decode_device_root().expect("renderer-qualified FFmpeg root");
    let pool = HwDeviceContextPool::default();

    assert!(pool
        .install_renderer_device_context(selector, root.clone())
        .expect("first generation install"));
    assert!(!pool
        .install_renderer_device_context(selector, root)
        .expect("idempotent generation install"));
    let replacement_runtime = ViewerGpuExecutionRuntime::new(
        &context.adapter,
        context.device.as_ref(),
        context.queue.as_ref(),
    )
    .expect("replacement Viewer runtime");
    let replacement_root = replacement_runtime
        .native_decode_device_root()
        .expect("replacement renderer-qualified FFmpeg root");
    assert!(pool
        .install_renderer_device_context(selector, replacement_root)
        .expect("replacement generation install"));
    let diagnostics = pool.diagnostics();
    assert_eq!(diagnostics.entries, 1);
    assert_eq!(diagnostics.retirements, 1);
    assert!(diagnostics.latest_generation >= 2);
    assert_eq!(runtime.native_import_pool_residency(), (1, 0));
    assert_eq!(runtime.native_import_retained_source_count(), 0);
    assert!(pool.retire_renderer_device_context(selector));
    assert!(!pool.retire_renderer_device_context(selector));
    assert_eq!(pool.diagnostics().entries, 0);
    retirement_support::retire_runtime(&context.device, runtime).expect("retire original runtime");
    retirement_support::retire_runtime(&context.device, replacement_runtime)
        .expect("retire replacement runtime");
}
