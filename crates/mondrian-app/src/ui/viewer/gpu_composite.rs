//! GPU compositor integration for the viewer panel.
//!
//! Manages the GPU compositor singleton, failure tracking, and the
//! preview output texture lifecycle (egui-managed, GPU-uploaded).
//!
//! Extracted from `viewer_panel.rs` during the Phase 5 file split.

use egui_wgpu::wgpu;
use mondrian_renderer::{CompositorConfig, CpuRgbaLayer, FrameCompositor, GpuContext};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

// ── Preview output texture manager ───────────────────────────────────

/// Manages the preview output texture lifecycle in egui's texture manager.
/// On each frame, CPU-composited RGBA data is uploaded to a reusable
/// egui texture, avoiding per-frame allocation.
#[derive(Default)]
pub struct PreviewOutput {
    texture_id: Option<egui::TextureId>,
    width: u32,
    height: u32,
}

impl PreviewOutput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn egui_texture_id(&self) -> Option<egui::TextureId> {
        self.texture_id
    }

    pub fn size(&self) -> egui::Vec2 {
        egui::vec2(self.width as f32, self.height as f32)
    }

    /// Upload composited RGBA data to the preview texture.
    /// The texture is created on first call and updated in-place on subsequent frames.
    pub fn update(&mut self, ctx: &egui::Context, width: u32, height: u32, data: &[u8]) {
        let size = [width as usize, height as usize];
        let image = egui::ColorImage::from_rgba_unmultiplied(size, data);
        let image = std::sync::Arc::new(image);

        if self.texture_id.is_none() {
            let tex = ctx.tex_manager().write().alloc(
                "mondrian-preview".into(),
                egui::epaint::ImageData::Color(image.clone()),
                egui::TextureOptions::LINEAR,
            );
            self.texture_id = Some(tex);
        } else {
            let delta = egui::epaint::ImageDelta {
                image: egui::epaint::ImageData::Color(image),
                pos: None,
                options: egui::TextureOptions::LINEAR,
            };
            ctx.tex_manager().write().set(self.texture_id.unwrap(), delta);
        }
        self.width = width;
        self.height = height;
    }

    /// Release the egui texture (e.g., on resolution change or panel close).
    pub fn release(&mut self, ctx: &egui::Context) {
        if let Some(id) = self.texture_id.take() {
            ctx.tex_manager().write().free(id);
        }
    }
}

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
