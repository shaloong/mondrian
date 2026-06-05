//! UI 渲染器 —— 将 DrawCommand 提交到 GPU
//!
//! [`UiRenderer`] 持有 wgpu 渲染管线，接收 DrawCommand 列表并渲染到纹理。

use std::sync::Arc;

use bytemuck::Pod;
use wgpu::util::DeviceExt;

use crate::batch::build_batches;
use crate::command::DrawCommand;
use crate::pipeline::UiPipeline;
use crate::shape::RectVertex;

/// 统一缓冲区数据（CPU→GPU）
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, bytemuck::Zeroable)]
struct Uniforms {
    screen_size: [f32; 2],
    _pad: [f32; 2],
}

/// GPU 2D UI 渲染器
///
/// 接收 DrawCommand 序列，批次化后通过 wgpu 渲染管线提交到纹理。
pub struct UiRenderer {
    pipeline: UiPipeline,
    sampler: wgpu::Sampler,
}

impl UiRenderer {
    /// 创建新的 UI 渲染器
    ///
    /// `surface_format` 应与输出纹理的格式匹配。
    /// 复用 `mondrian-renderer::GpuContext` 的 device。
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let pipeline = UiPipeline::new(device, surface_format);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ui_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self { pipeline, sampler }
    }

    /// 将绘制命令渲染到指定的纹理视图
    ///
    /// `device` 和 `queue` 来自 `GpuContext`。
    /// `view` 是渲染目标（通常是 surface texture 或离屏纹理）。
    /// `commands` 是 DrawEncoder 产出的命令列表。
    /// `screen_size` 是绘制区域的像素尺寸。
    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        commands: &[DrawCommand],
        screen_size: (u32, u32),
    ) {
        let batches = build_batches(commands, screen_size);

        let uniform_data = Uniforms {
            screen_size: [screen_size.0 as f32, screen_size.1 as f32],
            _pad: [0.0; 2],
        };
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ui_uniform"),
            contents: bytemuck::bytes_of(&uniform_data),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui_bg"),
            layout: &self.pipeline.bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ui_encoder") });

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            rpass.set_pipeline(&self.pipeline.render_pipeline);
            rpass.set_bind_group(0, &bind_group, &[]);

            for batch in &batches {
                if batch.vertices.is_empty() {
                    continue;
                }

                let vertex_data: &[RectVertex] = &batch.vertices;
                let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("ui_vb"),
                    contents: bytemuck::cast_slice(vertex_data),
                    usage: wgpu::BufferUsages::VERTEX,
                });

                rpass.set_vertex_buffer(0, vertex_buffer.slice(..));
                rpass.draw(0..vertex_data.len() as u32, 0..1);
            }
        }

        queue.submit(std::iter::once(encoder.finish()));
    }
}
