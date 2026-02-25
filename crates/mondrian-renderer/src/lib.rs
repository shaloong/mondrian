//! # mondrian-renderer
//!
//! GPU 渲染引擎（基于 wgpu）。
//!
//! 提供：
//! - `GpuContext`：wgpu Device/Queue/Adapter 管理
//! - `FrameCompositor`：多层实时帧合成
//! - `RenderPipeline`：YUV→RGB + 层混合 Shader 管线
//! - `ShaderRegistry`：可扩展效果 Shader 注册

pub mod compositor;
pub mod context;
pub mod pipeline;
pub mod shaders;

pub use compositor::{CompositorConfig, FrameCompositor};
pub use context::GpuContext;
pub use pipeline::{CpuRgbaLayer, RenderPipeline};
