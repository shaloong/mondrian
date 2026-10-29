//! # mondrian-effects
//!
//! 高级效果系统：LUT 调色 / 滤镜 / 转场 / 蒙版

pub mod adjustment;
pub use mondrian_core::EffectPassBlendPolicy;
pub mod alpha_contract;
mod float_frame;
pub use alpha_contract::{
    ColorFrameAlphaFormats, EffectAlphaConsumer, EffectAlphaContract, EffectAlphaOutputContract,
};
pub use float_frame::{
    blend_float_frames_controlled, blend_float_frames_in_place, convert_alpha_sample,
    materialize_finite_coverage_controlled, materialize_straight_coverage,
    mix_float_frames_controlled, normal_over_output_alpha, over_float_sample,
    validate_float32_frame_storage, EffectAlphaConversionFailure, EffectFloatFrame,
    EffectFloatFrameError, EffectFloatFrameView,
};
pub mod black_white;
pub mod box_blur;
pub mod channel_mixer;
pub mod chroma_key;
pub mod color_curves;
pub mod color_emboss;
pub mod coverage;
pub mod directional_blur;
pub mod drop_shadow;
pub mod effect;
pub mod emboss;
pub mod find_edges;
mod project_properties;
pub mod threshold;
pub use project_properties::{rebind_effect_project_geometry, EffectProjectPropertyInitializer};
pub mod execution;
pub mod execution_contract;
pub mod execution_planning;
mod execution_session;
pub mod extension_contract;
pub mod extension_sdk;
pub mod gamut_mapping;
pub mod gpu_pipeline;
pub mod gpu_plan;
pub mod graph;
pub mod hdr_grading;
pub mod heterogeneous_execution;
pub mod hsl;
pub(crate) mod hsl_space;
pub mod hue_saturation;
pub mod invert;
pub mod lut;
pub mod mask;
pub mod mask_raster;
pub mod prepared;
pub mod primary_grade;
mod processor_feedback;
mod processor_question;
pub use processor_feedback::*;
mod processor_residency;
pub use processor_residency::{
    EffectFloatProcessorFactory, EffectFloatProcessorInputRole, EffectFloatProcessorInstance,
    EffectFloatProcessorPreparation, EffectFloatProcessorSizing, EffectFloatProcessorStorage,
    EffectImageScaleRequirement, EffectProcessorControl, EffectProcessorHostResourceRefusal,
    EffectProcessorHostResourceRefusalReceipt, EffectProcessorInputRequest,
    EffectProcessorInputService, EffectProcessorInvocationError, EffectProcessorResources,
    EffectProcessorRetention, EffectProcessorStopReason, EffectProcessorTransientMemoryRefusal,
    PreparedEffectProcessorInput,
};
pub mod bevel;
pub mod kaleidoscope;
pub mod mosaic;
pub mod motion_tile;
pub(crate) mod parallel;
pub mod posterize;
pub mod qualifier;
pub mod shift_channels;
pub(crate) mod straight_alpha;
pub mod temporal_execution;
#[cfg(test)]
mod throughput;
pub mod tracking;
pub mod unsharp_mask;

pub use adjustment::{
    blend_rgba_f32_pixel, blend_rgba_f32_pixel_seeded, blend_rgba_pixel, blend_rgba_pixel_seeded,
};
pub use black_white::{
    BlackAndWhiteError, BlackAndWhiteGrade, BLACK_WHITE_HUE_STOPS, BLACK_WHITE_NEUTRAL_WEIGHT,
    BLACK_WHITE_STOP_KEYS, BLACK_WHITE_WEIGHT_MAX, BLACK_WHITE_WEIGHT_MIN,
};
pub use box_blur::{box_blur_frame_f32, BoxBlurDirection, BOX_BLUR_AUTHOR_MAX_RADIUS_PIXELS};
pub use chroma_key::{ChromaKeyError, PreparedChromaKey};
pub use color_curves::{
    ColorCurvesAuthoring, ColorCurvesMode, PreparedColorCurves, COLOR_CURVE_SAMPLE_COUNT,
    COLOR_CURVE_SAMPLE_ROWS,
};
pub use color_emboss::{
    ColorEmbossError, ColorEmbossGrade, COLOR_EMBOSS_HALO_PIXELS,
    COLOR_EMBOSS_MAX_CONTRAST_PERCENT, COLOR_EMBOSS_MAX_RELIEF,
};

