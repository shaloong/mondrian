//! GPU compute backend for effect processing.
//!
//! Provides compute-shader acceleration for supported effects with
//! automatic CPU fallback. Each effect retains its CPU implementation —
//! the GPU path is a transparent optimization, never a hard requirement.
//!
//! Shader modules, bind group layouts, and pipeline layouts are cached
//! after first creation. Per-dispatch resources (input/output textures,
//! uniform buffers, bind groups) are created each call.

use crate::context::GpuContext;
use crate::shaders;
use mondrian_effects::{EffectGpuExecutor, EffectRenderOp};
use parking_lot::Mutex;
use std::sync::Arc;
use wgpu;

/// Global flag to disable GPU acceleration at runtime (set by user preferences).
static GPU_DISABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set whether GPU acceleration is enabled. Call from the preferences UI.
pub fn set_gpu_enabled(enabled: bool) {
    GPU_DISABLED.store(!enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Check whether GPU acceleration is currently enabled.
pub fn gpu_enabled() -> bool {
    !GPU_DISABLED.load(std::sync::atomic::Ordering::Relaxed)
}

// ── Public types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuEffectKind {
    ColorAdjust,
    GaussianBlur,
    Lut3D,
}

#[derive(Debug, Clone)]
pub enum GpuExecResult {
    Success { data: Vec<u8> },
    Fallback { reason: GpuFallbackReason },
}

#[derive(Debug, Clone)]
pub enum GpuFallbackReason {
    GpuUnavailable,
    EffectNotSupported,
    ExecutionFailed(String),
}

impl GpuFallbackReason {
    pub fn user_message(&self) -> &str {
        match self {
            Self::GpuUnavailable => "GPU 不可用，使用 CPU 渲染",
            Self::EffectNotSupported => "该特效暂不支持 GPU 加速",
            Self::ExecutionFailed(e) => {
                tracing::warn!("GPU execution failed: {e}");
                "GPU 执行失败，已回退到 CPU"
            }
        }
    }
}

// ── Cached pipeline resources ────────────────────────────────────────

/// Pre-compiled shader + bind group layouts for a specific effect.
/// Created once on first use, shared across all dispatches.
struct CachedPipeline {
    shader: wgpu::ShaderModule,
    bgl_0: wgpu::BindGroupLayout,
    bgl_1: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
}

impl CachedPipeline {
    fn new(device: &wgpu::Device, shader_src: &str, label: &str) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(shader_src.into()),
        });

        let bgl_0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effect_bgl_0"),
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

        let bgl_1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effect_bgl_1"),
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
            label: Some("effect_pipeline_layout"),
            bind_group_layouts: &[&bgl_0, &bgl_1],
            push_constant_ranges: &[],
        });

        Self { shader, bgl_0, bgl_1, pipeline_layout }
    }
}

// ── GpuBackend ────────────────────────────────────────────────────────

/// Like CachedPipeline but with an extra binding (2) for a 3D LUT texture.
struct CachedLutPipeline {
    shader: wgpu::ShaderModule,
    bgl_0: wgpu::BindGroupLayout,
    bgl_1: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
}

impl CachedLutPipeline {
    fn new(device: &wgpu::Device, shader_src: &str, label: &str) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(shader_src.into()),
        });

        let bgl_0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lut_bgl_0"),
            entries: &[
                // binding 0: input 2D texture (read-only)
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
                // binding 1: output 2D storage texture (write-only)
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
                // binding 2: LUT 3D texture (read-only)
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let bgl_1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lut_bgl_1"),
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
            label: Some("lut_pipeline_layout"),
            bind_group_layouts: &[&bgl_0, &bgl_1],
            push_constant_ranges: &[],
        });

        Self { shader, bgl_0, bgl_1, pipeline_layout }
    }
}

pub struct GpuBackend {
    gpu: Arc<GpuContext>,
    available: bool,
    color_adjust: Mutex<Option<Arc<CachedPipeline>>>,
    blur: Mutex<Option<Arc<CachedPipeline>>>,
    lut3d: Mutex<Option<Arc<CachedLutPipeline>>>,
}

impl GpuBackend {
    pub async fn new() -> Option<Arc<Self>> {
        let gpu = GpuContext::new().await.ok()?;
        tracing::info!("GPU backend initialized: {:?}", gpu.adapter.get_info());
        Some(Arc::new(Self {
            gpu,
            available: true,
            color_adjust: Mutex::new(None),
            blur: Mutex::new(None),
            lut3d: Mutex::new(None),
        }))
    }

    pub fn is_available(&self) -> bool {
        self.available && gpu_enabled()
    }

    // ── Effect execution ──────────────────────────────────────────

