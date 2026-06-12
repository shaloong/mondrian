//! GPU compositor integration for the viewer panel.
//!
//! Manages the global GPU compositor singleton, failure tracking, and
//! utility functions for texture creation and zero-copy compositing.
//!
//! Extracted from `viewer_panel.rs` during the Phase 5 file split.

use egui_wgpu::wgpu;
use egui_wgpu::wgpu::util::DeviceExt as _;
use mondrian_renderer::{CompositorConfig, CpuRgbaLayer, FrameCompositor, GpuContext};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

// ── GPU color conversion ──────────────────────────────────────────────

/// Parameters for the GPU color conversion compute shader.
#[derive(Clone, Copy)]
pub struct GpuColorConversionParams {
    /// Source transfer gamma (e.g. Rec709 = 2.4, sRGB ≈ 2.2, 0.0 = linear).
    pub decode_gamma: f32,
    /// Display profile 3x3 linear matrix (row-major).
    pub display_matrix: [[f32; 3]; 3],
    /// Display profile gamma.
    pub display_gamma: f32,
    /// Profile color space transfer gamma for re-encoding.
    pub encode_gamma: f32,
}

impl Default for GpuColorConversionParams {
    fn default() -> Self {
        Self {
            decode_gamma: 2.4, // Rec709
            display_matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            display_gamma: 0.0, // no-op (γ=0 means linear/no conversion)
            encode_gamma: 2.4,  // Rec709
        }
    }
}

impl GpuColorConversionParams {
    /// True if this conversion is effectively a no-op.
    pub fn is_noop(&self) -> bool {
        let dm = self.display_matrix;
        let is_identity = (dm[0][0] - 1.0).abs() < 0.001
            && dm[0][1].abs() < 0.001
            && dm[0][2].abs() < 0.001
            && dm[1][0].abs() < 0.001
            && (dm[1][1] - 1.0).abs() < 0.001
            && dm[1][2].abs() < 0.001
            && dm[2][0].abs() < 0.001
            && dm[2][1].abs() < 0.001
            && (dm[2][2] - 1.0).abs() < 0.001;
        is_identity
            && (self.decode_gamma - self.encode_gamma).abs() < 0.01
            && self.display_gamma <= 0.001
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorConvertUniforms {
    decode_gamma: f32,
    _pad0: [f32; 3],
    display_matrix_0: [f32; 4],
    display_matrix_1: [f32; 4],
    display_matrix_2: [f32; 4],
    display_gamma: f32,
    encode_gamma: f32,
    _pad1: [f32; 2],
}

struct CachedColorConvert {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

fn color_convert_pipeline(device: &wgpu::Device) -> &'static CachedColorConvert {
    static PIPELINE: OnceLock<CachedColorConvert> = OnceLock::new();
    PIPELINE.get_or_init(|| {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("color_convert_shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../../../../mondrian-renderer/shaders/color_convert_compute.wgsl")
                    .into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("color_convert_bgl_0"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });

        let uniform_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("color_convert_bgl_1"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("color_convert_pl"),
            immediate_size: 0,
            bind_group_layouts: &[Some(&bind_group_layout), Some(&uniform_bgl)],
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("color_convert_pipeline"),
            layout: Some(&pipeline_layout),
            cache: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });

        CachedColorConvert { pipeline, bind_group_layout }
    })
}

/// Apply GPU color conversion to a composited texture.
/// Takes ownership of the input — returns it unchanged if no-op.
pub fn apply_gpu_color_conversion(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    input_texture: wgpu::Texture,
    width: u32,
    height: u32,
    params: GpuColorConversionParams,
) -> wgpu::Texture {
    if params.is_noop() {
        return input_texture;
    }

    let cached = color_convert_pipeline(device);

    // Output texture (COPY_DST included for recycling compatibility).
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("color_convert_output"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let input_view = input_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());

    let bg0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("color_convert_bg0"),
        layout: &cached.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&input_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&output_view),
            },
        ],
    });

    // Build uniforms (vec3 padded to vec4 for alignment)
    let uniforms = ColorConvertUniforms {
        decode_gamma: params.decode_gamma,
        _pad0: [0.0; 3],
        display_matrix_0: [
            params.display_matrix[0][0],
            params.display_matrix[0][1],
            params.display_matrix[0][2],
            0.0,
        ],
        display_matrix_1: [
            params.display_matrix[1][0],
            params.display_matrix[1][1],
            params.display_matrix[1][2],
            0.0,
        ],
        display_matrix_2: [
            params.display_matrix[2][0],
            params.display_matrix[2][1],
            params.display_matrix[2][2],
            0.0,
        ],
        display_gamma: params.display_gamma,
        encode_gamma: params.encode_gamma,
        _pad1: [0.0; 2],
    };

    let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("color_convert_uniforms"),
        contents: bytemuck::bytes_of(&uniforms),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    let bg1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("color_convert_bg1"),
        layout: &cached.pipeline.get_bind_group_layout(1),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buffer.as_entire_binding(),
        }],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("color_convert_encoder"),
    });
    {
        let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("color_convert_pass"),
            timestamp_writes: None,
        });
        cpass.set_pipeline(&cached.pipeline);
        cpass.set_bind_group(0, &bg0, &[]);
        cpass.set_bind_group(1, &bg1, &[]);
        let wg_x = width.div_ceil(8);
        let wg_y = height.div_ceil(8);
        cpass.dispatch_workgroups(wg_x, wg_y, 1);
    }
    queue.submit([encoder.finish()]);

    output
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

// ── Texture recycling ─────────────────────────────────────────────────

/// Return a wgpu texture to the global preview cache for reuse.
/// The next `try_reuse_texture` call with matching dimensions will return it.
pub fn recycle_texture(texture: wgpu::Texture, width: u32, height: u32) {
    static CACHE: parking_lot::Mutex<Option<(u32, u32, wgpu::Texture)>> =
        parking_lot::Mutex::new(None);
    let mut cache = CACHE.lock();
    // Only keep the most recent texture (most common case: constant resolution)
    *cache = Some((width, height, texture));
}

/// Try to get a recycled texture with matching dimensions.
/// Uploads `data` into it if found, returns `None` if no cached texture matches.
pub fn try_reuse_texture(
    _device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    data: &[u8],
) -> Option<wgpu::Texture> {
    static CACHE: parking_lot::Mutex<Option<(u32, u32, wgpu::Texture)>> =
        parking_lot::Mutex::new(None);
    let mut cache = CACHE.lock();
    if let Some((w, h, tex)) = cache.take() {
        if w == width && h == height {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
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
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
            return Some(tex);
        }
        // Wrong size — drop the old texture.
    }
    None
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
