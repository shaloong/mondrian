use super::*;

#[test]
fn retirement_drop_revokes_escaped_pool_lease_return() {
    use crate::{ColorFrameDescriptor, ColorFrameDomain, GpuColorFrameAllocationPlan, GpuContext};
    use std::time::Duration;

    // Absence of an adapter is a host capability limit. Once an adapter is
    // present, initialization and retirement errors remain real test failures.
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    if pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())).is_empty() {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(),
            "SKIP retirement_drop_revokes_escaped_pool_lease_return: no GPU adapter exposed by this host");
        return;
    }
    drop(instance);
    let context = pollster::block_on(GpuContext::new()).expect("available GPU must initialize");
    let runtime = ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
        .expect("Viewer runtime");
    let pool = Arc::clone(&runtime.resource_pool);
    let handle = GpuColorFrameHandle::new(
        GpuColorFrameId::from_raw(500),
        ColorFrameDescriptor {
            width: 2,
            height: 2,
            color_space: WorkingColorSpace::LinearRec709.into(),
            domain: ColorFrameDomain::Working,
            encoding: crate::ColorFrameEncoding::LinearFloat,
            residency: crate::ColorFrameResidency::Gpu,
            alpha: crate::ColorFrameAlpha::StraightCoverage,
        },
        GpuColorFrameTextureFormat::Rgba16Float,
        "escaped-retirement-lease",
    )
    .expect("test handle");
    let resource = pool.acquire(
        &context.device,
        &GpuColorFrameAllocationPlan::for_handle(handle),
    );
    let escaped = ViewerGpuPresentationOutputLease::new(resource, Arc::clone(&pool));
    let mut retiring = runtime.into_retirement();
    assert_eq!(
        pool.diagnostics().invalidations,
        0,
        "transition must retain the resource envelope"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while retiring.poll().expect("Renderer retirement").is_none() {
        assert!(Instant::now() < deadline, "worker retirement timeout");
        std::thread::sleep(Duration::from_millis(1));
    }
    context
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(5)),
        })
        .expect("queue barrier");
    drop(retiring);
    assert_eq!(
        pool.diagnostics().invalidations,
        1,
        "final release must revoke the return generation"
    );
    drop(escaped);
    assert_eq!(pool.diagnostics().stale_generation_releases, 1);
    assert_eq!(pool.diagnostics().retained_resources, 0);
}
