//! Hardware admission for default GPU tests; product failures are never skips.

pub fn gpu_is_available(test_name: &str) -> bool {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    if pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())).is_empty() {
        // Write through stderr directly so libtest does not hide the reason
        // behind its successful-test output capture.
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr(),
            "SKIP {test_name}: no GPU adapter exposed by this host"
        );
        false
    } else {
        true
    }
}
