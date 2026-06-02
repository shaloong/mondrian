//! Batched compositing pipeline — replaces per-layer submit with single submit.
//!
//! Key improvement over the legacy `RenderPipeline`: all layers are recorded
//! into a single command encoder before any GPU submission. Ping-pong render
//! targets are reused across frames via the texture pool.

use crate::context::GpuContext;
use crate::pipeline::CpuRgbaLayer;
use crate::texture_pool::TexturePool;
use bytemuck::{Pod, Zeroable};
use mondrian_core::{MondrianError, Result};
use std::sync::Arc;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CompositeUniforms {
    opacity: f32,
    blend_mode: u32,
    _padding: [f32; 2],
}

/// Batched compositing pipeline.
///
/// All layers are recorded into a single command encoder, then submitted
/// once. Texture creation is pooled for reuse across frames.
pub struct BatchedCompositor {
    gpu: Arc<GpuContext>,
    pipeline: wgpu::RenderPipeline,
    texture_bgl: wgpu::BindGroupLayout,
    uniform_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    texture_pool: Arc<TexturePool>,
    frame_counter: u64,
}

impl BatchedCompositor {
    pub fn new(gpu: Arc<GpuContext>, texture_pool: Arc<TexturePool>) -> Result<Self> {
        let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("batched_composite_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/composite.wgsl").into()),
        });

        let texture_bgl = gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("batch_tex_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let uniform_bgl = gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("batch_uniform_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let layout = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("batch_composite_layout"),
            bind_group_layouts: &[&texture_bgl, &uniform_bgl],
            push_constant_ranges: &[],
        });

        let pipeline = gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("batch_composite_pipeline"),
            layout: Some(&layout),
            cache: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("batch_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let uniform_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("batch_uniform_buf"),
            size: std::mem::size_of::<CompositeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            gpu,
            pipeline,
            texture_bgl,
            uniform_bgl,
            sampler,
            uniform_buffer,
            texture_pool,
            frame_counter: 0,
        })
    }

    /// Composite layers into a single RGBA8 buffer using one GPU submission.
    pub fn composite_layers_to_rgba(
        &mut self,
        output_width: u32,
        output_height: u32,
        layers: &[CpuRgbaLayer],
    ) -> Result<Vec<u8>> {
        let width = output_width.max(1);
        let height = output_height.max(1);

        if layers.is_empty() {
            return Ok(vec![0; width as usize * height as usize * 4]);
        }

        self.frame_counter += 1;
        self.texture_pool.advance_frame();

        // Acquire ping-pong render targets from pool
        let rt_usage = wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST;

        let accum_a = self.texture_pool.acquire(
            &self.gpu.device,
            width,
            height,
            wgpu::TextureFormat::Rgba8Unorm,
            rt_usage,
        );
        let accum_b = self.texture_pool.acquire(
            &self.gpu.device,
            width,
            height,
            wgpu::TextureFormat::Rgba8Unorm,
            rt_usage,
        );

        // Single encoder for all layers + readback
        let mut encoder = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("batch_composite_encoder"),
        });

        // Clear accum_a to transparent black (alpha=0). The first composited
        // layer will overlay onto this, producing correct results for the
        // Porter-Duff "Over" blend used by composite.wgsl.
        {
            let view = accum_a.create_view(&wgpu::TextureViewDescriptor::default());
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("batch_clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }

        // Composite each layer into the ping-pong targets — all in the SAME encoder
        let mut src_is_a = true;
        for layer in layers {
            if layer.width == 0 || layer.height == 0 {
                continue;
            }

            let layer_tex = self.upload_layer_texture(layer, width, height)?;
            let src = if src_is_a { &accum_a } else { &accum_b };
            let dst = if src_is_a { &accum_b } else { &accum_a };

            self.record_composite_pass(&mut encoder, src, dst, &layer_tex, layer.opacity);

            // Return layer texture to pool
            self.texture_pool.release(layer_tex, width, height);
            src_is_a = !src_is_a;
        }

        let final_tex = if src_is_a { &accum_a } else { &accum_b };

        // Readback from final texture
        let readback_data = self.record_readback(&mut encoder, final_tex, width, height)?;

        // Single submission for the entire frame
        self.gpu.queue.submit(Some(encoder.finish()));

        // Return render targets to pool
        self.texture_pool.release(accum_a, width, height);
        self.texture_pool.release(accum_b, width, height);

        // Wait for GPU and map readback
        self.wait_and_map_readback(readback_data, width, height)
    }

    fn upload_layer_texture(
        &self,
        layer: &CpuRgbaLayer,
        output_width: u32,
        output_height: u32,
    ) -> Result<wgpu::Texture> {
        if layer.data.len() != (layer.width as usize * layer.height as usize * 4) {
            return Err(MondrianError::TextureUploadFailed {
                reason: "layer rgba buffer size mismatch".to_string(),
            });
        }

        let tex_usage = wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING;
        let texture = self.texture_pool.acquire(
            &self.gpu.device,
            output_width,
            output_height,
            wgpu::TextureFormat::Rgba8Unorm,
            tex_usage,
        );

        // Pad layer data to output dimensions
        let copy_w = layer.width.min(output_width) as usize;
        let copy_h = layer.height.min(output_height) as usize;
        let src_stride = layer.width as usize * 4;
        let dst_stride = output_width as usize * 4;
        let mut staged = vec![0u8; output_width as usize * output_height as usize * 4];
        for y in 0..copy_h {
            let src_start = y * src_stride;
            let dst_start = y * dst_stride;
            staged[dst_start..dst_start + copy_w * 4]
                .copy_from_slice(&layer.data[src_start..src_start + copy_w * 4]);
        }

        self.gpu.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &staged,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(output_width * 4),
                rows_per_image: Some(output_height),
            },
            wgpu::Extent3d {
                width: output_width,
                height: output_height,
                depth_or_array_layers: 1,
            },
        );

        Ok(texture)
    }

    fn record_composite_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        src_accum: &wgpu::Texture,
        dst_accum: &wgpu::Texture,
        layer_tex: &wgpu::Texture,
        opacity: f32,
    ) {
        let src_view = src_accum.create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = dst_accum.create_view(&wgpu::TextureViewDescriptor::default());
        let layer_view = layer_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let tex_bg = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("batch_tex_bg"),
            layout: &self.texture_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&layer_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let uniforms = CompositeUniforms {
            opacity: opacity.clamp(0.0, 1.0),
            blend_mode: 0,
            _padding: [0.0, 0.0],
        };
        self.gpu
            .queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let uniform_bg = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("batch_uniform_bg"),
            layout: &self.uniform_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: self.uniform_buffer.as_entire_binding(),
            }],
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("batch_composite_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &tex_bg, &[]);
            pass.set_bind_group(1, &uniform_bg, &[]);
            pass.draw(0..4, 0..1);
        }
    }

    fn record_readback(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Buffer> {
        let bytes_per_pixel = 4u32;
        let unpadded = width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let buffer_size = padded as u64 * height as u64;

        let readback = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("batch_readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &readback,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );

        Ok(readback)
    }

    fn wait_and_map_readback(
        &self,
        readback: wgpu::Buffer,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>> {
        let (tx, rx) = std::sync::mpsc::channel();
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        self.gpu.device.poll(wgpu::Maintain::Wait);

        rx.recv()
            .map_err(|_| MondrianError::TextureUploadFailed {
                reason: "readback channel dropped".into(),
            })?
            .map_err(|e| MondrianError::TextureUploadFailed {
                reason: format!("readback map failed: {e}"),
            })?;

        let padded = {
            let unpadded = width * 4;
            let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            unpadded.div_ceil(align) * align
        };

        let mapped = slice.get_mapped_range();
        let mut out = vec![0u8; width as usize * height as usize * 4];
        for row in 0..height as usize {
            let src = row * padded as usize;
            let dst = row * width as usize * 4;
            out[dst..dst + width as usize * 4]
                .copy_from_slice(&mapped[src..src + width as usize * 4]);
        }
        drop(mapped);
        readback.unmap();

        Ok(out)
    }
}
