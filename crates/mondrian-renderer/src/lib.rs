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
    CpuEncodedColorFrame, GpuColorFrameAllocationPlan, GpuColorFrameContract, GpuColorFrameHandle,
    GpuColorFrameHandleError, GpuColorFrameId, GpuColorFrameResource, GpuColorFrameResourceTable,
    GpuColorFrameResourceTableError, GpuColorFrameTextureFormat, GpuColorFrameUploadError,
    GpuColorFrameUploadPlan, GpuColorFrameUploader, GpuColorFrameWgpuResource,
};
pub use color_stage::{
    execute_cpu_input_stage, execute_cpu_output_stage, CpuRenderColorStageExecutor,
    RenderColorStage, RenderColorStageDiagnostics, RenderColorStageExecution, RenderColorStageMode,
    RenderColorStagePlan, RenderColorStagePlanner, RenderGpuColorPassExecutionError,
    RenderGpuColorPassInputView, RenderGpuColorPassResolvedResources, RenderGpuColorPassSchedule,
    RenderGpuColorPassScheduleError, RenderGpuColorPassTargetView,
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
    OcioGpuBindingContract, OcioGpuFullscreenWrapperContract, OcioGpuGeneratedProgramContract,
    OcioGpuGeneratedProgramDiagnostic, OcioGpuGeneratedProgramSourceKind,
    OcioGpuNagaShaderStageArtifact, OcioGpuShaderCache, OcioGpuShaderCacheDiagnostics,
    OcioGpuShaderDiagnostic, OcioGpuShaderError, OcioGpuShaderPlan, OcioGpuShaderRequest,
    OcioGpuShaderStage, OcioGpuShaderTargetLanguage, OcioGpuShaderTranslationCache,
    OcioGpuShaderTranslationCacheDiagnostics, OcioGpuShaderTranslationError,
    OcioGpuShaderTranslationFailure, OcioGpuShaderTranslationRequest, OcioGpuShaderTranslator,
    OcioGpuTexture2DBindingContract, OcioGpuTexture3DBindingContract, OcioGpuTranslatedShader,
    OcioGpuUniformBindingContract, OcioGpuWgpuBindGroupError,
    OcioGpuWgpuBindGroupLayoutDescriptorPlan, OcioGpuWgpuBindGroupLayoutEntryPlan,
    OcioGpuWgpuBindGroupPreparer, OcioGpuWgpuBindResource, OcioGpuWgpuBindResourceEntry,
    OcioGpuWgpuBindResourcePlan, OcioGpuWgpuBindResourcePlanError, OcioGpuWgpuBindingLayoutPlan,
    OcioGpuWgpuBindingLayoutPlanError, OcioGpuWgpuBindingPlan, OcioGpuWgpuBindingResource,
    OcioGpuWgpuBlocker, OcioGpuWgpuColorTargetFormat, OcioGpuWgpuExecutionPlan,
    OcioGpuWgpuFullscreenShaderContract, OcioGpuWgpuFullscreenTopology,
    OcioGpuWgpuLayoutBindingResource, OcioGpuWgpuLutTextureDimension, OcioGpuWgpuLutTextureExtent,
    OcioGpuWgpuLutTextureFormat, OcioGpuWgpuLutUploadError, OcioGpuWgpuLutUploadPlan,
    OcioGpuWgpuLutUploadResource, OcioGpuWgpuLutUploader, OcioGpuWgpuOcioBindGroup,
    OcioGpuWgpuPackedLutTexture, OcioGpuWgpuPackedLutUploadPlan, OcioGpuWgpuPackedUniformBuffer,
    OcioGpuWgpuPipelineBindGroupResource, OcioGpuWgpuPipelineBindGroupSlot,
    OcioGpuWgpuPipelineLayout, OcioGpuWgpuPipelineLayoutError, OcioGpuWgpuPipelineLayoutPlan,
    OcioGpuWgpuPipelineLayoutPreparer, OcioGpuWgpuPreparedResources, OcioGpuWgpuRenderPassError,
    OcioGpuWgpuRenderPassNodePlan, OcioGpuWgpuRenderPassRecorder, OcioGpuWgpuRenderPassTarget,
    OcioGpuWgpuRenderPipeline, OcioGpuWgpuRenderPipelineCache,
    OcioGpuWgpuRenderPipelineCacheDiagnostics, OcioGpuWgpuRenderPipelineDescriptorPlan,
    OcioGpuWgpuRenderPipelineError, OcioGpuWgpuResourceCache, OcioGpuWgpuResourceCacheDiagnostics,
    OcioGpuWgpuResourcePlan, OcioGpuWgpuSamplerBinding, OcioGpuWgpuSamplerBindingPolicy,
    OcioGpuWgpuSamplerFiltering, OcioGpuWgpuShaderModule, OcioGpuWgpuShaderModuleCache,
    OcioGpuWgpuShaderModuleCacheDiagnostics, OcioGpuWgpuShaderModuleError,
    OcioGpuWgpuShaderVisibility, OcioGpuWgpuTexture2DUpload, OcioGpuWgpuTexture3DUpload,
    OcioGpuWgpuTextureContractMismatch, OcioGpuWgpuTextureSampleType, OcioGpuWgpuUniformUpload,
    OcioGpuWgpuUniformUploadError, OcioGpuWgpuUniformUploadPlan, OcioGpuWgpuUniformUploadResource,
    OcioGpuWgpuUniformUploader, OcioGpuWgpuUploadedLutTexture, OcioGpuWgpuUploadedLuts,
    OcioGpuWgpuUploadedTextureMismatch, OcioGpuWgpuUploadedUniformBuffer,
    OcioGpuWgpuUploadedUniformMismatch, OcioGpuWgpuWrapperBindGroup,
    OcioGpuWgpuWrapperBindingEntry, OcioGpuWgpuWrapperBindingPlan,
    OcioGpuWgpuWrapperBindingResource, OcioGpuWgpuWrapperInputResources,
    OcioGpuWgpuWrapperLinkBlocker, OcioGpuWgpuWrapperLinkPlan,
    OcioGpuWgpuWrapperShaderArtifactError, OcioGpuWgpuWrapperShaderModuleArtifact,
    OcioGpuWgpuWrapperShaderModuleArtifactCache,
    OcioGpuWgpuWrapperShaderModuleArtifactCacheDiagnostics,
    OcioGpuWgpuWrapperShaderModuleArtifactError, OcioGpuWgpuWrapperShaderModuleCache,
    OcioGpuWgpuWrapperShaderModuleCacheDiagnostics, OcioGpuWgpuWrapperShaderModules,
    OcioGpuWgpuWrapperShaderSourceArtifact,
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
