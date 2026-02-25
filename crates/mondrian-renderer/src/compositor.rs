//! 帧合成器（Frame Compositor）
//!
//! 将时间线中所有 ActiveClip 的解码帧合成为最终输出帧。

use crate::context::GpuContext;
use crate::pipeline::{CpuRgbaLayer, RenderPipeline};
use mondrian_core::types::*;
use std::sync::Arc;

pub struct CompositorConfig {
    pub output_resolution: Resolution,
    pub output_format: wgpu::TextureFormat,
}

impl Default for CompositorConfig {
    fn default() -> Self {
        Self {
            output_resolution: Resolution::FHD,
            output_format: wgpu::TextureFormat::Rgba8Unorm,
        }
    }
}

pub struct FrameCompositor {
    gpu: Arc<GpuContext>,
    pipeline: RenderPipeline,
    config: CompositorConfig,
    /// 输出帧纹理（复用，避免每帧重新分配）
    #[allow(dead_code)]
    output_texture: Option<wgpu::Texture>,
}

impl FrameCompositor {
    pub fn new(gpu: Arc<GpuContext>, config: CompositorConfig) -> Self {
        let pipeline =
            RenderPipeline::new(gpu.clone()).expect("failed to initialize render pipeline");
        Self { gpu, pipeline, config, output_texture: None }
    }

    /// 合成一帧（输入：已解码 RGBA 图层，输出：RGBA 像素）
    pub fn composite_frame(
        &mut self,
        width: u32,
        height: u32,
        layers: &[CpuRgbaLayer],
    ) -> mondrian_core::Result<Vec<u8>> {
        tracing::trace!(
            "Compositing frame at resolution {:?}",
            self.config.output_resolution
        );

        self.ensure_output_texture();
        self.pipeline.composite_layers_to_rgba(width, height, layers)
    }

    pub fn composite_rgba_layers(
        &mut self,
        width: u32,
        height: u32,
        layers: &[CpuRgbaLayer],
    ) -> mondrian_core::Result<Vec<u8>> {
        self.composite_frame(width, height, layers)
    }

    #[allow(dead_code)]
    fn ensure_output_texture(&mut self) {
        if self.output_texture.is_none() {
            let res = self.config.output_resolution;
            self.output_texture = Some(self.gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("compositor_output"),
                size: wgpu::Extent3d {
                    width: res.width,
                    height: res.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.config.output_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            }));
        }
    }
}
