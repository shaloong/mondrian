//! 帧合成器（Frame Compositor）
//!
//! 将时间线中所有 ActiveClip 的解码帧合成为最终输出帧。
//!
//! Phase 4: Uses batched pipeline with single GPU submission and texture pooling.

use crate::batched_pipeline::BatchedCompositor;
use crate::context::GpuContext;
use crate::pipeline::CpuRgbaLayer;
use crate::texture_pool::TexturePool;
use mondrian_core::types::*;
use std::sync::Arc;

pub struct CompositorConfig {
    pub output_resolution: Resolution,
    pub output_format: wgpu::TextureFormat,
    /// Maximum pooled textures per size class.
    pub texture_pool_per_key: usize,
    /// Maximum total pooled textures.
    pub texture_pool_max_total: usize,
}

impl Default for CompositorConfig {
    fn default() -> Self {
        Self {
            output_resolution: Resolution::FHD,
            output_format: wgpu::TextureFormat::Rgba8Unorm,
            texture_pool_per_key: 8,
            texture_pool_max_total: 64,
        }
    }
}

pub struct FrameCompositor {
    batched: BatchedCompositor,
    texture_pool: Arc<TexturePool>,
    _config: CompositorConfig,
}

impl FrameCompositor {
    pub fn new(gpu: Arc<GpuContext>, config: CompositorConfig) -> Self {
        let texture_pool = Arc::new(TexturePool::new(
            config.texture_pool_per_key,
            config.texture_pool_max_total,
        ));
        let batched = BatchedCompositor::new(gpu, Arc::clone(&texture_pool))
            .expect("failed to initialize batched compositor");

        Self { batched, texture_pool, _config: config }
    }

    /// 合成一帧（输入：已解码 RGBA 图层，输出：RGBA 像素）
    ///
    /// All layers are composited in a single GPU submission. Texture
    /// resources are pooled for reuse across frames.
    pub fn composite_frame(
        &mut self,
        width: u32,
        height: u32,
        layers: &[CpuRgbaLayer],
    ) -> mondrian_core::Result<Vec<u8>> {
        tracing::trace!(
            width,
            height,
            num_layers = layers.len(),
            pooled_textures = self.texture_pool.len(),
            "Batched frame composite"
        );
        self.batched.composite_layers_to_rgba(width, height, layers)
    }

    pub fn composite_rgba_layers(
        &mut self,
        width: u32,
        height: u32,
        layers: &[CpuRgbaLayer],
    ) -> mondrian_core::Result<Vec<u8>> {
        self.composite_frame(width, height, layers)
    }

    /// Composite layers into a wgpu texture (GPU-only, no CPU readback).
    /// Returns a non-pooled texture owned by the caller.
    pub fn composite_rgba_layers_to_texture(
        &mut self,
        width: u32,
        height: u32,
        layers: &[CpuRgbaLayer],
    ) -> mondrian_core::Result<wgpu::Texture> {
        self.batched.composite_layers_to_texture(width, height, layers)
    }

    /// Access the wgpu device for texture creation.
    pub fn device(&self) -> &wgpu::Device {
        self.batched.device()
    }

    /// Access the wgpu queue for texture uploads.
    pub fn queue(&self) -> &wgpu::Queue {
        self.batched.queue()
    }

    /// Return number of pooled textures (for dev metrics).
    pub fn pooled_texture_count(&self) -> usize {
        self.texture_pool.len()
    }
}
