//! # mondrian-renderer
//!
//! GPU 渲染引擎（基于 wgpu）。
//!
//! 提供：
//! - `GpuContext`：wgpu Device/Queue/Adapter 管理
//! - `FrameCompositor`：多层实时帧合成
//! - `RenderPipeline`：YUV→RGB + 层混合 Shader 管线
//! - `ShaderRegistry`：可扩展效果 Shader 注册

pub mod batched_pipeline;
pub mod color_frame;
pub mod color_stage;
pub mod color_transform;
pub mod compositor;
pub mod context;
pub mod gpu_backend;
pub mod ocio_gpu;
pub mod pipeline;
pub mod profile;
pub mod shaders;
pub mod texture_pool;
pub mod timeline_composite;
pub mod timeline_render_plan;

pub use color_frame::{
    ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency, CpuColorFrame,
    CpuEncodedColorFrame, GpuColorFrameHandle, GpuColorFrameHandleError, GpuColorFrameId,
    GpuColorFrameTextureFormat,
};
pub use color_stage::{
    execute_cpu_input_stage, execute_cpu_output_stage, CpuRenderColorStageExecutor,
    RenderColorStage, RenderColorStageDiagnostics, RenderColorStageExecution, RenderColorStageMode,
    RenderColorStagePlan, RenderColorStagePlanner,
};
pub use color_transform::{
    CpuColorTransformExecutor, RenderColorTransform, RenderColorTransformBackend,
    RenderColorTransformDiagnostics, RenderColorTransformDirection, RenderColorTransformError,
    RenderColorTransformGpuOptions, RenderColorTransformGpuPlan, RenderColorTransformGpuPlanner,
    RenderInputTransform, RenderInputTransformResult, RenderOutputTransformResult,
};
pub use compositor::{CompositorConfig, FrameCompositor};
pub use context::GpuContext;
pub use gpu_backend::{
    gpu_enabled, set_gpu_enabled, GpuBackend, GpuEffectKind, GpuExecResult, GpuFallbackReason,
};
pub use ocio_gpu::{
    OcioGpuShaderCache, OcioGpuShaderCacheDiagnostics, OcioGpuShaderError, OcioGpuShaderPlan,
    OcioGpuShaderRequest, OcioGpuWgpuBlocker, OcioGpuWgpuExecutionPlan, OcioGpuWgpuResourcePlan,
};
pub use pipeline::{CpuRgbaLayer, RenderPipeline};
pub use timeline_composite::{
    composite_timeline_elements, composite_timeline_elements_color_frame,
    composite_timeline_elements_into, is_identity_transform, quantize_transform_signature,
    TimelineAdjustmentLayer, TimelineCompositeElement, TimelineCompositeOptions,
    TimelineCompositeScratch, TimelineMediaLayer, TimelineSolidColorLayer,
};
pub use timeline_render_plan::{
    collect_timeline_color_diagnostics, evaluate_timeline_render_plan, mat3_to_affine,
    TimelineAdjustmentPlan, TimelineColorDiagnostic, TimelineEvaluationDiagnostics,
    TimelineEvaluationRequest, TimelineMediaPlan, TimelineNestedSequencePlan,
    TimelineRenderColorTarget, TimelineRenderIntent, TimelineRenderPlan, TimelineRenderPlanElement,
    TimelineRenderQuality, TimelineRenderSettings, TimelineSolidColorPlan,
};
