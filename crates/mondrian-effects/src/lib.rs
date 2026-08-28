//! # mondrian-effects
//!
//! 高级效果系统：LUT 调色 / 滤镜 / 转场 / 蒙版

pub mod adjustment;
pub mod color_curves;
pub mod coverage;
pub mod effect;
pub mod execution;
pub mod execution_contract;
pub mod execution_planning;
mod execution_session;
pub mod gamut_mapping;
pub mod gpu_plan;
pub mod graph;
pub mod hdr_grading;
pub mod heterogeneous_execution;
pub mod lut;
pub mod mask;
pub mod mask_raster;
pub mod plugin_contract;
pub mod plugin_sdk;
pub mod prepared;
pub mod primary_grade;
pub mod qualifier;
pub mod temporal_execution;
pub mod tracking;
pub mod transition;

pub use adjustment::{
    blend_rgba_f32_pixel, blend_rgba_f32_pixel_seeded, blend_rgba_pixel, blend_rgba_pixel_seeded,
};
pub use color_curves::{
    ColorCurvesAuthoring, ColorCurvesMode, PreparedColorCurves, COLOR_CURVE_SAMPLE_COUNT,
    COLOR_CURVE_SAMPLE_ROWS,
};
pub use coverage::{has_positive_coverage, mix_straight_rgba, straight_rgba_from_premultiplied};
pub use effect::{
    build_effect_render_graph, compile_clip_effect_graph, effect_category_tree, effect_definition,
    effect_display_name, effect_library_types, effect_registry_revision, instantiate_effect_node,
    register_effect_definition, CustomEffectProcessorBinding, EffectCacheKeyBuilder,
    EffectCachePolicy, EffectCategoryNode, EffectColorDomain, EffectColorDomainContract,
    EffectDefinition, EffectEvalContext, EffectGraphBuildError, EffectGraphBuilder,
    EffectGraphPreparer, EffectInstantiationError, EffectNode, EffectNodeExt,
    EffectPreparationContext, EffectRenderOp, EffectRenderParamsBuilder, EffectRenderPlan,
    EffectResourceDependency, EffectResourceRecovery, EffectType, PreparedEffectEvaluator,
    PreparedLut3D,
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
pub use gamut_mapping::{GamutCompressionGrade, GamutMappingError, HighlightRecoveryGrade};
pub use gpu_plan::{
    get_or_lower_effect_graph_to_gpu_plan, lower_effect_graph_node_to_gpu_point_plan,
    lower_effect_graph_nodes_to_gpu_plan, lower_effect_graph_to_gpu_plan, CompiledEffectGpuPlan,
    EffectGpuPlanBlocker, EffectGpuPointOp, MAX_FUSED_GPU_EFFECT_OPS,
};
pub use graph::{
    compile_reference_effect_graph, compile_reference_effect_graph_in_domain,
    compile_reference_render_graph, identity_compiled_effect_graph, prepare_effect_graph_topology,
    CompiledEffectDomainPlan, CompiledEffectGraph, CompiledEffectStageBinding, EffectDomainBlocker,
    EffectDomainBlockerKind, EffectDomainTransition, EffectGraphBuilderState, EffectGraphNode,
    EffectGraphNodeId, EffectGraphNodeKind, EffectGraphValue, EffectRenderGraph,
    PreparedEffectGraphTopology,
};
pub use hdr_grading::{
    HdrGradingAuthoring, HdrGradingError, HdrGradingZone, HdrZoneControl, PreparedHdrGrading,
    HDR_GRADING_MAX_STOPS, HDR_GRADING_MIN_STOPS, HDR_GRADING_SAMPLE_COUNT,
    HDR_GRADING_SAMPLE_ROWS, HDR_GRADING_ZONE_COUNT,
};
pub use heterogeneous_execution::{
    plan_effect_graph_value_execution, CompiledEffectValueExecutionPlan, EffectCompletionToken,
    EffectGraphExecutionBudget, EffectGraphExecutionBudgetKind, EffectGraphExecutionPlanError,
    EffectGraphExecutionRequest, EffectGraphExecutionStep, EffectMaterializationId,
    EffectValueFormat, EffectValueMaterialization, EffectValueResidency,
    HeterogeneousCpuCompletionEvidence, HeterogeneousCpuExecutionStopReason,
    HeterogeneousCpuTransferEvidence, HeterogeneousEffectShapeIdentity,
    PreparedHeterogeneousCpuCompletion, PreparedHeterogeneousEffectWork,
    PreparedHeterogeneousEffectWorkError, PreparedHeterogeneousGpuDispatch,
    PreparedHeterogeneousGpuStep, PreparedHeterogeneousGpuSuffix,
};
pub use lut::{
    Lut3D, LutLibrary, LutLibraryEntry, LutPreparationCache, LutPreparationCacheConfig,
    LutPreparationCacheDiagnostics,
};
pub use mask::{
    BezierPoint, MaskComponent, MaskEvaluation, MaskId, MaskOp, MaskShape, MaskShapeInterpolation,
    MaskShapeKeyframe, MAX_MASK_PATH_POINTS,
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
    PreparedEffectStack, PreparedGradeGraph,
};
pub use primary_grade::{AscCdlGrade, PrimariesGrade, PrimaryGradeError, WhiteBalanceGrade};
pub use qualifier::{
    PreparedQualifier, QualifierAuthoring, QualifierError, QualifierMode,
    MAX_QUALIFIER_BLUR_RADIUS, MAX_QUALIFIER_DENOISE_RADIUS,
};
pub use temporal_execution::{
    collect_temporal_frame_demands, prepare_temporal_frame_execution, EffectExecutionContinuity,
    EffectFrameTileF32, EffectTemporalExecutionError, EffectTemporalExecutionOutput,
    EffectTemporalExecutionRequest, EffectTemporalFrameDemandBatch, EffectTemporalFrameProvider,
    EffectTemporalFrameProviderError, EffectTemporalFrameRequest, EffectTemporalSourceIdentity,
    PreparedEffectTemporalExecution, PreparedTemporalFrameSet, PreparedTemporalFrameSetError,
};
pub use tracking::{
    canonicalize_tracking_shape, track_frame_pair, transform_tracking_shape, TrackingError,
    TrackingFrame, TrackingObservation, TrackingQuality, TrackingRegion, TrackingTransform,
};
pub use transition::Transition;
