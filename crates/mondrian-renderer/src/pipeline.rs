//! 渲染管线（Shader 编译与管线对象）

use crate::context::GpuContext;
use bytemuck::{Pod, Zeroable};
use mondrian_core::{MondrianError, Result};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct CpuRgbaLayer {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub opacity: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CompositeUniforms {
    opacity: f32,
    blend_mode: u32,
    _padding: [f32; 2],
}

pub struct RenderPipeline {
    gpu: Arc<GpuContext>,
    composite_pipeline: wgpu::RenderPipeline,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    uniform_bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
}

impl RenderPipeline {
    pub fn new(gpu: Arc<GpuContext>) -> Result<Self> {
        let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("renderer_composite_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/composite.wgsl").into()),
        });

        let texture_bind_group_layout =
            gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("composite_texture_bgl"),
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

        let uniform_bind_group_layout =
            gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("composite_uniform_bgl"),
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

        let pipeline_layout = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("composite_pipeline_layout"),
            immediate_size: 0,
            bind_group_layouts: &[
                Some(&texture_bind_group_layout),
                Some(&uniform_bind_group_layout),
            ],
        });

        let composite_pipeline =
            gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                cache: None,
                multiview_mask: None,
                label: Some("composite_pipeline"),
                layout: Some(&pipeline_layout),

                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
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
            });

        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("composite_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let uniform_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("composite_uniform_buffer"),
            size: std::mem::size_of::<CompositeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            gpu,
            composite_pipeline,
            texture_bind_group_layout,
            uniform_bind_group_layout,
            sampler,
            uniform_buffer,
        })
    }

    pub fn composite_layers_to_rgba(
        &self,
        output_width: u32,
        output_height: u32,
        layers: &[CpuRgbaLayer],
    ) -> Result<Vec<u8>> {
        let width = output_width.max(1);
        let height = output_height.max(1);

        if layers.is_empty() {
            return Ok(vec![0; width as usize * height as usize * 4]);
        }

        let accum_a = self.create_render_target(width, height, "accum_a");
        let accum_b = self.create_render_target(width, height, "accum_b");

        self.clear_texture(&accum_a, width, height, 1.0);

        let mut src_accum_is_a = true;
        for layer in layers {
            if layer.width == 0 || layer.height == 0 {
                continue;
            }

            let src_texture = self.create_layer_texture(layer, width, height)?;
            let dst_target = if src_accum_is_a { &accum_b } else { &accum_a };
            let src_target = if src_accum_is_a { &accum_a } else { &accum_b };
            self.draw_composite_pass(src_target, dst_target, &src_texture, layer.opacity);
            src_accum_is_a = !src_accum_is_a;
        }

        let final_texture = if src_accum_is_a { &accum_a } else { &accum_b };
        self.readback_rgba(final_texture, width, height)
    }

    fn create_render_target(&self, width: u32, height: u32, label: &str) -> wgpu::Texture {
        self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    fn clear_texture(&self, texture: &wgpu::Texture, width: u32, height: u32, alpha: f64) {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clear_texture_encoder"),
        });

        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear_texture_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: alpha }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        self.gpu.queue.submit([encoder.finish()]);
        let _ = self
            .gpu
            .device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None });

        let _ = (width, height);
    }

    fn create_layer_texture(
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

        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("layer_rgba_texture"),
            size: wgpu::Extent3d {
                width: output_width,
                height: output_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        let mut staged = vec![0u8; output_width as usize * output_height as usize * 4];
        let copy_w = layer.width.min(output_width) as usize;
        let copy_h = layer.height.min(output_height) as usize;
        let src_stride = layer.width as usize * 4;
        let dst_stride = output_width as usize * 4;
        for y in 0..copy_h {
            let src_start = y * src_stride;
            let src_end = src_start + copy_w * 4;
            let dst_start = y * dst_stride;
            let dst_end = dst_start + copy_w * 4;
            staged[dst_start..dst_end].copy_from_slice(&layer.data[src_start..src_end]);
        }

        self.gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &staged,
            wgpu::TexelCopyBufferLayout {
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

    fn draw_composite_pass(
        &self,
        current_accum: &wgpu::Texture,
        next_accum: &wgpu::Texture,
        layer_texture: &wgpu::Texture,
        opacity: f32,
    ) {
        let current_view = current_accum.create_view(&wgpu::TextureViewDescriptor::default());
        let next_view = next_accum.create_view(&wgpu::TextureViewDescriptor::default());
        let layer_view = layer_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let texture_bind_group = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composite_texture_bg"),
            layout: &self.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&layer_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&current_view),
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

        let uniform_bind_group = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composite_uniform_bg"),
            layout: &self.uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: self.uniform_buffer.as_entire_binding(),
            }],
        });

        let mut encoder = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("composite_pass_encoder"),
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &next_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.composite_pipeline);
            pass.set_bind_group(0, &texture_bind_group, &[]);
            pass.set_bind_group(1, &uniform_bind_group, &[]);
            pass.draw(0..4, 0..1);
        }

        self.gpu.queue.submit([encoder.finish()]);
        let _ = self
            .gpu
            .device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
    }

    fn readback_rgba(&self, texture: &wgpu::Texture, width: u32, height: u32) -> Result<Vec<u8>> {
        let bytes_per_pixel = 4u32;
        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
        let buffer_size = padded_bytes_per_row as u64 * height as u64;

        let readback = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("composite_readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("composite_readback_encoder"),
        });

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );

        self.gpu.queue.submit([encoder.finish()]);

        let (tx, rx) = std::sync::mpsc::channel();
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        let _ = self
            .gpu
            .device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None });

        match rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                return Err(MondrianError::TextureUploadFailed {
                    reason: format!("gpu readback map failed: {err}"),
                });
            }
            Err(err) => {
                return Err(MondrianError::TextureUploadFailed {
                    reason: format!("gpu readback channel failed: {err}"),
                });
            }
        }

        let mapped = slice.get_mapped_range().expect("gpu readback mapped range");
        let mut out = vec![0u8; (width as usize) * (height as usize) * 4];
        for row in 0..height as usize {
            let src_start = row * padded_bytes_per_row as usize;
            let src_end = src_start + unpadded_bytes_per_row as usize;
            let dst_start = row * unpadded_bytes_per_row as usize;
            let dst_end = dst_start + unpadded_bytes_per_row as usize;
            out[dst_start..dst_end].copy_from_slice(&mapped[src_start..src_end]);
        }
        drop(mapped);
        readback.unmap();

        Ok(out)
    }
}