pub use bevel::{bevel_frame_f32, BevelError, BevelGrade, BEVEL_HALO_PIXELS, BEVEL_MAX_THICKNESS};
pub use coverage::{has_positive_coverage, mix_straight_rgba, straight_rgba_from_premultiplied};
pub use effect::{
    build_effect_render_graph, compile_clip_effect_graph, effect_category_tree, effect_definition,
    effect_display_name, effect_library_types, effect_registry_revision, instantiate_effect_node,
    instantiate_effect_node_with_project_geometry, register_effect_definition,
    CustomEffectProcessorBinding, EffectCacheKeyBuilder, EffectCachePolicy, EffectCategoryNode,
    EffectColorDomain, EffectColorDomainContract, EffectDefinition, EffectDefinitionError,
    EffectEvalContext, EffectFrameContext, EffectFrameContextError, EffectGraphBuildError,
    EffectGraphBuilder, EffectGraphPreparer, EffectInstantiationError, EffectNode, EffectNodeExt,
    EffectPreparationContext, EffectProjectGeometry, EffectProjectGeometryError, EffectRenderOp,
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
    compiled_effect_graph_supports_rgba_f32_with_domain_processor,
    CustomEffectFloatRenderProcessor, CustomEffectRenderProcessor, EffectDomainProcessorCacheKey,
    EffectExecutionError, EffectFloatExecutionError, EffectFloatUnsupportedReason,
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
pub use extension_contract::{
    effect_extension_runtime_status, EffectExtensionApiVersion, EffectExtensionContract,
    EffectExtensionLibraryPolicy, EffectExtensionRuntimeFailurePolicy,
    EffectExtensionRuntimeStatus, CURRENT_EFFECT_EXTENSION_API_VERSION,
};
pub use extension_sdk::{EffectExtensionDefinitionBuilder, EffectGraphDsl};
pub use gamut_mapping::{GamutCompressionGrade, GamutMappingError, HighlightRecoveryGrade};
pub use gpu_pipeline::{
    lower_effect_graph_to_gpu_pipeline, spatial_pass_for, CompiledEffectGpuPipeline,
    EffectGpuSpatialPass, EffectGpuStage,
};
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
    plan_effect_graph_value_execution, CompiledEffectValueExecutionPlan,
    EffectAlphaMaterializationAdmission, EffectCompletionToken, EffectGraphExecutionBudget,
    EffectGraphExecutionBudgetKind, EffectGraphExecutionPlanError, EffectGraphExecutionRequest,
    EffectGraphExecutionStep, EffectMaterializationId, EffectPlannedAlpha,
    EffectPlannedValueFormat, EffectValueFormat, EffectValueMaterialization, EffectValueResidency,
    HeterogeneousCpuCompletionEvidence, HeterogeneousCpuExecutionStopReason,
    HeterogeneousCpuTransferEvidence, HeterogeneousEffectShapeIdentity,
    PreparedHeterogeneousCpuCompletion, PreparedHeterogeneousEffectWork,
    PreparedHeterogeneousEffectWorkError, PreparedHeterogeneousGpuDispatch,
    PreparedHeterogeneousGpuStep, PreparedHeterogeneousGpuSuffix,
};
pub use hsl::{HslGrade, HslGradeError};
pub use hue_saturation::{HueBand, HueSaturationError, HueSaturationGrade};
pub use invert::{invert_rgba_f32, InvertChannels};
pub use kaleidoscope::{
    kaleidoscope_frame_f32, KaleidoscopeError, KaleidoscopeGrade, KALEIDOSCOPE_MAX_MIRRORING,
    KALEIDOSCOPE_MAX_SIZE, KALEIDOSCOPE_MIN_MIRRORING, KALEIDOSCOPE_MIN_SIZE,
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
pub use mosaic::{
    apply_mosaic_f32_controlled, mosaic_frame_f32, MosaicError, MosaicGrade,
    MOSAIC_MAX_BLOCK_PIXELS, MOSAIC_MIN_BLOCK_PIXELS,
};
pub use motion_tile::{
    motion_tile_frame_f32, MotionTileError, MotionTileGrade, MOTION_TILE_MAX_PERCENT,
};
pub use posterize::{
    posterize_frame_f32, PosterizeError, PosterizeGrade, POSTERIZE_MAX_LEVELS, POSTERIZE_MIN_LEVELS,
};
pub use prepared::{
    EffectDependencyCheckError, EffectProgramDependencyIdentity, PreparedEffectInput,
    PreparedEffectProgram, PreparedEffectStack, PreparedGradeGraph,
};
pub use primary_grade::{AscCdlGrade, PrimariesGrade, PrimaryGradeError, WhiteBalanceGrade};
pub use qualifier::{
    PreparedQualifier, QualifierAuthoring, QualifierError, QualifierMode,
    MAX_QUALIFIER_BLUR_RADIUS, MAX_QUALIFIER_DENOISE_RADIUS,
};
pub use temporal_execution::{
    collect_temporal_frame_demands, prepare_temporal_frame_execution,
    prepare_temporal_frame_program, EffectExecutionContinuity, EffectFrameTileF32,
    EffectTemporalExecutionError, EffectTemporalExecutionOutput, EffectTemporalExecutionRequest,
    EffectTemporalFrameDemandBatch, EffectTemporalFrameProvider, EffectTemporalFrameProviderError,
    EffectTemporalFrameRequest, EffectTemporalSourceIdentity, PreparedEffectTemporalExecution,
    PreparedEffectTemporalProgram, PreparedTemporalFrameSet, PreparedTemporalFrameSetError,
};
pub use threshold::{ThresholdError, ThresholdGrade, THRESHOLD_MAX_LEVEL, THRESHOLD_MIN_LEVEL};
pub use tracking::{
    canonicalize_tracking_shape, track_frame_pair, transform_tracking_shape, TrackingError,
    TrackingFrame, TrackingObservation, TrackingQuality, TrackingRegion, TrackingTransform,
};
pub use unsharp_mask::{
    apply_unsharp_mask_f32, sharpened_channel, unsharp_mask_gain,
    UNSHARP_MASK_AUTHOR_MAX_AMOUNT_PERCENT, UNSHARP_MASK_AUTHOR_MAX_RADIUS_PIXELS,
    UNSHARP_MASK_AUTHOR_MIN_RADIUS_PIXELS,
};

pub use processor_question::{
    EffectProcessorGuiActionFeedback, EffectProcessorGuiActionReceipt, EffectProcessorGuiAdmission,
    EffectProcessorGuiPresence, EffectProcessorGuiState, EffectProcessorLiveQuestion,
    EffectProcessorQuestionId, EffectProcessorQuestionReceipt, EffectProcessorQuestionReply,
    EffectProcessorQuestionService, EffectProcessorQuestionState, EffectProcessorQuestionView,
};

#[cfg(test)]
mod image_scale_admission_tests;

pub use channel_mixer::{
    ChannelMixerError, ChannelMixerGrade, CHANNEL_MIXER_WEIGHT_COUNT, CHANNEL_MIXER_WEIGHT_MAX,
    CHANNEL_MIXER_WEIGHT_MIN,
};
pub use directional_blur::{
    directional_blur_step, directional_blur_taps, DIRECTIONAL_BLUR_AUTHOR_MAX_LENGTH_PIXELS,
    MAX_DIRECTIONAL_BLUR_TAPS,
};
pub use drop_shadow::{
    DropShadowError, DropShadowGrade, DROP_SHADOW_AUTHOR_MAX_DISTANCE_PIXELS,
    DROP_SHADOW_AUTHOR_MAX_OPACITY, DROP_SHADOW_AUTHOR_MAX_SOFTNESS_PIXELS,
};
pub use emboss::{
    EmbossError, EmbossGrade, EMBOSS_HALO_PIXELS, EMBOSS_MAX_CONTRAST_PERCENT, EMBOSS_MAX_RELIEF,
};
pub use find_edges::{
    FindEdgesError, FindEdgesGrade, FIND_EDGES_HALO_PIXELS, FIND_EDGES_MAX_HALO_PIXELS,
};
pub use shift_channels::{ChannelSource, ShiftChannelsGrade};