    pub fn execute_color_adjust(
        &self,
        input: &[u8],
        width: u32,
        height: u32,
        exposure: f32,
        contrast: f32,
        saturation: f32,
    ) -> GpuExecResult {
        if !self.available {
            return GpuExecResult::Fallback { reason: GpuFallbackReason::GpuUnavailable };
        }
        if exposure.abs() < 1e-4 && (contrast - 1.0).abs() < 1e-4 && (saturation - 1.0).abs() < 1e-4 {
            return GpuExecResult::Success { data: input.to_vec() };
        }
        let pipeline = self.get_or_create_pipeline(&self.color_adjust, shaders::COLOR_ADJUST_COMPUTE, "color_adjust");
        self.dispatch(input, width, height, pipeline.as_ref(), &[exposure, contrast, saturation, 0.0])
    }

    pub fn execute_blur(
        &self,
        input: &[u8],
        width: u32,
        height: u32,
        radius: f32,
    ) -> GpuExecResult {
        if !self.available {
            return GpuExecResult::Fallback { reason: GpuFallbackReason::GpuUnavailable };
        }
        if radius < 1e-4 {
            return GpuExecResult::Success { data: input.to_vec() };
        }
        let pipeline = self.get_or_create_pipeline(&self.blur, shaders::BLUR_GAUSSIAN_COMPUTE, "blur_gaussian");
        let horiz = self.dispatch(input, width, height, pipeline.as_ref(), &[radius, 1.0, 0.0, 0.0]);
        let horiz_data = match horiz {
            GpuExecResult::Success { data } => data,
            other => return other,
        };
        self.dispatch(&horiz_data, width, height, pipeline.as_ref(), &[radius, 0.0, 1.0, 0.0])
    }

    /// GPU 3D LUT color grading.
    /// `lut_rgba` is a flat array of size³ × 4 f32 values (R, G, B, A=1.0).
    pub fn execute_lut3d(
        &self,
        input: &[u8],
        width: u32,
        height: u32,
        lut_rgba: &[f32],
        lut_size: u32,
        intensity: f32,
    ) -> GpuExecResult {
        if !self.available {
            return GpuExecResult::Fallback { reason: GpuFallbackReason::GpuUnavailable };
        }
        if intensity <= 0.001 {
            return GpuExecResult::Success { data: input.to_vec() };
        }
        let pipeline = self.get_or_create_lut_pipeline();
        self.dispatch_lut(input, width, height, &pipeline, lut_rgba, lut_size, intensity)
    }

    // ── Internal ──────────────────────────────────────────────────

    fn get_or_create_pipeline(
        &self,
        cache: &Mutex<Option<Arc<CachedPipeline>>>,
        src: &str,
        label: &str,
    ) -> Arc<CachedPipeline> {
        let mut guard = cache.lock();
        if guard.is_none() {
            *guard = Some(Arc::new(CachedPipeline::new(&self.gpu.device, src, label)));
        }
        Arc::clone(guard.as_ref().unwrap())
    }

    fn get_or_create_lut_pipeline(&self) -> Arc<CachedLutPipeline> {
        let mut guard = self.lut3d.lock();
        if guard.is_none() {
            *guard = Some(Arc::new(CachedLutPipeline::new(
                &self.gpu.device,
                shaders::LUT3D_COMPUTE,
                "lut3d_compute",
            )));
        }
        Arc::clone(guard.as_ref().unwrap())
    }

