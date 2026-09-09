//! # mondrian-renderer
//!
//! GPU 渲染引擎（基于 wgpu）。
//!
//! 提供：
//! - `GpuContext`：wgpu Device/Queue/Adapter 管理
//! - `PreparedVisualProgram`：不可变视觉作者状态编译
//! - `GpuFrameCompositor`：类型化工作域合成
//! - `ViewerGpuExecutionRuntime`：有界 Viewer GPU 执行

pub mod color;
pub mod color_accuracy;
pub mod color_frame;
pub mod color_reference;
pub mod color_report_vocab;
mod color_stage;
pub mod color_transform;
pub mod context;
mod cpu_quantization;
mod cpu_visual_execution;
mod cpu_yuv;
mod creative_lut_gpu;
pub mod cross_application_qualification;
pub mod display_calibration;
pub mod gpu_composite_execution;
pub mod gpu_compositor;
pub mod gpu_output_working_set;
pub mod gpu_qualification;
pub mod gpu_visual_frame;
mod heterogeneous_cpu;
pub mod heterogeneous_gpu;
pub mod native_video;
pub mod ocio_gpu;
pub mod picture_sampling;
pub mod prepared_visual_execution;
pub mod prepared_visual_frame_closure;
pub mod prepared_visual_program;
pub mod prepared_visual_range_closure;
pub mod profile;
pub mod program_scopes_gpu;
#[doc(hidden)]
pub mod qualification_attestation;
pub mod realtime_performance;
pub mod reference_output;
mod resident_encode;
mod resolved_visual_identity;
pub mod shot_match;
pub mod signal_monitor;
pub mod source_frame_preparation;
pub mod timeline_composite;
mod timeline_effect_routes;
pub mod timeline_render_plan;
pub mod timeline_temporal;
pub mod viewer_execution;
mod viewer_retirement;
pub mod viewer_runtime;
pub mod viewer_working_set;
#[cfg(feature = "validation")]
mod visual_execution_validation;
pub mod working_float_policy;
pub use heterogeneous_cpu::{
    HeterogeneousCpuPrefixBatchCompletion, HeterogeneousCpuPrefixBatchError,
    HeterogeneousCpuPrefixBatchExecutor, HeterogeneousCpuPrefixBatchGrant,
    HeterogeneousCpuPrefixBatchItem, HeterogeneousCpuPrefixBatchOutput,
    HeterogeneousCpuPrefixBatchRequest, HeterogeneousCpuPrefixFrameContractViolation,
    HeterogeneousCpuPrefixSource, PreparedHeterogeneousEffectRoute,
};
pub mod viewer_spatial;

