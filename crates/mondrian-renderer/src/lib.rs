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
pub mod gpu_backend;
pub mod pipeline;
pub mod shaders;
pub mod timeline_composite;
pub mod timeline_render_plan;

pub use compositor::{CompositorConfig, FrameCompositor};
pub use context::GpuContext;
pub use gpu_backend::{GpuBackend, GpuEffectKind, GpuExecResult, GpuFallbackReason};
pub use pipeline::{CpuRgbaLayer, RenderPipeline};
pub use timeline_composite::{
    composite_timeline_elements, composite_timeline_elements_float_linear,
    composite_timeline_elements_into, is_identity_transform, quantize_transform_signature,
    TimelineAdjustmentLayer, TimelineCompositeElement, TimelineCompositeOptions,
    TimelineCompositeScratch, TimelineMediaLayer, TimelineSolidColorLayer,
};
pub use timeline_render_plan::{
    build_timeline_render_plan, collect_timeline_color_diagnostics, mat3_to_affine,
    TimelineAdjustmentPlan, TimelineColorDiagnostic, TimelineMediaPlan, TimelineNestedSequencePlan,
    TimelineRenderPlanElement, TimelineSolidColorPlan,
};
