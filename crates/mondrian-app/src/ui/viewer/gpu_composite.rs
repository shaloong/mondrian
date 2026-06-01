//! GPU compositor integration for the viewer panel.
//!
//! Manages the process-wide GPU compositor singleton, failure tracking,
//! and the primary entry point for GPU-accelerated layer compositing.
//!
//! Extracted from `viewer_panel.rs` during the Phase 5 file split.

use mondrian_renderer::{CompositorConfig, CpuRgbaLayer, FrameCompositor, GpuContext};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// Try to composite RGBA layers using the GPU.
/// Returns `None` if GPU compositing is unavailable or fails.
pub fn try_gpu_composite_rgba_layers(
    width: u32,
    height: u32,
    rgba_layers_for_gpu: &[CpuRgbaLayer],
) -> Option<Vec<u8>> {
    if !gpu_compositor_enabled() {
        return None;
    }

    let gpu_compositor = global_gpu_compositor()?;
    let _gpu_composite_started_at = Instant::now();
    let gpu_result = {
        let mut guard = gpu_compositor.lock();
        guard.composite_rgba_layers(width, height, rgba_layers_for_gpu)
    };

    match gpu_result {
        Ok(gpu_rgba) => {
            record_gpu_compositor_result(true);
            Some(gpu_rgba)
        }
        Err(err) => {
            record_gpu_compositor_result(false);
            tracing::warn!("GPU compositor failed, falling back to CPU: {}", err);
            None
        }
    }
}

fn global_gpu_compositor() -> Option<&'static Mutex<FrameCompositor>> {
    static GPU_COMPOSITOR: OnceLock<Option<Mutex<FrameCompositor>>> = OnceLock::new();
    GPU_COMPOSITOR
        .get_or_init(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            let gpu = rt.block_on(GpuContext::new()).ok()?;
            Some(Mutex::new(FrameCompositor::new(
                gpu,
                CompositorConfig::default(),
            )))
        })
        .as_ref()
}

fn gpu_compositor_enabled() -> bool {
    global_gpu_compositor().is_some()
}

fn record_gpu_compositor_result(success: bool) {
    static CONSECUTIVE_FAILURES: OnceLock<AtomicU64> = OnceLock::new();
    let counter = CONSECUTIVE_FAILURES.get_or_init(|| AtomicU64::new(0));

    if success {
        counter.store(0, Ordering::Relaxed);
    } else {
        let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
        if n % 60 == 0 {
            tracing::warn!("GPU compositor failed {n} times consecutively, retrying...");
        }
    }
}

// Note: `record_preview_perf_composite_ns` is defined in viewer_panel.rs.
// We inline the GPU perf path here; non-GPU perf is recorded in the panel.