pub use color_accuracy::{
    compare_code_values, compare_linear_rgba, compare_pq_hdr_display_rgba,
    compare_srgb_display_rgba8, CodeValueAccuracyBudget, CodeValueAccuracyError,
    CodeValueAccuracyReport, CodeValueAccuracyStatistics, LinearAccuracyBudget,
    LinearAccuracyChannelGroup, LinearAccuracyError, LinearAccuracyGroupReport,
    LinearAccuracyStatistics, LinearRgbaAccuracyBudget, LinearRgbaAccuracyReport,
    PqHdrDisplayAccuracyBudget, PqHdrDisplayAccuracyError, PqHdrDisplayAccuracyReport,
    PqHdrDisplayAccuracyStatistics, SrgbDisplayAccuracyBudget, SrgbDisplayAccuracyError,
    SrgbDisplayAccuracyReport, SrgbDisplayAccuracyStatistics,
};
pub use color_frame::{
    execute_native_decoded_frame_import, ColorFrameAlpha, ColorFrameDescriptor, ColorFrameDomain,
    ColorFrameEncoding, ColorFrameResidency, ColorFrameSpace, CpuColorFrame, CpuEncodedColorFrame,
    CpuEncodedFloatColorFrame, CpuSourceColorFrame, EncodedRgbaF32Frame,
    GpuColorFrameAllocationPlan, GpuColorFrameBindGroupCacheKeyAllocationError,
    GpuColorFrameContract, GpuColorFrameHandle, GpuColorFrameHandleError, GpuColorFrameId,
    GpuColorFrameIdAllocationError, GpuColorFrameIdAllocator, GpuColorFrameReadback,
    GpuColorFrameReadbackError, GpuColorFrameReadbackPlan, GpuColorFrameResource,
    GpuColorFrameResourceTable, GpuColorFrameResourceTableError, GpuColorFrameTextureFormat,
    GpuColorFrameUploadError, GpuColorFrameUploadPlan, GpuColorFrameUploader,
    GpuColorFrameWgpuResource, GpuColorFrameWgpuResourcePool,
    GpuColorFrameWgpuResourcePoolDiagnostics, GpuColorFrameWgpuResourcePoolOptions,
    GpuNativeDecodedFrameImportBackend, GpuNativeDecodedFrameImportContract,
    GpuNativeDecodedFrameImportError, GpuNativeDecodedFrameImportExecution,
    GpuNativeDecodedFrameImportMode, GpuNativeDecodedFrameImportPlan,
    GpuNativeDecodedFrameImportPlanError, GpuNativeDecodedFrameImportRoute,
    GpuNativeDecodedFrameImportSource, GpuNativeDecodedFrameImportSupport,
    GpuNativeDecodedFrameImportSupportError, GpuNativeDecodedFrameSourceDescriptor,
    GpuNativeDecodedFrameSourceFormatError, GpuNativeDecodedFrameTextureFormat,
    GpuNativeDecodedFrameVideoSampling, GpuNativeRgbDecodePlan, GpuNativeRgbDecodePlanError,
    GpuResidentEncoderInputLease, GpuVideoChromaLocation, GpuVideoRange, LinearFloatSource,
    SourceAlphaInterpretationError, ViewerGpuPresentationOutputLease,
};
pub use color_reference::{
    import_external_color_reference, import_external_color_reference_with_limits,
    ColorReferenceDecoder, ColorReferenceDescriptor, ColorReferenceEncoding, ColorReferenceFrame,
    ColorReferenceImportLimits, ColorReferenceOrigin, ColorReferencePayloadFormat,
    ColorReferencePixels, ColorReferenceValidationError,
};
#[allow(unused_imports)]
pub(crate) use color_stage::{
    execute_cpu_input_stage, execute_cpu_input_stage_float,
    execute_cpu_input_stage_float_with_session, execute_cpu_input_stage_with_session,
    execute_cpu_output_boundary, execute_cpu_output_boundary_float,
    execute_cpu_output_boundary_float_with_session, execute_cpu_output_boundary_rgba8,
    execute_cpu_output_boundary_rgba8_with_session, execute_cpu_output_boundary_with_session,
    execute_cpu_output_stage, execute_cpu_output_stage_with_session,
    execute_cpu_program_monitor_boundary_rgba8,
    execute_cpu_program_monitor_boundary_rgba8_with_session,
    execute_cpu_program_monitor_presentation_rgba8,
    execute_cpu_program_monitor_presentation_rgba8_with_session,
    execute_cpu_program_monitor_presentation_rgba8_with_signal_monitoring_with_session,
    execute_cpu_source_input_stage, execute_cpu_source_input_stage_with_session,
    execute_cpu_working_transform, execute_cpu_working_transform_with_session,
    CpuRenderColorStageExecutor, CpuSignalMonitoringError, RenderColorStage,
    RenderColorStageDiagnostics, RenderColorStageExecution, RenderColorStageGpuBlockerBreakdown,
    RenderColorStageMode, RenderColorStagePlan, RenderColorStagePlanner,
    RenderGpuColorPassExecutionError, RenderGpuColorPassResolvedResources,
    RenderGpuColorPassSchedule, RenderGpuColorPassScheduleError, RenderGpuColorTransformRecord,
    RenderGpuColorTransformResourcePlan, RenderGpuColorTransformResourcePlanError,
    RenderGpuColorTransformRuntimeRecordError, RenderGpuCompositeGraphRecord,
    RenderGpuCompositeGraphRecordError, RenderGpuEffectDomainRecord,
    RenderGpuEffectDomainRecordError, RenderGpuInputStageRecord, RenderGpuInputStageResourcePlan,
    RenderGpuInputStageResourcePlanError, RenderGpuInputStageRuntimeRecordError,
    RenderGpuOutputBoundaryBackendContext, RenderGpuOutputBoundaryRecordError,
    RenderGpuOutputBoundaryRecordRequest, RenderGpuOutputBoundaryRuntime,
    RenderGpuOutputBoundaryRuntimeDiagnostics, RenderGpuOutputBoundaryRuntimeOwnedBackendContext,
    RenderGpuOutputBoundaryRuntimeRecordError, RenderGpuOutputDiagnosticArea,
    RenderGpuOutputFrameReport, RenderGpuOutputHealthAction, RenderGpuOutputHealthCheck,
    RenderGpuOutputHealthEvidence, RenderGpuOutputHealthReport, RenderGpuOutputHealthRootCause,
    RenderGpuOutputHealthSeverity, RenderGpuOutputHealthStatus, RenderGpuOutputHealthSummary,
    RenderGpuOutputHealthVerdict, RenderGpuOutputRuntimeDiagnosticsReport,
    RenderGpuOutputStageBackendContext, RenderGpuOutputStageDiagnosticsReport,
    RenderGpuOutputStageMaterializeError, RenderGpuOutputStageMaterializedResources,
    RenderGpuOutputStageReadbackError, RenderGpuOutputStageRecord, RenderGpuOutputStageRecordError,
    RenderGpuOutputStageRecordRequest, RenderGpuOutputStageResourcePlan,
    RenderGpuOutputStageResourcePlanError, RenderGpuWorkingFrameUploadError,
    RenderGpuWorkingFrameUploadRecord, RenderOutputColorBoundary,
    RenderOutputColorBoundaryExecutor, RenderOutputColorBoundaryFloat,
    RenderOutputColorBoundaryGpuRecordError, RenderOutputColorBoundaryPlanner,
    RenderOutputColorBoundaryRgba8, RenderOutputColorBoundaryStagePlan,
    RenderOutputColorBoundaryTarget, RenderProgramMonitorBoundaryRgba8,
    RenderProgramMonitorPresentationRgba8, RENDER_GPU_OUTPUT_HEALTH_REPORT_SCHEMA_VERSION,
};
pub use color_transform::{
    CpuColorTransformExecutor, RenderColorTransform, RenderColorTransformBackend,
    RenderColorTransformDiagnostics, RenderColorTransformDirection, RenderColorTransformError,
    RenderColorTransformExecutionFailure, RenderColorTransformGpuOptions,
    RenderColorTransformGpuPlan, RenderColorTransformGpuPlanner, RenderCpuColorExecutionSession,
    RenderEffectColorDomainGpuPlan, RenderEffectColorDomainGpuPlanError,
    RenderEffectColorDomainGpuPlanner, RenderInputTransform, RenderInputTransformResult,
    RenderIntermediateColorTransform, RenderMonitorAdaptation, RenderMonitorAdaptationError,
    RenderOcioDisplayView, RenderOutputTransformFloatResult, RenderOutputTransformResult,
};
pub use context::GpuContext;
pub use context::{
    native_video_texture_device_features, ocio_lut_filtering_device_features,
    request_adapter_with_native_video_preference,
};
pub use creative_lut_gpu::{
    GpuCreativeLutCacheConfig, GpuCreativeLutCacheDiagnostics, GpuCreativeLutError,
};
pub use cross_application_qualification::{
    CrossApplicationAccuracyBudget, CrossApplicationArtifactEvidence, CrossApplicationCaseReport,
    CrossApplicationComparisonStatistics, CrossApplicationFrameCoordinate,
    CrossApplicationMissingArtifact, CrossApplicationPairComparison,
    CrossApplicationPixelOrientation, CrossApplicationProducer,
    CrossApplicationProducerRequirement, CrossApplicationProducerScope,
    CrossApplicationQualificationArtifact, CrossApplicationQualificationCase,
    CrossApplicationQualificationError, CrossApplicationQualificationLimits,
    CrossApplicationQualificationProfile, CrossApplicationQualificationReport,
    CrossApplicationQualificationRun, CrossApplicationQualificationStatus,
    PreparedCrossApplicationQualification,
};
pub use display_calibration::{
    GpuDisplayCalibrationLut, GpuDisplayCalibrationPipeline, GpuDisplayCalibrationPipelineError,
    GpuDisplayCalibrationPlan, GpuDisplayCalibrationPlanError, GpuDisplayCalibrationPrepareError,
    GpuDisplayCalibrationPreparedPass, GpuDisplayCalibrationRecordError,
    GpuDisplayCalibrationRuntime, GpuDisplayCalibrationRuntimeDiagnostics,
    GpuDisplayCalibrationRuntimeError,
};
pub use gpu_composite_execution::{
    GpuCompositeExecutionDiagnostics, GpuCompositeExecutionPlan, GpuCompositeExecutionPlanError,
    GpuCompositeExecutionPlanner, GpuCompositeExecutionPolicy, GpuCompositeLayerExecution,
    GpuCompositeLayerFootprint, GpuCompositeRect, GpuCompositeSourceCrop,
    DEFAULT_GPU_COMPOSITE_TILE_DIMENSION, MAX_GPU_COMPOSITE_TILES,
};
pub use gpu_compositor::{
    evaluate_gpu_compositing_capability, GpuCompositeError, GpuCompositeLayer,
    GpuCompositeLayerSource, GpuCompositeRecord, GpuCompositeRequest, GpuCompositingBlockerReason,
    GpuCompositingCapability, GpuCompositingDiagnostics, GpuCompositorTextureBindingDiagnostics,
    GpuCompositorUniformArenaDiagnostics, GpuFrameCompositor, GpuPointEffectRecord,
    GpuSolidSourceRecord,
};
pub use gpu_output_working_set::{
    estimate_render_gpu_output_active_working_set, RenderGpuOutputActiveResourceDemand,
    RenderGpuOutputActiveWorkingSetAdmissionError, RenderGpuOutputActiveWorkingSetEstimate,
    RenderGpuOutputActiveWorkingSetEstimateError, RenderGpuOutputActiveWorkingSetStage,
    RenderGpuOutputExecutionResourceGrant,
};
pub use gpu_qualification::{
    GpuColorQualificationError, GpuColorQualificationExecutionPolicy,
    GpuColorQualificationPolicyError, GPU_COLOR_QUALIFICATION_POLICY_ENV,
    SEALED_GPU_COLOR_QUALIFICATION_POLICY,
};
pub use gpu_visual_frame::{
    estimate_gpu_visual_frame_active_working_set, GpuVisualFrameActiveTextureDemand,
    GpuVisualFrameActiveWorkingSetAdmissionError, GpuVisualFrameActiveWorkingSetEstimate,
    GpuVisualFrameActiveWorkingSetEstimateError, GpuVisualFrameActiveWorkingSetStage,
    GpuVisualFrameElement, GpuVisualFrameExecutionError, GpuVisualFrameExecutionResourceGrant,
    GpuVisualFrameExecutor, GpuVisualFrameRecord, GpuVisualFrameRequest, GpuVisualFrameSource,
    GpuVisualSourceLayer, GpuVisualTransitionInput,
};
pub use heterogeneous_gpu::{
    record_heterogeneous_gpu_continuation, HeterogeneousGpuBatchId,
    HeterogeneousGpuCompletedContinuation, HeterogeneousGpuCompletedEvidence,
    HeterogeneousGpuCompletedFrame, HeterogeneousGpuContinuationBinding,
    HeterogeneousGpuContinuationError, HeterogeneousGpuContinuationRequest,
    HeterogeneousGpuContinuationRuntime, HeterogeneousGpuExecutionCapability,
    HeterogeneousGpuRecordResources, HeterogeneousGpuRecordedContinuation,
    HeterogeneousGpuRecordedEvidence, HeterogeneousGpuRecordingRequirements,
    HeterogeneousGpuResourceGrant, HeterogeneousGpuResourceKind,
    HeterogeneousGpuSubmissionAuthority, HeterogeneousGpuSubmittedContinuation,
    HeterogeneousGpuSubmittedEvidence,
};
pub use mondrian_core::{ResolvedVisualFrameIdentity, ResolvedVisualNodeMaterializationIdentity};
#[cfg(target_os = "windows")]
pub use native_video::{
    inspect_d3d12_native_decoded_frame, D3D12NativeDecodedFrameInspection,
    D3D12NativeDecodedFrameInspectionError, D3D12NativeVideoImportBackend,
    D3D12NativeVideoImportBackendCreateError, D3D12NativeVideoImportBackendOptions,
    NativeVideoAdapterError, NativeVideoAdapterLuid,
};
pub use native_video::{
    GpuNativeRgbDecodeRecordError, GpuNativeRgbDecoder, GpuNativeRgbPrepareError,
    GpuNativeRgbPreparedPass, GpuNativeVideoExtent, GpuNativeYuvDecodePlan,
    GpuNativeYuvDecodePlanError, GpuNativeYuvDecodeRecordError, GpuNativeYuvDecoder,
    GpuNativeYuvPlaneViews, GpuNativeYuvPreparedPass, GpuYuvChromaPlaneLayout,
    GpuYuvChromaSubsampling, GpuYuvCodeAlignment, NativeVideoImportCandidateTimingReceipt,
    NativeVideoImportCandidateToken, NativeVideoImportCpuTimings,
    NativeVideoImportGpuTimingDiagnostics, NativeVideoImportGpuTimingPolicy,
    NativeVideoImportGpuTimingSample, NativeVideoImportToken,
    GPU_NATIVE_IMPORT_MAX_STORAGE_PIXEL_RATIO, NATIVE_VIDEO_IMPORT_GPU_TIMING_MAX_CAPACITY,
    NATIVE_VIDEO_IMPORT_GPU_TIMING_SCHEMA_VERSION,
};
#[cfg(target_os = "macos")]
pub use native_video::{MetalNativeVideoImportBackend, MetalNativeVideoImportBackendCreateError};
#[cfg(target_os = "linux")]
pub use native_video::{VulkanNativeVideoImportBackend, VulkanNativeVideoImportBackendCreateError};
pub use ocio_gpu::{
    OcioGpuBindingContract, OcioGpuBindingContractValidationError,
    OcioGpuFullscreenWrapperContract, OcioGpuGeneratedProgramContract,
    OcioGpuGeneratedProgramDiagnostic, OcioGpuGeneratedProgramSourceKind,
    OcioGpuNagaShaderStageArtifact, OcioGpuShaderCache, OcioGpuShaderCacheDiagnostics,
    OcioGpuShaderDiagnostic, OcioGpuShaderError, OcioGpuShaderPlan, OcioGpuShaderRequest,
    OcioGpuShaderStage, OcioGpuShaderTargetLanguage, OcioGpuShaderTranslationCache,
    OcioGpuShaderTranslationCacheDiagnostics, OcioGpuShaderTranslationError,
    OcioGpuShaderTranslationFailure, OcioGpuShaderTranslationRequest, OcioGpuShaderTranslator,
    OcioGpuTexture2DBindingContract, OcioGpuTexture3DBindingContract, OcioGpuTranslatedShader,
    OcioGpuUniformBindingContract, OcioGpuWgpuBackendObjectError, OcioGpuWgpuBackendObjectRuntime,
    OcioGpuWgpuBackendObjectRuntimeDiagnostics, OcioGpuWgpuBackendPrepError,
    OcioGpuWgpuBackendPrepRuntime, OcioGpuWgpuBackendPrepRuntimeDiagnostics,
    OcioGpuWgpuBindGroupError, OcioGpuWgpuBindGroupLayoutDescriptorPlan,
    OcioGpuWgpuBindGroupLayoutEntryPlan, OcioGpuWgpuBindGroupPreparer, OcioGpuWgpuBindResource,
    OcioGpuWgpuBindResourceEntry, OcioGpuWgpuBindResourcePlan, OcioGpuWgpuBindResourcePlanError,
    OcioGpuWgpuBindingLayoutPlan, OcioGpuWgpuBindingLayoutPlanError, OcioGpuWgpuBindingPlan,
    OcioGpuWgpuBindingResource, OcioGpuWgpuBlocker, OcioGpuWgpuColorTargetFormat,
    OcioGpuWgpuExecutionPlan, OcioGpuWgpuFullscreenShaderContract, OcioGpuWgpuFullscreenTopology,
    OcioGpuWgpuLayoutBindingResource, OcioGpuWgpuLutTextureDimension, OcioGpuWgpuLutTextureExtent,
    OcioGpuWgpuLutTextureFormat, OcioGpuWgpuLutUploadError, OcioGpuWgpuLutUploadPlan,
    OcioGpuWgpuLutUploadResource, OcioGpuWgpuLutUploader, OcioGpuWgpuOcioBindGroup,
    OcioGpuWgpuPackedLutTexture, OcioGpuWgpuPackedLutUploadPlan, OcioGpuWgpuPackedUniformBuffer,
    OcioGpuWgpuPipelineBindGroupResource, OcioGpuWgpuPipelineBindGroupSlot,
    OcioGpuWgpuPipelineLayout, OcioGpuWgpuPipelineLayoutError, OcioGpuWgpuPipelineLayoutPlan,
    OcioGpuWgpuPipelineLayoutPreparer, OcioGpuWgpuPreparedBackendObjects,
    OcioGpuWgpuPreparedResources, OcioGpuWgpuPreparedStaticPipeline,
    OcioGpuWgpuPreparedWrapperInputLayout, OcioGpuWgpuRenderPassError,
    OcioGpuWgpuRenderPassNodePlan, OcioGpuWgpuRenderPipeline, OcioGpuWgpuRenderPipelineCache,
    OcioGpuWgpuRenderPipelineCacheDiagnostics, OcioGpuWgpuRenderPipelineDescriptorPlan,
    OcioGpuWgpuRenderPipelineError, OcioGpuWgpuResourceCache, OcioGpuWgpuResourceCacheDiagnostics,
    OcioGpuWgpuResourcePlan, OcioGpuWgpuSamplerBinding, OcioGpuWgpuSamplerBindingPolicy,
    OcioGpuWgpuSamplerFiltering, OcioGpuWgpuShaderModule, OcioGpuWgpuShaderModuleCache,
    OcioGpuWgpuShaderModuleCacheDiagnostics, OcioGpuWgpuShaderModuleError,
    OcioGpuWgpuShaderVisibility, OcioGpuWgpuStaticPipelineCacheDiagnostics,
    OcioGpuWgpuTexture2DUpload, OcioGpuWgpuTexture3DUpload, OcioGpuWgpuTextureContractMismatch,
    OcioGpuWgpuTextureSampleType, OcioGpuWgpuUniformUpload, OcioGpuWgpuUniformUploadError,
    OcioGpuWgpuUniformUploadPlan, OcioGpuWgpuUniformUploadResource, OcioGpuWgpuUniformUploader,
    OcioGpuWgpuUploadedLutTexture, OcioGpuWgpuUploadedLuts, OcioGpuWgpuUploadedTextureMismatch,
    OcioGpuWgpuUploadedUniformBuffer, OcioGpuWgpuUploadedUniformMismatch,
    OcioGpuWgpuWrapperBindingEntry, OcioGpuWgpuWrapperBindingPlan,
    OcioGpuWgpuWrapperBindingResource, OcioGpuWgpuWrapperInputBindingCacheDiagnostics,
    OcioGpuWgpuWrapperLinkBlocker, OcioGpuWgpuWrapperLinkPlan,
    OcioGpuWgpuWrapperShaderArtifactError, OcioGpuWgpuWrapperShaderModuleArtifact,
    OcioGpuWgpuWrapperShaderModuleArtifactCache,
    OcioGpuWgpuWrapperShaderModuleArtifactCacheDiagnostics,
    OcioGpuWgpuWrapperShaderModuleArtifactError, OcioGpuWgpuWrapperShaderModuleCache,
    OcioGpuWgpuWrapperShaderModuleCacheDiagnostics, OcioGpuWgpuWrapperShaderModules,
    OcioGpuWgpuWrapperShaderSourceArtifact,
};
pub use prepared_visual_execution::{
    execute_prepared_visual_closure, PreparedVisualExecutionAdapter, PreparedVisualExecutionError,
    PreparedVisualExecutionNodeInputs, PreparedVisualExecutionStructureError,
};
pub use prepared_visual_frame_closure::{
    prepare_bound_visual_frame_closure, prepare_visual_frame_closure,
    PreparedVisualChildCanvasPolicy, PreparedVisualFrameClosure, PreparedVisualFrameClosureError,
    PreparedVisualFrameClosureRequest, PreparedVisualFrameEvaluation, PreparedVisualFrameNode,
    PreparedVisualFrameNodeId, PreparedVisualNestedBinding, PreparedVisualNestedInstanceStep,
    PreparedVisualNestedSample,
};
pub use prepared_visual_program::{
    prepared_visual_author_fingerprint, PreparedVisualAuthorFingerprintError,
    PreparedVisualAuthorSnapshotIdentity, PreparedVisualEffectBlocker,
    PreparedVisualFrameReachability, PreparedVisualMaterializationContract,
    PreparedVisualNestedDemand, PreparedVisualNestedRange, PreparedVisualNestedRangeDemand,
    PreparedVisualProgram, PreparedVisualProgramBindError, PreparedVisualProgramBinding,
    PreparedVisualProgramBindingError, PreparedVisualProgramCache,
    PreparedVisualProgramCacheConfig, PreparedVisualProgramCacheDiagnostics,
    PreparedVisualProgramDependencyError, PreparedVisualProgramDiagnostics,
    PreparedVisualProgramError, PreparedVisualRangeReachability, PreparedVisualTransitionBlocker,
    DEFAULT_PREPARED_VISUAL_PROGRAM_CACHE_BYTES, DEFAULT_PREPARED_VISUAL_PROGRAM_CACHE_CAPACITY,
};
pub use prepared_visual_range_closure::{
    next_bound_prepared_visual_media_demand_frame, next_bound_prepared_visual_new_asset_frame,
    next_prepared_visual_media_demand_frame, prepare_bound_visual_range_closure,
    prepare_visual_range_closure, PreparedVisualRangeClosure, PreparedVisualRangeClosureError,
};
pub use program_scopes_gpu::{
    GpuProgramScopesBufferLayout, GpuProgramScopesError, GpuProgramScopesRecord,
    GpuProgramScopesRequest, GpuProgramScopesRuntime, GpuProgramScopesRuntimeDiagnostics,
};
pub use realtime_performance::{
    evaluate_realtime_visual_performance, RealtimePerformanceExecutionPolicy,
    RealtimePerformancePolicyError, RealtimeVisualAdapterIdentity, RealtimeVisualCheckRelation,
    RealtimeVisualFrameEvidence, RealtimeVisualPerformanceCheck,
    RealtimeVisualPerformanceObservation, RealtimeVisualPerformanceReport,
    RealtimeVisualPerformanceVerdict, RealtimeVisualQuantiles, RealtimeVisualScenarioId,
    RealtimeVisualStageQuantiles, RealtimeVisualWarmPathEvidence, RealtimeVisualWorkload,
    REALTIME_PERFORMANCE_EXECUTION_POLICY_ENV, REALTIME_VISUAL_PERFORMANCE_PROFILE,
    REALTIME_VISUAL_PERFORMANCE_REPORT_SCHEMA_VERSION,
    SEALED_REALTIME_PERFORMANCE_EXECUTION_POLICY,
};
pub use reference_output::{ReferenceOutputProgram, ReferenceOutputProgramError};
pub use resident_encode::{
    D3D12ResidentEncodeAdapter, D3D12ResidentEncodeAdapterContract,
    D3D12ResidentEncodeAdapterCreateError, D3D12ResidentEncodeAdapterDiagnostics,
    D3D12ResidentEncodeSubmissionError,
};
pub use resolved_visual_identity::{resolved_visual_frame_identity, ResolvedVisualIdentityError};
pub use shot_match::{
    analyze_shot_match_frame, solve_shot_match, ShotMatchAnalysisError, ShotMatchSolution,
};
pub use signal_monitor::{
    GpuSignalMonitorError, GpuSignalMonitorRequest, GpuSignalMonitorRuntime,
    GpuSignalMonitorRuntimeDiagnostics,
};
pub use source_frame_preparation::{
    prepare_decoded_cpu_source_frame, DecodedCpuSourceFrame, PreparedSourceFrame,
    PreparedSourceFrameExecution, SourceFramePreparationError, SourceFramePreparationIntent,
};
pub use timeline_composite::{
    admit_timeline_render_plan_for_cpu_compositor, composite_path_diagnostics,
    composite_timeline_elements, composite_timeline_elements_color_frame,
    composite_timeline_elements_color_frame_with_diagnostics, composite_timeline_elements_into,
    estimate_timeline_cpu_working_set, is_identity_transform, quantize_transform_signature,
    TimelineAdjustmentLayer, TimelineCompositeBackground, TimelineCompositeColorPath,
    TimelineCompositeColorPathSummary, TimelineCompositeDiagnostics,
    TimelineCompositeDomainBlockerBreakdown, TimelineCompositeElement, TimelineCompositeError,
    TimelineCompositeExecutionDiagnostics, TimelineCompositeFrame,
    TimelineCompositeLegacyBreakdown, TimelineCompositeOptions, TimelineCompositeScratch,
    TimelineCpuCompositeAdmission, TimelineCpuCompositePrecision, TimelineCpuExecutionPolicy,
    TimelineCpuWorkingSetDiagnostics, TimelineCpuWorkingSetError, TimelineCpuWorkingSetEstimate,
    TimelineCpuWorkingSetGrant, TimelineCrossDissolveLayer, TimelineEffectColorRuntime,
    TimelineMediaLayer, TimelineSolidColorLayer, TimelineTransitionInput,
};
pub use timeline_effect_routes::{
    PreparedTimelinePreviewEffectRoute, PreparedTimelinePreviewEffectRoutes,
    TimelinePreviewEffectRouteError,
};
pub use timeline_render_plan::{
    evaluate_prepared_visual_program, evaluate_prepared_visual_program_with_session,
    mat3_to_affine, project_affine_to_sampled_extents, TimelineAdjustmentPlan,
    TimelineBasicTitlePlan, TimelineColorDiagnostic, TimelineCrossDissolvePlan,
    TimelineEvaluationDiagnostics, TimelineEvaluationRequest, TimelineGradePlan, TimelineMediaPlan,
    TimelineNestedSequencePlan, TimelineRenderColorTarget, TimelineRenderIntent,
    TimelineRenderPlan, TimelineRenderPlanElement, TimelineRenderQuality, TimelineRenderSettings,
    TimelineSolidColorPlan, TimelineTransitionInputPlan,
};
pub use timeline_temporal::{
    PreparedTimelineFrameExecution, TimelineFrameExecutionRequest, TimelineFramePreparationError,
    TimelineTemporalDemandBatch, TimelineTemporalPreparationError, TimelineTemporalSource,
    TimelineTemporalSourceDemand,
};
pub use viewer_execution::{
    native_source_texture_format_from_decoded, native_video_sampling_from_decoded,
    ViewerGpuCpuYuvSource, ViewerGpuCrossDissolveLayer, ViewerGpuExecutionLayer,
    ViewerGpuMediaSource, ViewerGpuNativeSource, ViewerGpuSourceLayer, ViewerGpuTransitionInput,
    ViewerHeterogeneousGpuInput, ViewerNativeVideoImportRuntime,
};
pub use viewer_retirement::{
    ViewerCpuYuvUploadWorkerExit, ViewerGpuExecutionRetirement, ViewerGpuRetirementReceipt,
};
pub use viewer_runtime::{
    ViewerGpuExecutionCpuStageTimings, ViewerGpuExecutionError, ViewerGpuExecutionGpuStage,
    ViewerGpuExecutionRecord, ViewerGpuExecutionRequest, ViewerGpuExecutionResidency,
    ViewerGpuExecutionRuntime, ViewerGpuExecutionRuntimeCreateError, ViewerGpuExecutionStageMarker,
    ViewerGpuNativeVideoFacts, ViewerGpuOutputPrecision, ViewerGpuPresentationOutputTakeError,
    ViewerHeterogeneousGpuCompletedBatch, ViewerHeterogeneousGpuSubmissionBatch,
};
pub use viewer_spatial::{GpuViewerSpatialRuntimeDiagnostics, ViewerSourceRect};
pub use viewer_working_set::{
    estimate_viewer_gpu_active_working_set, ViewerGpuActiveTextureDemand,
    ViewerGpuActiveWorkingSetAdmissionError, ViewerGpuActiveWorkingSetDiagnostics,
    ViewerGpuActiveWorkingSetEstimate, ViewerGpuActiveWorkingSetEstimateError,
    ViewerGpuActiveWorkingSetStage, ViewerGpuExecutionResourceGrant,
    PROFESSIONAL_REALTIME_VIEWER_MAX_ACTIVE_TEXTURES,
    PROFESSIONAL_REALTIME_VIEWER_MAX_ACTIVE_TEXTURE_BYTES,
    PROFESSIONAL_REALTIME_VIEWER_MAX_IDLE_PER_CONTRACT,
    PROFESSIONAL_REALTIME_VIEWER_MAX_IDLE_TEXTURE_BYTES,
};
#[cfg(feature = "validation")]
pub use visual_execution_validation::{
    prepared_visual_execution_semantic_trace, PreparedVisualExecutionEffectRequestTrace,
    PreparedVisualExecutionInstanceStepTrace, PreparedVisualExecutionNestedBindingTrace,
    PreparedVisualExecutionNodeTrace, PreparedVisualExecutionSampleTrace,
    PreparedVisualExecutionSemanticTrace, PreparedVisualExecutionTemporalBatchTrace,
    PreparedVisualExecutionTemporalSourceKindTrace, PreparedVisualExecutionTemporalSourceTrace,
};
pub use working_float_policy::{
    product_gpu_working_bytes_per_pixel, product_gpu_working_texture_format,
    GpuWorkingFloat16ImplementationQualification, GpuWorkingFloatBlockers, GpuWorkingFloatDecision,
    GpuWorkingFloatDecisionReason, GpuWorkingFloatFormat, GpuWorkingFloatPerformanceEvidence,
    GpuWorkingFloatPolicy, GpuWorkingFloatPreference, GpuWorkingFloatQualityEvidence,
    PRODUCT_GPU_WORKING_FLOAT_DECISION, PRODUCT_GPU_WORKING_FLOAT_POLICY,
};
mod basic_title;
pub use basic_title::{
    basic_title_raster_request_identity, project_basic_title_transform, BasicTitleFontQuery,
    BasicTitleRasterDiagnostics, BasicTitleRasterError, BasicTitleRasterFrame,
    BasicTitleRasterIdentity, BasicTitleRasterRequestIdentity, BasicTitleRasterizer,
    PreparedBasicTitleFontFace, PreparedBasicTitleFontSet,
};