    fn dispatch_lut(
        &self,
        input: &[u8],
        width: u32,
        height: u32,
        pipeline: &CachedLutPipeline,
        lut_rgba: &[f32],
        lut_size: u32,
        intensity: f32,
    ) -> GpuExecResult {
        let device = &self.gpu.device;
        let queue = &self.gpu.queue;
        let byte_size = (width * height * 4) as u64;

        if input.len() as u64 != byte_size {
            return GpuExecResult::Fallback {
                reason: GpuFallbackReason::ExecutionFailed("input size mismatch".into()),
            };
        }

        // Input/output textures (same pattern as dispatch)
        let input_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lut_in"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &input_tex, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
            },
            input,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );

        let output_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lut_out"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        // 3D LUT texture
        let lut_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lut_3d_table"),
            size: wgpu::Extent3d { width: lut_size, height: lut_size, depth_or_array_layers: lut_size },
            mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        // Convert [f32; N*4] to bytes for upload
        let lut_bytes: &[u8] = bytemuck::cast_slice(lut_rgba);
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &lut_tex, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
            },
            lut_bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(lut_size * 16), // 4 × f32 = 16 bytes per texel
                rows_per_image: Some(lut_size),
            },
            wgpu::Extent3d { width: lut_size, height: lut_size, depth_or_array_layers: lut_size },
        );

        // Uniform buffer
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lut_uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform_buf, 0, bytemuck::cast_slice(&[intensity, lut_size as f32, 0.0f32, 0.0f32]));

        // Readback
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lut_readback"),
            size: byte_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        // Compute pipeline
        let compute_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("lut_compute"),
            layout: Some(&pipeline.pipeline_layout),
            module: &pipeline.shader,
            entry_point: "main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // Bind groups
        let input_view = input_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = output_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let lut_view = lut_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let bg0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lut_bg0"),
            layout: &pipeline.bgl_0,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&input_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&output_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&lut_view) },
            ],
        });

        let bg1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lut_bg1"),
            layout: &pipeline.bgl_1,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform_buf.as_entire_binding() },
            ],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("lut_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&compute_pipeline);
            cpass.set_bind_group(0, &bg0, &[]);
            cpass.set_bind_group(1, &bg1, &[]);
            cpass.dispatch_workgroups((width + 7) / 8, (height + 7) / 8, 1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &output_tex, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &readback,
                layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        queue.submit(Some(encoder.finish()));

        // Readback
        let (tx, rx) = std::sync::mpsc::channel();
        readback.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        device.poll(wgpu::Maintain::Wait);

        match rx.recv().unwrap_or(Err(wgpu::BufferAsyncError)) {
            Ok(()) => {
                let mapped = readback.slice(..).get_mapped_range();
                let result = mapped.to_vec();
                drop(mapped);
                readback.unmap();
                GpuExecResult::Success { data: result }
            }
            Err(e) => GpuExecResult::Fallback {
                reason: GpuFallbackReason::ExecutionFailed(e.to_string()),
            },
        }
    }

    fn dispatch(
        &self,
        input: &[u8],
        width: u32,
        height: u32,
        pipeline: &CachedPipeline,
        uniforms: &[f32; 4],
    ) -> GpuExecResult {
        let device = &self.gpu.device;
        let queue = &self.gpu.queue;
        let byte_size = (width * height * 4) as u64;

        if input.len() as u64 != byte_size {
            return GpuExecResult::Fallback {
                reason: GpuFallbackReason::ExecutionFailed("input size mismatch".into()),
            };
        }

        // Input texture
        let input_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gpu_in"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &input_tex, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
            },
            input,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );

        // Output texture
        let output_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gpu_out"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        // Uniform buffer
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gpu_uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform_buf, 0, bytemuck::cast_slice(uniforms));

        // Readback buffer
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gpu_readback"),
            size: byte_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        // Compute pipeline
        let compute_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("effect_compute"),
            layout: Some(&pipeline.pipeline_layout),
            module: &pipeline.shader,
            entry_point: "main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // Bind groups (per-dispatch: reference specific textures/buffers)
        let input_view = input_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = output_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let bg0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("effect_bg0"),
            layout: &pipeline.bgl_0,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&input_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&output_view) },
            ],
        });

        let bg1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("effect_bg1"),
            layout: &pipeline.bgl_1,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform_buf.as_entire_binding() },
            ],
        });

        // Single encoder: compute + copy
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("effect_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&compute_pipeline);
            cpass.set_bind_group(0, &bg0, &[]);
            cpass.set_bind_group(1, &bg1, &[]);
            cpass.dispatch_workgroups((width + 7) / 8, (height + 7) / 8, 1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &output_tex, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &readback,
                layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );

        queue.submit(Some(encoder.finish()));

        // Readback
        let (tx, rx) = std::sync::mpsc::channel();
        readback.slice(..).map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        device.poll(wgpu::Maintain::Wait);

        match rx.recv().unwrap_or(Err(wgpu::BufferAsyncError)) {
            Ok(()) => {
                let mapped = readback.slice(..).get_mapped_range();
                let result = mapped.to_vec();
                drop(mapped);
                readback.unmap();
                GpuExecResult::Success { data: result }
            }
            Err(e) => GpuExecResult::Fallback {
                reason: GpuFallbackReason::ExecutionFailed(e.to_string()),
            },
        }
    }
}

// ── EffectGpuExecutor implementation ─────────────────────────────────

impl EffectGpuExecutor for GpuBackend {
    fn try_execute_op(
        &self,
        op: &EffectRenderOp,
        input: &[u8],
        width: u32,
        height: u32,
    ) -> Option<Vec<u8>> {
        match op {
            EffectRenderOp::ColorAdjust { exposure, contrast, saturation } => {
                match self.execute_color_adjust(input, width, height, *exposure, *contrast, *saturation) {
                    GpuExecResult::Success { data } => Some(data),
                    GpuExecResult::Fallback { reason } => {
                        tracing::debug!("GPU ColorAdjust fallback: {}", reason.user_message());
                        None
                    }
                }
            }
            EffectRenderOp::GaussianBlur { radius } => {
                match self.execute_blur(input, width, height, *radius) {
                    GpuExecResult::Success { data } => Some(data),
                    GpuExecResult::Fallback { reason } => {
                        tracing::debug!("GPU Blur fallback: {}", reason.user_message());
                        None
                    }
                }
            }
            EffectRenderOp::Lut3D { lut, intensity } => {
                // Convert Vec<[f32; 3]> to Vec<f32> (RGBA with alpha=1.0)
                let lut_rgba: Vec<f32> = lut
                    .data
                    .iter()
                    .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 1.0f32])
                    .collect();
                match self.execute_lut3d(input, width, height, &lut_rgba, lut.size, *intensity) {
                    GpuExecResult::Success { data } => Some(data),
                    GpuExecResult::Fallback { reason } => {
                        tracing::debug!("GPU LUT3D fallback: {}", reason.user_message());
                        None
                    }
                }
            }
            _ => None,
        }
    }
}
