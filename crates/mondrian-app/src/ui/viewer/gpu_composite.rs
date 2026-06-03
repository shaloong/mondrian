//! GPU compositor integration for the viewer panel.
//!
//! Manages the global GPU compositor singleton, failure tracking, and
//! utility functions for texture creation and zero-copy compositing.
//!
//! Extracted from `viewer_panel.rs` during the Phase 5 file split.

use egui_wgpu::wgpu;
use mondrian_renderer::{CompositorConfig, CpuRgbaLayer, FrameCompositor, GpuContext};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

// ── GPU compositor ───────────────────────────────────────────────────

/// Initialize the global GPU compositor with an external wgpu device
/// (typically from eframe's `CreationContext::wgpu_render_state`).
/// Must be called once at app startup, before any compositing.
pub fn init_gpu_compositor(gpu: Arc<GpuContext>) {
    let compositor = Mutex::new(FrameCompositor::new(gpu, CompositorConfig::default()));
    if GPU_COMPOSITOR.set(compositor).is_err() {
        tracing::warn!("init_gpu_compositor called more than once — ignored");
    }
}

static GPU_COMPOSITOR: OnceLock<Mutex<FrameCompositor>> = OnceLock::new();

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
    GPU_COMPOSITOR.get()
}

fn gpu_compositor_enabled() -> bool {
    GPU_COMPOSITOR.get().is_some()
}

/// Try to composite RGBA layers to a wgpu texture (zero-copy, no readback).
/// Returns the composited texture, or `None` if GPU is unavailable or fails.
pub fn try_gpu_composite_to_texture(
    width: u32,
    height: u32,
    rgba_layers_for_gpu: &[CpuRgbaLayer],
) -> Option<wgpu::Texture> {
    let gpu_compositor = global_gpu_compositor()?;
    let gpu_result = {
        let mut guard = gpu_compositor.lock();
        guard.composite_rgba_layers_to_texture(width, height, rgba_layers_for_gpu)
    };

    match gpu_result {
        Ok(texture) => {
            record_gpu_compositor_result(true);
            Some(texture)
        }
        Err(err) => {
            record_gpu_compositor_result(false);
            tracing::warn!("GPU compositor (texture) failed, falling back: {}", err);
            None
        }
    }
}

/// Get the shared wgpu device from the global compositor.
pub fn gpu_device() -> Option<wgpu::Device> {
    global_gpu_compositor().map(|c| c.lock().device().clone())
}

/// Get the shared wgpu queue from the global compositor.
pub fn gpu_queue() -> Option<wgpu::Queue> {
    global_gpu_compositor().map(|c| c.lock().queue().clone())
}

/// Upload RGBA8 pixel data to a new wgpu texture.
pub fn create_rgba_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    data: &[u8],
) -> wgpu::Texture {
    let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mondrian_preview_upload"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        size,
    );
    texture
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
