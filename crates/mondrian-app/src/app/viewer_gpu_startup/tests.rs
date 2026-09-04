//! Real device/progress/Renderer fault injection without a native Window.

use super::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

fn device() -> (wgpu::Adapter, wgpu::Device, wgpu::Queue) {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(
        mondrian_renderer::request_adapter_with_native_video_preference(
            &instance,
            &wgpu::RequestAdapterOptions::default(),
        ),
    )
    .expect("real GPU adapter (not a capability skip)");
    let supported = adapter.features();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: mondrian_renderer::native_video_texture_device_features(supported)
            | mondrian_renderer::ocio_lut_filtering_device_features(supported),
        ..Default::default()
    }))
    .expect("real GPU device");
    (adapter, device, queue)
}

fn assert_joined(
    evidence: super::super::viewer_gpu_device_progress::ViewerGpuDeviceProgressShutdownEvidence,
) {
    assert!(evidence.worker_started);
    assert!(evidence.worker_terminated);
    assert!(!evidence.worker_panicked);
    assert!(!evidence.timed_out);
    assert!(evidence.retirement_requested);
    assert!(evidence.retirement_handoff_accepted);
    assert!(evidence.retirement_completed);
    assert_eq!(evidence.generation_terminal_kind, None);
}

#[test]
#[ignore = "requires a real local GPU"]
fn partial_startup_error_before_renderer_joins_without_inventing_renderer() {
    let (_adapter, device, queue) = device();
    let mut startup =
        ViewerGpuStartupOwner::new(&device, &queue, ViewerGpuDeviceProgressWake::default())
            .expect("progress started");
    // This is also the reopen boundary: the candidate exists but retiring the
    // old generation can fail before the candidate Renderer is constructed.
    let preparation: Result<(), &str> = Err("old generation seal failed");
    assert_eq!(preparation, Err("old generation seal failed"));
    assert!(
        startup.activate().is_none(),
        "partial owner cannot become an Adapter"
    );
    let receipt = startup
        .shutdown_until(Instant::now() + Duration::from_secs(10))
        .expect("actual progress owner");
    assert_joined(receipt);
    assert_eq!(receipt.renderer_retirement, None);
}

#[test]
#[ignore = "requires a real local GPU"]
fn partial_startup_panic_after_renderer_keeps_worker_until_consuming_shutdown() {
    let (adapter, device, queue) = device();
    let mut startup =
        ViewerGpuStartupOwner::new(&device, &queue, ViewerGpuDeviceProgressWake::default())
            .expect("progress started");
    let failure = catch_unwind(AssertUnwindSafe(|| {
        startup.install_runtime(
            ViewerGpuExecutionRuntime::new(&adapter, &device, &queue)
                .expect("real Renderer runtime"),
        );
        panic!("injected UI router construction panic");
    }));
    assert_eq!(
        failure.expect_err("injected panic").downcast_ref::<&str>(),
        Some(&"injected UI router construction panic")
    );
    assert!(
        startup.runtime().is_some(),
        "catch boundary must not drop Renderer"
    );
    let receipt = startup
        .shutdown_until(Instant::now() + Duration::from_secs(10))
        .expect("actual progress owner");
    assert_joined(receipt);
    assert!(receipt
        .renderer_retirement
        .expect("actual Renderer worker receipt")
        .is_healthy());
}

#[test]
#[ignore = "requires a real local GPU"]
fn completed_startup_transfers_once_without_retiring_the_live_adapter() {
    let (adapter, device, queue) = device();
    let mut startup =
        ViewerGpuStartupOwner::new(&device, &queue, ViewerGpuDeviceProgressWake::default())
            .expect("progress started");
    startup.install_runtime(
        ViewerGpuExecutionRuntime::new(&adapter, &device, &queue).expect("real Renderer runtime"),
    );
    let (progress, runtime) = startup.activate().expect("complete pair");
    assert!(
        startup.handles.is_none(),
        "activated guard must not pin an old device/queue"
    );
    assert!(startup.activate().is_none(), "no duplicate activation");
    assert!(
        startup.shutdown_until(Instant::now()).is_none(),
        "consumed guard has no receipt"
    );
    let receipt = progress.retire_device_generation_until(
        StartupRetirement {
            runtime: Some(runtime.into_retirement()),
            _device: device,
            _queue: queue,
            native_error_logged: false,
        },
        Instant::now() + Duration::from_secs(10),
    );
    assert_joined(receipt);
    assert!(receipt
        .renderer_retirement
        .expect("transferred runtime was still alive")
        .is_healthy());
}
