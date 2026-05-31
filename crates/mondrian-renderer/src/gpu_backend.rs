//! GPU compute backend for effect processing.
//!
//! Provides compute-shader acceleration for supported effects with
//! automatic CPU fallback. Each effect retains its CPU implementation —
//! the GPU path is a transparent optimization, never a hard requirement.

use crate::context::GpuContext;
use crate::shaders;
use mondrian_effects::{EffectGpuExecutor, EffectRenderOp};
use parking_lot::Mutex;
use std::sync::Arc;
use wgpu;

// ── Public types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuEffectKind {
    ColorAdjust,
    GaussianBlur,
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

// ── GpuBackend ────────────────────────────────────────────────────────

pub struct GpuBackend {
    gpu: Arc<GpuContext>,
    available: bool,
    /// Cached shader modules (compiled once, reused).
    color_adjust_shader: Mutex<Option<wgpu::ShaderModule>>,
    blur_shader: Mutex<Option<wgpu::ShaderModule>>,
}

impl GpuBackend {
    pub async fn new() -> Option<Arc<Self>> {
        let gpu = GpuContext::new().await.ok()?;
        tracing::info!("GPU backend initialized: {:?}", gpu.adapter.get_info());
        Some(Arc::new(Self {
            gpu,
            available: true,
            color_adjust_shader: Mutex::new(None),
            blur_shader: Mutex::new(None),
        }))
    }

    pub fn is_available(&self) -> bool {
        self.available
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
        self.dispatch(
            input, width, height,
            &self.color_adjust_shader,
            shaders::COLOR_ADJUST_COMPUTE,
            "color_adjust",
            &[exposure, contrast, saturation, 0.0],
        )
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
        // Horizontal pass
        let horiz = self.dispatch(
            input, width, height,
            &self.blur_shader,
            shaders::BLUR_GAUSSIAN_COMPUTE,
            "blur",
            &[radius, 1.0, 0.0, 0.0],
        );
        let horiz_data = match horiz {
            GpuExecResult::Success { data } => data,
            other => return other,
        };
        // Vertical pass
        self.dispatch(
            &horiz_data, width, height,
            &self.blur_shader,
            shaders::BLUR_GAUSSIAN_COMPUTE,
            "blur",
            &[radius, 0.0, 1.0, 0.0],
        )
    }

    // ── Internal ──────────────────────────────────────────────────

    fn dispatch(
        &self,
        input: &[u8],
        width: u32,
        height: u32,
        _shader_cache: &Mutex<Option<wgpu::ShaderModule>>,
        shader_src: &str,
        shader_label: &str,
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

        // Shader module
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(shader_label),
            source: wgpu::ShaderSource::Wgsl(shader_src.into()),
        });

        // Bind group layouts
        let bgl_0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl_0"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
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
            label: Some("bgl_1"),
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

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("layout"),
            bind_group_layouts: &[&bgl_0, &bgl_1],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(shader_label),
            layout: Some(&layout),
            module: &shader,
            entry_point: "main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // Bind groups
        let input_view = input_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = output_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let bg0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg0"),
            layout: &bgl_0,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&input_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&output_view) },
            ],
        });

        let bg1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg1"),
            layout: &bgl_1,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform_buf.as_entire_binding() },
            ],
        });

        // Compute dispatch
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("effect_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&pipeline);
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
            _ => None,
        }
    }
}
