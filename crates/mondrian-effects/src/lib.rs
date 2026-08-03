//! # mondrian-effects
//!
//! 高级效果系统：LUT 调色 / 滤镜 / 转场 / 蒙版

pub mod adjustment;
pub mod effect;
pub mod execution;
pub mod execution_contract;
pub mod execution_planning;
mod execution_session;
pub mod gpu_plan;
pub mod graph;
pub mod heterogeneous_execution;
pub mod lut;
pub mod mask;
pub mod mask_raster;
pub mod plugin_contract;
pub mod plugin_sdk;
pub mod prepared;
pub mod temporal_execution;
pub mod transition;

pub use adjustment::{
    blend_rgba_f32_pixel, blend_rgba_f32_pixel_seeded, blend_rgba_pixel, blend_rgba_pixel_seeded,
};
pub use effect::{
    build_effect_render_graph, compile_clip_effect_graph, effect_category_tree, effect_definition,
    effect_display_name, effect_library_types, effect_registry_revision,
    register_effect_definition, CustomEffectProcessorBinding, EffectCacheKeyBuilder,
    EffectCachePolicy, EffectCategoryNode, EffectColorDomain, EffectColorDomainContract,
    EffectDefinition, EffectEvalContext, EffectGraphBuildError, EffectGraphBuilder,
    EffectGraphPreparer, EffectNode, EffectNodeExt, EffectPreparationContext, EffectRenderOp,
    EffectRenderParamsBuilder, EffectRenderPlan, EffectResourceDependency, EffectResourceRecovery,
    EffectType, PreparedEffectEvaluator, PreparedLut3D,
};
pub use execution::{
    apply_compiled_effect_graph, apply_compiled_effect_graph_pass,
    apply_compiled_effect_graph_pass_rgba_f32,
    apply_compiled_effect_graph_pass_rgba_f32_with_domain_processor,
    apply_compiled_effect_graph_rgba_f32,
    apply_compiled_effect_graph_rgba_f32_with_domain_processor,
    compiled_effect_graph_has_resolvable_rgba_f32_domain,
    compiled_effect_graph_has_rgba_f32_execution_shape, compiled_effect_graph_supports_rgba_f32,
    compiled_effect_graph_supports_rgba_f32_with_domain_processor, CustomEffectRenderProcessor,
    EffectDomainProcessorCacheKey, EffectExecutionError, EffectFloatExecutionError,
    EffectFloatUnsupportedReason,
};
pub use execution_contract::{
    EffectDeterminism, EffectExecutionAdmissionError, EffectExecutionContract,
    EffectExecutionContractError, EffectExecutionContractViolation, EffectExecutionEnvelope,
    EffectExecutionMode, EffectExecutionModes, EffectGraphTopology, EffectProcessingBackend,
    EffectProcessingBackends, EffectResourceLifetime, EffectRoiPropagation, EffectStateModel,
    EffectTemporalInputExtent, EffectTemporalSpan, EffectWorkingPrecision,
};
pub use execution_planning::{
    EffectExecutionDemand, EffectExecutionDemandError, EffectExecutionEnvironment,
    EffectExecutionEnvironmentError, EffectExecutionLane, EffectExecutionLaneId,
    EffectExecutionModeObligation, EffectExecutionObligations, EffectExecutionTransfer,
    EffectFrameExtent, EffectInputRoi, EffectLinearStagePlacement, EffectLinearStagePlacementError,
    EffectLinearStagePlacementStep, EffectPixelRoi, EffectRoiHalo, EffectTemporalBoundary,
    EffectTemporalDirection, EffectTemporalWindow, EffectWorkingPrecisions,
};
pub use execution_session::{
    EffectExecutionSession, EffectExecutionSessionConfig, EffectExecutionSessionDiagnostics,
};
pub use gpu_plan::{
    get_or_lower_effect_graph_to_gpu_plan, lower_effect_graph_nodes_to_gpu_plan,
    lower_effect_graph_to_gpu_plan, CompiledEffectGpuPlan, EffectGpuPlanBlocker, EffectGpuPointOp,
    MAX_FUSED_GPU_EFFECT_OPS,
};
pub use graph::{
    compile_reference_effect_graph, compile_reference_effect_graph_in_domain,
    compile_reference_render_graph, identity_compiled_effect_graph, prepare_effect_graph_topology,
    CompiledEffectDomainPlan, CompiledEffectGraph, CompiledEffectStageBinding, EffectDomainBlocker,
    EffectDomainBlockerKind, EffectDomainTransition, EffectGraphBuilderState, EffectGraphNode,
    EffectGraphNodeId, EffectGraphNodeKind, EffectGraphValue, EffectRenderGraph,
    PreparedEffectGraphTopology,
};
pub use heterogeneous_execution::{
    plan_effect_graph_value_execution, CompiledEffectValueExecutionPlan, EffectCompletionToken,
    EffectGraphExecutionBudget, EffectGraphExecutionBudgetKind, EffectGraphExecutionPlanError,
    EffectGraphExecutionRequest, EffectGraphExecutionStep, EffectMaterializationId,
    EffectValueFormat, EffectValueMaterialization, EffectValueResidency,
    HeterogeneousCpuCompletionEvidence, HeterogeneousCpuExecutionStopReason,
    PreparedHeterogeneousCpuCompletion, PreparedHeterogeneousEffectWork,
    PreparedHeterogeneousEffectWorkError,
};
pub use lut::{
    Lut3D, LutLibrary, LutLibraryEntry, LutPreparationCache, LutPreparationCacheConfig,
    LutPreparationCacheDiagnostics,
};
pub use mask::{
    BezierPoint, MaskComponent, MaskId, MaskKeyframe, MaskOp, MaskShape, MAX_MASK_PATH_POINTS,
};
pub use mask_raster::{rasterize_mask_shape, MaskRasterError, PreparedMaskRaster};
pub use plugin_contract::{
    effect_plugin_runtime_status, EffectPluginApiVersion, EffectPluginContract,
    EffectPluginLibraryPolicy, EffectPluginRuntimeFailurePolicy, EffectPluginRuntimeStatus,
    CURRENT_EFFECT_PLUGIN_API_VERSION,
};
pub use plugin_sdk::{EffectGraphDsl, EffectPluginDefinitionBuilder};
pub use prepared::{
    EffectDependencyCheckError, EffectProgramDependencyIdentity, PreparedEffectProgram,
    PreparedEffectStack,
};
pub use temporal_execution::{
    collect_temporal_frame_demands, prepare_temporal_frame_execution, EffectExecutionContinuity,
    EffectFrameTileF32, EffectTemporalExecutionError, EffectTemporalExecutionOutput,
    EffectTemporalExecutionRequest, EffectTemporalFrameDemandBatch, EffectTemporalFrameProvider,
    EffectTemporalFrameProviderError, EffectTemporalFrameRequest, EffectTemporalSourceIdentity,
    PreparedEffectTemporalExecution, PreparedTemporalFrameSet, PreparedTemporalFrameSetError,
};
pub use transition::Transition;
