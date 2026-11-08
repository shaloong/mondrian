//! # mondrian-effects
//!
//! 高级效果系统：LUT 调色 / 滤镜 / 转场 / 蒙版

pub mod adjustment;
pub mod auxiliary;
mod auxiliary_preview;
pub use auxiliary_preview::{AuxiliaryPreviewFallback, AuxiliaryPreviewFallbackCause};
pub mod displacement_map;
pub use displacement_map::{
    apply_displacement_map_controlled, displacement_map_frame_f32, DisplacementMapBehavior,
    DisplacementMapChannel, DisplacementMapError, DisplacementMapGrade,
    DISPLACEMENT_MAP_MAX_PIXELS,
};
pub use mondrian_core::EffectPassBlendPolicy;
pub mod alpha_contract;
pub mod arithmetic;
mod float_frame;
mod float_working_set;
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
pub use float_working_set::estimate_compiled_float_raster_working_bytes;
pub mod basic_3d;
pub mod black_white;
pub mod blanking_fill;
pub mod box_blur;
pub mod brightness_contrast;
pub mod bulge;
pub mod camera_view;
pub mod cell_pattern;
pub mod change_to_color;
pub mod channel_blur;
pub mod channel_combiner;
pub mod channel_mixer;
pub mod checkerboard;
pub mod choker;
pub mod chroma_key;
pub mod chromatic_aberration_removal;
pub mod circle_ellipse;
pub mod color_balance;
pub mod color_curves;
pub mod color_emboss;
pub mod color_offset;
pub mod convolution;
pub mod corner_pin;
pub mod coverage;
pub mod deband;
pub mod dent;
pub mod directional_blur;
pub mod drop_shadow;
pub mod dust_scratches;
pub mod echo;
pub mod effect;
pub mod emboss;
pub mod find_edges;
pub mod flip;
pub mod four_color_gradient;
pub mod fractal;
pub mod fractal_noise;
mod project_properties;
pub mod radial_blur;
pub(crate) mod resample;
pub mod ripple;
pub mod roughen_edges;
pub mod spherize;
pub mod spill_suppressor;
pub mod split;
pub mod stroke;
pub mod threshold;
pub mod tint;
pub mod transform;
pub mod tritone;
pub mod twirl;
pub mod vibrance;
pub mod vr_project_2d;
pub mod vr_rotate_sphere;
pub mod vr_sphere_to_plane;
pub mod warp;
pub mod wave_warp;
pub mod wire_removal;
pub use project_properties::{rebind_effect_project_geometry, EffectProjectPropertyInitializer};
pub mod execution;
pub mod execution_contract;
pub mod execution_planning;
mod execution_session;
pub mod extension_contract;
pub mod extension_sdk;
pub mod extract;
pub mod eyedropper_fill;
pub mod fill;
pub mod gamut_mapping;
pub mod glow;
pub mod gpu_pipeline;
pub mod gpu_plan;
pub mod graph;
pub mod grid;
pub mod grid_warp;
pub mod hdr_grading;
pub mod hdr_tone_mapping;
pub mod heterogeneous_execution;
pub mod hsl;
pub(crate) mod hsl_space;
pub mod hue_saturation;
pub mod invert;
pub mod key_cleaner;
pub mod keylight;
pub mod lut;
pub mod mask;
pub mod mask_raster;
pub mod matte_refine;
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
pub mod bezier_warp;
pub mod broadcast_safe;
pub mod brush_strokes;
pub mod colorama;
pub mod jpeg_damage;
pub mod kaleidoscope;
pub mod leave_color;
pub mod lens_distortion;
pub mod lens_flare;
pub mod levels;
pub mod linear_color_key;
pub mod magnify;
pub mod median;
pub mod minimax;
pub mod mirror;
pub mod mosaic;
pub mod motion_tile;
pub mod noise;
pub mod offset;
pub(crate) mod parallel;
pub mod photo_filter;
pub mod polar_coordinates;
pub mod posterize;
pub mod qualifier;
pub mod ramp;
pub mod remove_color_matting;
pub mod scatter;
pub mod selective_color;
pub mod shift_channels;
pub mod smart_blur;
pub(crate) mod straight_alpha;
pub mod temporal_execution;
pub mod test_signals;
#[cfg(test)]
mod throughput;
pub mod tracking;
pub mod turbulent_displace;
pub mod unsharp_mask;

pub use adjustment::{
    blend_rgba_f32_pixel, blend_rgba_f32_pixel_seeded, blend_rgba_pixel, blend_rgba_pixel_seeded,
};
pub use arithmetic::{
    arithmetic_frame_f32, ArithmeticError, ArithmeticGrade, ArithmeticOperator,
    ARITHMETIC_MAX_VALUE, ARITHMETIC_MIN_VALUE, ARITHMETIC_OPERATOR_COUNT,
};
pub use basic_3d::{
    basic_3d_frame_f32, Basic3DError, Basic3DGrade, BASIC_3D_MAX_ANGLE, BASIC_3D_MAX_DISTANCE,
};
pub use black_white::{
    BlackAndWhiteError, BlackAndWhiteGrade, BLACK_WHITE_HUE_STOPS, BLACK_WHITE_NEUTRAL_WEIGHT,
    BLACK_WHITE_STOP_KEYS, BLACK_WHITE_WEIGHT_MAX, BLACK_WHITE_WEIGHT_MIN,
};
pub use blanking_fill::{
    blanking_fill_frame_f32, BlankingFillError, BlankingFillGrade, BlankingFillMode,
    BlankingFillParameters, BLANKING_FILL_BLUR_WEIGHTS, BLANKING_FILL_MANUAL_RANGE,
    BLANKING_FILL_MAX_EDGE, BLANKING_FILL_SOURCE_ZOOM_RANGE,
};
pub use box_blur::{box_blur_frame_f32, BoxBlurDirection, BOX_BLUR_AUTHOR_MAX_RADIUS_PIXELS};
pub use brightness_contrast::{
    brightness_contrast_frame_f32, BrightnessContrastError, BrightnessContrastGrade,
    BRIGHTNESS_CONTRAST_MAX_BRIGHTNESS, BRIGHTNESS_CONTRAST_MAX_CONTRAST,
    BRIGHTNESS_CONTRAST_MIN_BRIGHTNESS, BRIGHTNESS_CONTRAST_MIN_CONTRAST,
};
pub use broadcast_safe::{
    broadcast_safe_frame_f32, BroadcastLocale, BroadcastSafeError, BroadcastSafeGrade,
    BroadcastSafeMode, BROADCAST_SAFE_LOCALE_COUNT, BROADCAST_SAFE_MAX_IRE, BROADCAST_SAFE_MIN_IRE,
    BROADCAST_SAFE_MODE_COUNT,
};
pub use camera_view::{
    camera_view_frame_f32, CameraViewError, CameraViewGrade, CAMERA_VIEW_DEFAULT_DISTANCE,
    CAMERA_VIEW_DEFAULT_FILL, CAMERA_VIEW_DEFAULT_FOCAL, CAMERA_VIEW_DEFAULT_ZOOM,
    CAMERA_VIEW_DISTANCE_RANGE, CAMERA_VIEW_FOCAL_RANGE, CAMERA_VIEW_MAX_ANGLE,
    CAMERA_VIEW_ZOOM_RANGE,
};
pub use cell_pattern::{
    cell_pattern_frame_f32, CellPatternControls, CellPatternError, CellPatternGrade,
    CellPatternOverflow, CellPatternType, CELL_PATTERN_DEFAULT_OFFSET,
    CELL_PATTERN_DEFAULT_OVERFLOW, CELL_PATTERN_DEFAULT_PATTERN, CELL_PATTERN_DEFAULT_SIZE,
    CELL_PATTERN_MAX_CELLS, CELL_PATTERN_MAX_CONTRAST, CELL_PATTERN_MAX_COORDINATE,
    CELL_PATTERN_MAX_CYCLE_REVOLUTIONS, CELL_PATTERN_MAX_DISPERSE, CELL_PATTERN_MAX_EVOLUTION,
    CELL_PATTERN_MAX_SEED, CELL_PATTERN_MAX_SIZE, CELL_PATTERN_MIN_SIZE, CELL_PATTERN_SALT_ID,
    CELL_PATTERN_SALT_JITTER, CELL_PATTERN_SALT_MIXED, CELL_PATTERN_SALT_STRIDE,
};
pub use change_to_color::{
    change_to_color_frame_f32, ChangeToColorBy, ChangeToColorChange, ChangeToColorError,
    ChangeToColorGrade, CHANGE_TO_COLOR_BY_COUNT, CHANGE_TO_COLOR_CHANGE_COUNT,
    CHANGE_TO_COLOR_CHROMA_FLOOR, CHANGE_TO_COLOR_MAX_AMOUNT, CHANGE_TO_COLOR_MAX_UNIT,
    CHANGE_TO_COLOR_MIN_AMOUNT, CHANGE_TO_COLOR_MIN_UNIT, CHANGE_TO_COLOR_SOFTNESS_FLOOR,
    CHANGE_TO_COLOR_SQRT_3,
};
pub use channel_blur::{
    channel_blur_frame_f32, ChannelBlurDirection, ChannelBlurError, ChannelBlurGrade,
    CHANNEL_BLUR_DEFAULT_AMOUNT, CHANNEL_BLUR_DEFAULT_EDGE_BEHAVIOR, CHANNEL_BLUR_MAX_RADIUS,
};
pub use chroma_key::{ChromaKeyError, PreparedChromaKey};
pub use chromatic_aberration_removal::{
    chromatic_aberration_removal_frame_f32, ChromaticAberrationRemovalError,
    ChromaticAberrationRemovalGrade, CHROMATIC_ABERRATION_REMOVAL_MAX_AMOUNT,
    CHROMATIC_ABERRATION_REMOVAL_MAX_EDGE_SCALE,
};
pub use color_curves::{
    ColorCurvesAuthoring, ColorCurvesMode, PreparedColorCurves, COLOR_CURVE_SAMPLE_COUNT,
    COLOR_CURVE_SAMPLE_ROWS,
};
pub use color_emboss::{
    ColorEmbossError, ColorEmbossGrade, COLOR_EMBOSS_HALO_PIXELS,
    COLOR_EMBOSS_MAX_CONTRAST_PERCENT, COLOR_EMBOSS_MAX_RELIEF,
};
pub use fractal::{
    apply_inversion, fractal_atan_turns, fractal_frame_f32, fractal_hue_rgb, FractalBudget,
    FractalControls, FractalEquation, FractalError, FractalGrade, FractalOversampleMethod,
    FractalPalette, FractalSetChoice, FRACTAL_BASE_EXTENT, FRACTAL_DEFAULT_CYCLE_OFFSET,
    FRACTAL_DEFAULT_CYCLE_STEPS, FRACTAL_DEFAULT_EDGE_HIGHLIGHT, FRACTAL_DEFAULT_EQUATION,
    FRACTAL_DEFAULT_ESCAPE_LIMIT, FRACTAL_DEFAULT_HUE, FRACTAL_DEFAULT_JULIA_MAGNIFICATION,
    FRACTAL_DEFAULT_JULIA_X, FRACTAL_DEFAULT_JULIA_Y, FRACTAL_DEFAULT_MANDELBROT_MAGNIFICATION,
    FRACTAL_DEFAULT_MANDELBROT_X, FRACTAL_DEFAULT_MANDELBROT_Y, FRACTAL_DEFAULT_OVERLAY,
    FRACTAL_DEFAULT_OVERSAMPLE_FACTOR, FRACTAL_DEFAULT_OVERSAMPLE_METHOD, FRACTAL_DEFAULT_PALETTE,
    FRACTAL_DEFAULT_POST_INVERSION_OFFSET, FRACTAL_DEFAULT_SET_CHOICE,
    FRACTAL_DEFAULT_TRANSPARENCY, FRACTAL_EDGE_PHASE_EPSILON, FRACTAL_GHOST_MIN_ITERATIONS,
    FRACTAL_ITERATION_SAMPLE_BUDGET, FRACTAL_MAX_EDGE_WIDTH, FRACTAL_MAX_ITERATIONS,
    FRACTAL_MAX_SAMPLES, FRACTAL_MIN_BAILOUT_R2, FRACTAL_MIN_INVERSION_R2,
};

pub use auxiliary::{
    render_op_auxiliary_scratch_frames, sample_auxiliary, sample_auxiliary_f32,
    AuxiliaryFloatFrame, AuxiliaryFloatInput, AuxiliaryFrame, AuxiliaryPolicy, AuxiliaryResolution,
    BILINEAR_TAPS, IDENTITY_AFFINE, PIXEL_CENTRE,
};
pub use bevel::{bevel_frame_f32, BevelError, BevelGrade, BEVEL_HALO_PIXELS, BEVEL_MAX_THICKNESS};
pub use bezier_warp::{
    apply_bezier_warp_f32_controlled, bezier_warp_frame_f32, BezierWarpControls, BezierWarpError,
    BezierWarpGrade, BEZIER_WARP_DEFAULT_QUALITY, BEZIER_WARP_MAX_QUALITY, BEZIER_WARP_MIN_QUALITY,
    BEZIER_WARP_REFERENCE_EXTENT,
};
pub use brush_strokes::{
    brush_strokes_frame_f32, BrushStrokesError, BrushStrokesGrade, BRUSH_STROKES_MAX_DENSITY,
    BRUSH_STROKES_MAX_LENGTH, BRUSH_STROKES_MAX_RANDOMNESS, BRUSH_STROKES_MAX_SIZE,
    BRUSH_STROKES_SURFACE_COUNT,
};
pub use bulge::{
    bulge_frame_f32, BulgeError, BulgeGrade, BULGE_MAX_HEIGHT, BULGE_MAX_RADIUS_PIXELS,
    BULGE_MAX_TAPER_PIXELS, BULGE_MIN_HEIGHT, BULGE_MIN_RADIUS_PIXELS, BULGE_MIN_TAPER_PIXELS,
};
pub use circle_ellipse::{
    circle_ellipse_frame_f32, circle_ellipse_pixel, CircleEllipseControls, CircleEllipseError,
    CircleEllipseGrade, CIRCLE_ELLIPSE_DEFAULT_CENTER, CIRCLE_ELLIPSE_DEFAULT_CONTEXT_SLIDER,
    CIRCLE_ELLIPSE_DEFAULT_EDGE, CIRCLE_ELLIPSE_DEFAULT_OPACITY, CIRCLE_ELLIPSE_DEFAULT_RADIUS,
    CIRCLE_ELLIPSE_MAX_CONTEXT_SLIDER, CIRCLE_ELLIPSE_MAX_COORDINATE, CIRCLE_ELLIPSE_MAX_EDGE,
    CIRCLE_ELLIPSE_MAX_FEATHER, CIRCLE_ELLIPSE_MAX_RADIUS, CIRCLE_ELLIPSE_MIN_EDGE,
};
pub use color_offset::{
    color_offset_frame_f32, ColorOffsetError, ColorOffsetGrade, ColorOffsetOverflow,
    COLOR_OFFSET_MAX_PHASE_PIXELS, COLOR_OFFSET_OVERFLOW_COUNT,
};
pub use colorama::{
    colorama_frame_f32, ColoramaAddMode, ColoramaError, ColoramaGpuControls, ColoramaGrade,
    ColoramaInterpolation, ColoramaMaskMode, ColoramaMatchMode, ColoramaModify, ColoramaPalette,
    ColoramaPhaseSource, ColoramaSettings, COLORAMA_ADD_MODE_COUNT, COLORAMA_DEFAULT_BLEND,
    COLORAMA_DEFAULT_MASK_LAYER, COLORAMA_DEFAULT_MATCH_COLOR, COLORAMA_DEFAULT_MODIFY,
    COLORAMA_DEFAULT_PALETTE, COLORAMA_DEFAULT_PHASE_SHIFT, COLORAMA_DEFAULT_PHASE_SOURCE,
    COLORAMA_DEFAULT_REPETITIONS, COLORAMA_DEFAULT_SOFTNESS, COLORAMA_DEFAULT_TOLERANCE,
    COLORAMA_INTERPOLATION_COUNT, COLORAMA_KIND, COLORAMA_MASK_MODE_COUNT,
    COLORAMA_MATCH_MODE_COUNT, COLORAMA_MAX_BLEND, COLORAMA_MAX_REPETITIONS, COLORAMA_MODIFY_COUNT,
    COLORAMA_PALETTE_COUNT, COLORAMA_PHASE_SOURCE_COUNT, COLORAMA_STOP_COUNT,
};
pub use convolution::{
    convolution_frame_f32, ConvolutionError, ConvolutionGrade, CONVOLUTION_HALO_PIXELS,
    CONVOLUTION_MAX_CELL, CONVOLUTION_MIN_CELL,
};
pub use corner_pin::{
    corner_pin_frame_f32, CornerPinError, CornerPinGrade, CORNER_PIN_CORNER_COUNT,
    CORNER_PIN_MAX_COORDINATE, CORNER_PIN_MIN_COORDINATE,
};
pub use coverage::{has_positive_coverage, mix_straight_rgba, straight_rgba_from_premultiplied};
pub use deband::{
    deband_frame_f32, DebandError, DebandGrade, DEBAND_DITHER_SALT_A, DEBAND_DITHER_SALT_B,
    DEBAND_MAX_EDGE_THRESHOLD, DEBAND_MAX_POST_OPTIMIZATION, DEBAND_MAX_RADIUS, DEBAND_MIN_RADIUS,
};
pub use dent::{
    dent_frame_f32, DentEdge, DentError, DentGrade, DentKind, DENT_DEFAULT_CENTRE_PIXELS,
    DENT_DEFAULT_SIZE_FRACTION, DENT_DEFAULT_STRENGTH, DENT_MAX_SIZE_FRACTION, DENT_MAX_STRENGTH,
    DENT_MIN_SIZE_FRACTION, DENT_MIN_STRENGTH,
};
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
pub use extract::{
    extract_frame_f32, ExtractChannel, ExtractError, ExtractGrade, EXTRACT_AUTHOR_MAX_LEVEL,
    EXTRACT_CHANNEL_COUNT,
};
pub use eyedropper_fill::{
    eyedropper_fill_frame_f32, AverageMode, EyedropperFillError, EyedropperFillGrade,
    EYEDROPPER_MAX_COORDINATE, EYEDROPPER_MAX_RADIUS, EYEDROPPER_MIN_RADIUS,
};
pub use fill::{
    fill_frame_f32, FillControls, FillError, FillGrade, FILL_DEFAULT_ALL_MASKS, FILL_DEFAULT_COLOR,
    FILL_DEFAULT_FEATHER, FILL_DEFAULT_FILL_MASK, FILL_DEFAULT_OPACITY, FILL_MAX_ALL_MASKS,
    FILL_MAX_FEATHER, FILL_MAX_OPACITY,
};
pub use gamut_mapping::{GamutCompressionGrade, GamutMappingError, HighlightRecoveryGrade};
pub use glow::{
    glow_frame_f32, GlowBasedOn, GlowColors, GlowComposite, GlowDimensions, GlowError, GlowGrade,
    GlowOperation, GLOW_AUTHOR_MAX_COLOR_LOOPS, GLOW_AUTHOR_MAX_COLOR_PHASE_DEGREES,
    GLOW_AUTHOR_MAX_MIDPOINT, GLOW_AUTHOR_MAX_RADIUS_PIXELS, GLOW_AUTHOR_MAX_STRENGTH,
    GLOW_AUTHOR_MAX_THRESHOLD, GLOW_AUTHOR_MIN_COLOR_LOOPS, GLOW_AUTHOR_MIN_MIDPOINT,
    GLOW_BLUR_ITERATIONS, GLOW_DEFAULT_COLOR_LOOPS, GLOW_DEFAULT_COLOR_PHASE,
    GLOW_DEFAULT_MIDPOINT, GLOW_DEFAULT_RADIUS_PIXELS, GLOW_DEFAULT_STRENGTH,
    GLOW_DEFAULT_THRESHOLD, GLOW_INTENSITY_SCALE,
};
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
    collect_auxiliary_inputs, compile_reference_effect_graph,
    compile_reference_effect_graph_in_domain, compile_reference_render_graph,
    identity_compiled_effect_graph, prepare_effect_graph_topology, validate_auxiliary_graph,
    AuxiliaryCoordinateMapping, AuxiliaryFilter, AuxiliaryInputSpec, AuxiliaryOutOfBounds,
    AuxiliaryRole, AuxiliarySampling, AuxiliarySourceBinding, CompiledEffectDomainPlan,
    CompiledEffectGraph, CompiledEffectStageBinding, EffectDomainBlocker, EffectDomainBlockerKind,
    EffectDomainTransition, EffectGraphBuilderState, EffectGraphError, EffectGraphNode,
    EffectGraphNodeId, EffectGraphNodeKind, EffectGraphValue, EffectRenderGraph, EffectStageOrigin,
    PreparedEffectGraphTopology, UNBOUND_AUXILIARY_SLOT,
};
pub use grid::{
    grid_frame_f32, grid_pixel, GridControls, GridError, GridGrade, GridSizeFrom,
    GRID_DEFAULT_ANCHOR, GRID_DEFAULT_BORDER, GRID_DEFAULT_CORNER, GRID_DEFAULT_HEIGHT,
    GRID_DEFAULT_OPACITY, GRID_DEFAULT_SIZE_FROM, GRID_DEFAULT_WIDTH, GRID_DEGENERATE_CELL,
    GRID_MAX_BORDER, GRID_MAX_COORDINATE, GRID_MAX_FEATHER, GRID_MAX_WIDTH, GRID_MIN_WIDTH,
};
pub use grid_warp::{
    grid_warp_frame_f32, GridWarpError, GridWarpGrade, GRID_WARP_AMPLITUDE_RATIO,
    GRID_WARP_DEFAULT_AXIS, GRID_WARP_DEFAULT_QUALITY, GRID_WARP_IDENTITY_MAX_AXIS,
    GRID_WARP_MAX_AXIS, GRID_WARP_MAX_BLEND_SLOPE, GRID_WARP_MIN_AXIS, GRID_WARP_QUALITY_MAX,
    GRID_WARP_QUALITY_MIN,
};
pub use hdr_grading::{
    HdrGradingAuthoring, HdrGradingError, HdrGradingZone, HdrZoneControl, PreparedHdrGrading,
    HDR_GRADING_MAX_STOPS, HDR_GRADING_MIN_STOPS, HDR_GRADING_SAMPLE_COUNT,
    HDR_GRADING_SAMPLE_ROWS, HDR_GRADING_ZONE_COUNT,
};
pub use hdr_tone_mapping::{
    hdr_tone_mapping_frame_f32, HdrToneMappingError, HdrToneMappingGrade, ToneMapMode,
    HDR_TONE_MAPPING_MAX_GAIN, HDR_TONE_MAPPING_MAX_GAMMA, HDR_TONE_MAPPING_MIN_GAIN,
    HDR_TONE_MAPPING_MIN_GAMMA, HDR_TONE_MAPPING_MODE_COUNT, HDR_TONE_MAPPING_WHITE_POINT_INVERSE,
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
pub use jpeg_damage::{
    JpegDamageError, JpegDamageGrade, JpegDamageScalingComponent, JPEG_DAMAGE_BLOCK_SIZE,
    JPEG_DAMAGE_DEFAULT_COMPONENT, JPEG_DAMAGE_DEFAULT_FREQUENCY_SCALE,
    JPEG_DAMAGE_DEFAULT_MACROBLOCK_ASPECT, JPEG_DAMAGE_DEFAULT_QUALITY,
    JPEG_DAMAGE_DEFAULT_RESOLUTION, JPEG_DAMAGE_KIND, JPEG_DAMAGE_MAX_MACROBLOCK_ASPECT,
    JPEG_DAMAGE_MAX_RESOLUTION, JPEG_DAMAGE_MIN_MACROBLOCK_ASPECT,
};
pub use kaleidoscope::{
    kaleidoscope_frame_f32, KaleidoscopeError, KaleidoscopeGrade, KALEIDOSCOPE_MAX_MIRRORING,
    KALEIDOSCOPE_MAX_SIZE, KALEIDOSCOPE_MIN_MIRRORING, KALEIDOSCOPE_MIN_SIZE,
};
pub use leave_color::{
    leave_color_frame_f32, LeaveColorError, LeaveColorGrade, LeaveColorMatch,
    LEAVE_COLOR_MATCH_COUNT, LEAVE_COLOR_MAX_PERCENT, LEAVE_COLOR_MIN_PERCENT,
};
pub use lens_distortion::{
    lens_distortion_frame_f32, LensDistortionDirection, LensDistortionError, LensDistortionGrade,
    LensDistortionResize, LENS_DISTORTION_DIRECTION_COUNT, LENS_DISTORTION_MAX_CENTRE_PIXELS,
    LENS_DISTORTION_MAX_FOV_DEGREES, LENS_DISTORTION_MIN_FOV_DEGREES, LENS_DISTORTION_RESIZE_COUNT,
};
pub use lens_flare::{
    lens_flare_frame_f32, LensFlareError, LensFlareGrade, LensFlareType,
    LENS_FLARE_MAX_BRIGHTNESS_PERCENT,
};
pub use levels::{
    levels_frame_f32, LevelsChannel, LevelsClip, LevelsError, LevelsGrade, LEVELS_CHANNEL_COUNT,
    LEVELS_CLIP_COUNT, LEVELS_MAX_GAMMA, LEVELS_MAX_LEVEL, LEVELS_MIN_GAMMA, LEVELS_MIN_LEVEL,
};
pub use linear_color_key::{
    linear_color_key_frame_f32, KeyOperation, LinearColorKeyError, LinearColorKeyGrade,
    MatchColours, LINEAR_COLOR_KEY_MAX_PERCENT,
};
pub use lut::{
    Lut3D, LutLibrary, LutLibraryEntry, LutPreparationCache, LutPreparationCacheConfig,
    LutPreparationCacheDiagnostics,
};
pub use magnify::{
    is_declared_blend_mode, magnify_frame_f32, MagnifyError, MagnifyGrade, MagnifyLink,
    MagnifyScaling, MagnifyShape, MAGNIFY_MAX_DIAMETER_PIXELS, MAGNIFY_MAX_FEATHER_PIXELS,
    MAGNIFY_MIN_DIAMETER_PIXELS,
};
pub use mask::{
    BezierPoint, MaskComponent, MaskEvaluation, MaskId, MaskOp, MaskShape, MaskShapeInterpolation,
    MaskShapeKeyframe, MAX_MASK_PATH_POINTS,
};
pub use mask_raster::{rasterize_mask_shape, MaskRasterError, PreparedMaskRaster};
pub use mirror::{
    mirror_frame_f32, MirrorError, MirrorGrade, MIRROR_MAX_ANGLE_DEGREES, MIRROR_MAX_CENTRE_PIXELS,
};
pub use mosaic::{
    apply_mosaic_f32_controlled, mosaic_frame_f32, MosaicError, MosaicGrade,
    MOSAIC_MAX_BLOCK_PIXELS, MOSAIC_MIN_BLOCK_PIXELS,
};
pub use motion_tile::{
    motion_tile_frame_f32, MotionTileError, MotionTileGrade, MOTION_TILE_MAX_PERCENT,
};
pub use noise::{
    noise_frame_f32, noise_hash, noise_sample, NoiseError, NoiseGrade, NoiseType,
    NOISE_MAX_AMOUNT_PERCENT, NOISE_SALT_BLUE, NOISE_SALT_GREEN, NOISE_SALT_LUMINANCE,
    NOISE_SALT_RED,
};
pub use offset::{
    offset_frame_f32, OffsetError, OffsetGrade, OFFSET_MAX_BLEND_PERCENT, OFFSET_MAX_SHIFT_PIXELS,
};
pub use photo_filter::{
    photo_filter_frame_f32, PhotoFilterError, PhotoFilterGrade, PhotoFilterPreset,
    PHOTO_FILTER_DEFAULT_COLOR, PHOTO_FILTER_DEFAULT_DENSITY, PHOTO_FILTER_MAX_DENSITY,
    PHOTO_FILTER_MIN_DENSITY,
};
pub use polar_coordinates::{
    polar_coordinates_frame_f32, PolarConversion, PolarCoordinatesGrade, PolarInterpolation,
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
pub use radial_blur::{
    radial_blur_frame_f32, radial_blur_jitter, RadialBlurError, RadialBlurGrade, RadialBlurMode,
    RADIAL_BLUR_AUTHOR_MAX_AMOUNT, RADIAL_BLUR_MAX_TAPS,
};
pub use ramp::{
    ramp_frame_f32, RampError, RampGrade, RampShape, RAMP_DITHER_SALT, RAMP_MAX_BLEND,
    RAMP_MAX_CENTRE_PIXELS, RAMP_MAX_SCATTER, RAMP_SHAPE_COUNT,
};
pub use remove_color_matting::{
    remove_color_matting_frame_f32, RemoveColorMattingError, RemoveColorMattingGrade,
};
pub use scatter::{
    scatter_frame_f32, ScatterError, ScatterGrade, SCATTER_MAX_AMOUNT_PIXELS, SCATTER_MAX_COPIES,
};
pub use selective_color::{
    selective_color_frame_f32, SelectiveColorError, SelectiveColorGrade, SelectiveColorMethod,
    SelectiveColorRange, CHROMA_GATE_END, CHROMA_GATE_START, LUMA_PIVOT, MIN_MAX_CHANNEL,
    SELECTIVE_COLOR_SLIDER_MAX, SELECTIVE_COLOR_SLIDER_MIN, TENT_HALF_WIDTH_DEGREES,
};
pub use smart_blur::{
    smart_blur_frame_f32, SmartBlurError, SmartBlurGrade, SmartBlurMode,
    SMART_BLUR_AUTHOR_MAX_RADIUS_PIXELS, SMART_BLUR_AUTHOR_MIN_RADIUS_PIXELS,
    SMART_BLUR_DEFAULT_RADIUS_PIXELS, SMART_BLUR_DEFAULT_THRESHOLD, SMART_BLUR_EDGE_GAIN,
    SMART_BLUR_MAX_THRESHOLD, SMART_BLUR_MIN_THRESHOLD,
};
pub use spherize::{
    spherize_frame_f32, SpherizeError, SpherizeGrade, SPHERIZE_MAX_RADIUS_PIXELS,
    SPHERIZE_MIN_RADIUS_PIXELS,
};
pub use spill_suppressor::{
    spill_suppressor_frame_f32, SpillSuppressorError, SpillSuppressorGrade, SpillSuppressorMethod,
    SPILL_SUPPRESSOR_DEFAULT, SPILL_SUPPRESSOR_DEFAULT_COLOUR_CORRECTION,
    SPILL_SUPPRESSOR_DEFAULT_DESATURATION, SPILL_SUPPRESSOR_DEFAULT_LUMINANCE_CORRECTION,
    SPILL_SUPPRESSOR_DEFAULT_RANGE, SPILL_SUPPRESSOR_DEFAULT_TOLERANCE,
    SPILL_SUPPRESSOR_MAX_AMOUNT, SPILL_SUPPRESSOR_METHOD_COUNT, SPILL_SUPPRESSOR_MIN_AMOUNT,
};
pub use split::{
    split_frame_f32, SplitError, SplitGrade, SPLIT_MAX_COORDINATE, SPLIT_MAX_PIXELS,
    SPLIT_MIN_PIXELS,
};
pub use stroke::{
    stroke_frame_f32, StrokeControls, StrokeError, StrokeGrade, StrokePaintStyle,
    STROKE_DEFAULT_BRUSH_HARDNESS, STROKE_DEFAULT_BRUSH_SIZE, STROKE_DEFAULT_COLOR,
    STROKE_DEFAULT_OPACITY, STROKE_MAX_BRUSH_HARDNESS, STROKE_MAX_BRUSH_SIZE,
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
pub use tint::{tint_frame_f32, TintError, TintGrade};
pub use tracking::{
    canonicalize_tracking_shape, track_frame_pair, transform_tracking_shape, TrackingError,
    TrackingFrame, TrackingObservation, TrackingQuality, TrackingRegion, TrackingTransform,
};
pub use transform::{
    transform_frame_f32, TransformError, TransformGrade, TransformSampling,
    TRANSFORM_MAX_AXIS_DEGREES, TRANSFORM_MAX_CENTRE_PIXELS, TRANSFORM_MAX_ROTATION_DEGREES,
    TRANSFORM_MAX_SCALE_PERCENT, TRANSFORM_MAX_SHUTTER_DEGREES, TRANSFORM_MAX_SKEW_DEGREES,
    TRANSFORM_MIN_SCALE_PERCENT, TRANSFORM_MIN_SKEW_DEGREES, TRANSFORM_SAMPLING_COUNT,
};
pub use tritone::{tritone_frame_f32, TritoneError, TritoneGrade};
pub use turbulent_displace::{
    turbulent_displace_frame_f32, DisplaceAntialias, DisplacePin, DisplacementType,
    TurbulentDisplaceError, TurbulentDisplaceGrade, TURBULENT_ANTIALIAS_COUNT,
    TURBULENT_MAX_AMOUNT, TURBULENT_MAX_COMPLEXITY, TURBULENT_MAX_CYCLE,
    TURBULENT_MAX_OFFSET_PIXELS, TURBULENT_MAX_RANDOM_SEED, TURBULENT_MAX_SIZE,
    TURBULENT_MIN_AMOUNT, TURBULENT_MIN_COMPLEXITY, TURBULENT_MIN_CYCLE, TURBULENT_MIN_SIZE,
    TURBULENT_PIN_COUNT, TURBULENT_TYPE_COUNT,
};
pub use twirl::{
    twirl_frame_f32, TwirlError, TwirlGrade, TWIRL_MAX_ANGLE_DEGREES, TWIRL_MAX_RADIUS_PERCENT,
    TWIRL_MIN_RADIUS_PERCENT,
};
pub use unsharp_mask::{
    apply_unsharp_mask_f32, sharpened_channel, unsharp_mask_gain,
    UNSHARP_MASK_AUTHOR_MAX_AMOUNT_PERCENT, UNSHARP_MASK_AUTHOR_MAX_RADIUS_PIXELS,
    UNSHARP_MASK_AUTHOR_MIN_RADIUS_PIXELS,
};
pub use vibrance::{vibrance_frame_f32, VibranceError, VibranceGrade};
pub use vr_project_2d::{
    vr_project_2d_frame_f32, VrProject2DControls, VrProject2DError, VrProject2DGrade,
    VR_PROJECT_2D_DEFAULT_FEATHER_PERCENT, VR_PROJECT_2D_DEFAULT_HORIZONTAL_FOV_DEGREES,
    VR_PROJECT_2D_DEFAULT_SCALE_DEGREES, VR_PROJECT_2D_DEFAULT_STEREO_DISPARITY_DEGREES,
    VR_PROJECT_2D_DEFAULT_VERTICAL_FOV_DEGREES, VR_PROJECT_2D_MAX_FEATHER_PERCENT,
    VR_PROJECT_2D_MAX_HORIZONTAL_FOV_DEGREES, VR_PROJECT_2D_MAX_SCALE_DEGREES,
    VR_PROJECT_2D_MAX_STEREO_DISPARITY_DEGREES, VR_PROJECT_2D_MAX_VERTICAL_FOV_DEGREES,
    VR_PROJECT_2D_MIN_FEATHER_PERCENT, VR_PROJECT_2D_MIN_HORIZONTAL_FOV_DEGREES,
    VR_PROJECT_2D_MIN_SCALE_DEGREES, VR_PROJECT_2D_MIN_STEREO_DISPARITY_DEGREES,
    VR_PROJECT_2D_MIN_VERTICAL_FOV_DEGREES,
};
pub use vr_rotate_sphere::{
    vr_rotate_sphere_frame_f32, VrFrameLayout, VrRotateSphereControls, VrRotateSphereError,
    VrRotateSphereGrade, VR_ROTATE_SPHERE_MAX_HORIZONTAL_FOV_DEGREES,
    VR_ROTATE_SPHERE_MAX_VERTICAL_FOV_DEGREES, VR_ROTATE_SPHERE_MIN_FOV_DEGREES,
};
pub use vr_sphere_to_plane::{
    vr_sphere_to_plane_frame_f32, VrSphereToPlaneControls, VrSphereToPlaneError,
    VrSphereToPlaneGrade, VR_SPHERE_TO_PLANE_DEFAULT_FOV_DEGREES,
    VR_SPHERE_TO_PLANE_MAX_FOV_DEGREES, VR_SPHERE_TO_PLANE_MIN_FOV_DEGREES,
};
pub use warp::{
    warp_frame_f32, WarpAxis, WarpError, WarpGrade, WarpStyle, WARP_DEFAULT_AXIS,
    WARP_DEFAULT_BEND, WARP_DEFAULT_DISTORTION, WARP_DEFAULT_STYLE, WARP_IDENTITY_EPSILON,
    WARP_MAX_BEND, WARP_MAX_DISTORTION, WARP_MIN_BEND, WARP_MIN_DISTORTION,
};

pub use processor_question::{
    EffectProcessorGuiActionFeedback, EffectProcessorGuiActionReceipt, EffectProcessorGuiAdmission,
    EffectProcessorGuiPresence, EffectProcessorGuiState, EffectProcessorLiveQuestion,
    EffectProcessorQuestionId, EffectProcessorQuestionReceipt, EffectProcessorQuestionReply,
    EffectProcessorQuestionService, EffectProcessorQuestionState, EffectProcessorQuestionView,
};

#[cfg(test)]
mod image_scale_admission_tests;

pub use channel_combiner::{
    channel_combiner_frame_f32, ChannelCombinerChannel, ChannelCombinerDerived,
    ChannelCombinerError, ChannelCombinerGrade, ChannelCombinerSlot,
};
pub use channel_mixer::{
    ChannelMixerError, ChannelMixerGrade, CHANNEL_MIXER_WEIGHT_COUNT, CHANNEL_MIXER_WEIGHT_MAX,
    CHANNEL_MIXER_WEIGHT_MIN,
};
pub use checkerboard::{
    checkerboard_frame_f32, checkerboard_pixel, CheckerboardControls, CheckerboardError,
    CheckerboardGrade, CheckerboardSizeFrom, CHECKERBOARD_DEFAULT_ANCHOR,
    CHECKERBOARD_DEFAULT_CORNER, CHECKERBOARD_DEFAULT_HEIGHT, CHECKERBOARD_DEFAULT_OPACITY,
    CHECKERBOARD_DEFAULT_SIZE_FROM, CHECKERBOARD_DEFAULT_WIDTH, CHECKERBOARD_DEGENERATE_CELL,
    CHECKERBOARD_MAX_COORDINATE, CHECKERBOARD_MAX_FEATHER, CHECKERBOARD_MAX_WIDTH,
    CHECKERBOARD_MIN_WIDTH,
};
pub use choker::{
    choker_frame_f32, ChokerError, ChokerGrade, CHOKER_AUTHOR_MAX_CHOKE_PIXELS,
    CHOKER_DEFAULT_CHOKE_PIXELS,
};
pub use color_balance::{
    color_balance_frame_f32, ColorBalanceError, ColorBalanceGrade, COLOR_BALANCE_DEFAULT_BALANCE,
    COLOR_BALANCE_MAX_BALANCE, COLOR_BALANCE_MIN_BALANCE,
};
pub use directional_blur::{
    directional_blur_step, directional_blur_taps, DIRECTIONAL_BLUR_AUTHOR_MAX_LENGTH_PIXELS,
    MAX_DIRECTIONAL_BLUR_TAPS,
};
pub use drop_shadow::{
    DropShadowError, DropShadowGrade, DROP_SHADOW_AUTHOR_MAX_DISTANCE_PIXELS,
    DROP_SHADOW_AUTHOR_MAX_OPACITY, DROP_SHADOW_AUTHOR_MAX_SOFTNESS_PIXELS,
};
pub use dust_scratches::{
    dust_scratches_frame_f32, DustScratchesError, DustScratchesGrade,
    DUST_SCRATCHES_DEFAULT_OPERATE_ON_ALPHA, DUST_SCRATCHES_DEFAULT_RADIUS,
    DUST_SCRATCHES_DEFAULT_THRESHOLD, DUST_SCRATCHES_GPU_MAX_RADIUS, DUST_SCRATCHES_MAX_RADIUS,
};
pub use echo::{
    echo_offset, EchoError, EchoGrade, EchoOperator, EchoTap, ECHO_AUTHOR_MAX_COUNT,
    ECHO_DEFAULT_COUNT, ECHO_DEFAULT_DECAY_PERCENT, ECHO_DEFAULT_STARTING_INTENSITY,
    ECHO_DEFAULT_TIME_SECONDS, ECHO_MAX_DECAY_PERCENT, ECHO_MAX_TAPS, ECHO_MAX_TIME_SECONDS,
    ECHO_OPERATOR_COUNT, ECHO_TIMESCALE,
};
pub use emboss::{
    EmbossError, EmbossGrade, EMBOSS_HALO_PIXELS, EMBOSS_MAX_CONTRAST_PERCENT, EMBOSS_MAX_RELIEF,
};
pub use find_edges::{
    FindEdgesError, FindEdgesGrade, FIND_EDGES_HALO_PIXELS, FIND_EDGES_MAX_HALO_PIXELS,
};
pub use flip::{
    flip_frame_f32, FlipAxis, FlipError, FlipGrade, FLIP_AXIS_COUNT, FLIP_MAX_BLEND_PERCENT,
    FLIP_MAX_CENTRE_PIXELS,
};
pub use four_color_gradient::{
    four_color_gradient_frame_f32, four_color_gradient_pixel, FourColorGradientError,
    FourColorGradientGrade, FOUR_COLOR_GRADIENT_DEFAULT_BLEND, FOUR_COLOR_GRADIENT_DEFAULT_OPACITY,
    FOUR_COLOR_GRADIENT_DITHER_SALT, FOUR_COLOR_GRADIENT_MAX_BLEND, FOUR_COLOR_GRADIENT_MAX_DITHER,
    FOUR_COLOR_GRADIENT_MAX_OPACITY, FOUR_COLOR_GRADIENT_MAX_POINT, FOUR_COLOR_GRADIENT_MIN_BLEND,
    FOUR_COLOR_GRADIENT_MIN_DITHER, FOUR_COLOR_GRADIENT_MIN_OPACITY,
    FOUR_COLOR_GRADIENT_POINT_COUNT,
};
pub use fractal_noise::{
    fractal_noise_frame_f32, FractalNoiseError, FractalNoiseGrade, FractalNoiseType,
    FractalNoiseUniform, FractalOverflow, FractalType, FRACTAL_FEATURE_PIXELS,
    FRACTAL_MAX_BRIGHTNESS, FRACTAL_MAX_CENTRE_AUXILIARY, FRACTAL_MAX_COMPLEXITY,
    FRACTAL_MAX_CONTRAST, FRACTAL_MAX_CYCLE, FRACTAL_MAX_OFFSET_PIXELS, FRACTAL_MAX_OPACITY,
    FRACTAL_MAX_RANDOM_SEED, FRACTAL_MAX_ROTATION_DEGREES, FRACTAL_MAX_SCALE,
    FRACTAL_MAX_SUB_INFLUENCE, FRACTAL_MAX_SUB_SCALE, FRACTAL_MIN_BRIGHTNESS,
    FRACTAL_MIN_CENTRE_AUXILIARY, FRACTAL_MIN_COMPLEXITY, FRACTAL_MIN_CONTRAST, FRACTAL_MIN_CYCLE,
    FRACTAL_MIN_OPACITY, FRACTAL_MIN_SCALE, FRACTAL_MIN_SUB_INFLUENCE, FRACTAL_MIN_SUB_SCALE,
    FRACTAL_NOISE_TYPE_COUNT, FRACTAL_OVERFLOW_COUNT, FRACTAL_TYPE_COUNT,
};
pub use key_cleaner::{
    key_cleaner_frame_f32, KeyCleanerError, KeyCleanerGrade,
    KEY_CLEANER_AUTHOR_MAX_EDGE_RADIUS_PIXELS, KEY_CLEANER_DEFAULT_ALPHA_CONTRAST,
    KEY_CLEANER_DEFAULT_EDGE_RADIUS_PIXELS, KEY_CLEANER_DEFAULT_REDUCE_CHATTER,
    KEY_CLEANER_DEFAULT_STRENGTH,
};
pub use keylight::{
    keylight_frame_f32, keylight_key_channel, keylight_other_channels, KeylightDisplay,
    KeylightError, KeylightGrade, KEYLIGHT_AUTHOR_MAX_DESPILL,
    KEYLIGHT_AUTHOR_MAX_EDGE_FEATHER_PIXELS, KEYLIGHT_AUTHOR_MAX_EDGE_SHRINK_PIXELS,
    KEYLIGHT_AUTHOR_MAX_MATTE_PERCENT, KEYLIGHT_AUTHOR_MAX_PREBLUR_PIXELS,
    KEYLIGHT_AUTHOR_MAX_SCREEN_GAIN, KEYLIGHT_DEFAULT_DESPILL, KEYLIGHT_DEFAULT_DESPILL_BIAS,
    KEYLIGHT_DEFAULT_EDGE_FEATHER_PIXELS, KEYLIGHT_DEFAULT_EDGE_SHRINK_PIXELS,
    KEYLIGHT_DEFAULT_INNER_MATTE, KEYLIGHT_DEFAULT_OUTER_MATTE, KEYLIGHT_DEFAULT_PREBLUR_PIXELS,
    KEYLIGHT_DEFAULT_SCREEN_BALANCE, KEYLIGHT_DEFAULT_SCREEN_COLOR, KEYLIGHT_DEFAULT_SCREEN_GAIN,
};
pub use matte_refine::{
    matte_refine_frame_f32, MatteRefineError, MatteRefineGrade,
    MATTE_REFINE_AUTHOR_MAX_CLEAN_AMOUNT, MATTE_REFINE_AUTHOR_MAX_CONTRAST,
    MATTE_REFINE_AUTHOR_MAX_EDGE_DETAIL, MATTE_REFINE_AUTHOR_MAX_EXPAND_RADIUS_PIXELS,
    MATTE_REFINE_AUTHOR_MAX_FEATHER_PIXELS, MATTE_REFINE_AUTHOR_MAX_MORE_MOTION_BLUR,
    MATTE_REFINE_AUTHOR_MAX_OTHER_EDGE_RADIUS_PIXELS, MATTE_REFINE_AUTHOR_MAX_PER_FRAME_SAMPLES,
    MATTE_REFINE_AUTHOR_MAX_SHIFT_EDGE_PIXELS, MATTE_REFINE_AUTHOR_MAX_SHUTTER_ANGLE_DEGREES,
    MATTE_REFINE_AUTHOR_MAX_SMOOTH_PIXELS, MATTE_REFINE_AUTHOR_MAX_TREMOR_AMOUNT,
    MATTE_REFINE_AUTHOR_MAX_TREMOR_REDUCTION, MATTE_REFINE_AUTHOR_MAX_VIEW,
    MATTE_REFINE_DEFAULT_BETTER_QUALITY, MATTE_REFINE_DEFAULT_CLEAN_AMOUNT,
    MATTE_REFINE_DEFAULT_CLEAN_EDGE_COLOR, MATTE_REFINE_DEFAULT_CONTRAST,
    MATTE_REFINE_DEFAULT_EDGE_DETAIL, MATTE_REFINE_DEFAULT_EXPAND_RADIUS_PIXELS,
    MATTE_REFINE_DEFAULT_EXTEND_SMOOTH, MATTE_REFINE_DEFAULT_FEATHER_PIXELS,
    MATTE_REFINE_DEFAULT_MORE_MOTION_BLUR, MATTE_REFINE_DEFAULT_OTHER_EDGE_RADIUS_PIXELS,
    MATTE_REFINE_DEFAULT_PER_FRAME_SAMPLES, MATTE_REFINE_DEFAULT_SHIFT_EDGE_PIXELS,
    MATTE_REFINE_DEFAULT_SHUTTER_ANGLE_DEGREES, MATTE_REFINE_DEFAULT_SMOOTH_PIXELS,
    MATTE_REFINE_DEFAULT_TREMOR_AMOUNT, MATTE_REFINE_DEFAULT_TREMOR_REDUCTION,
    MATTE_REFINE_DEFAULT_VIEW_EDGE_REGION, MATTE_REFINE_DEFAULT_VIEW_MAP,
};
pub use median::{
    median_frame_f32, MedianError, MedianGrade, MEDIAN_BIN_COUNT, MEDIAN_COARSE_BINS,
    MEDIAN_DEFAULT_OPERATE_ON_ALPHA, MEDIAN_DEFAULT_RADIUS, MEDIAN_GPU_MAX_RADIUS,
    MEDIAN_MAX_RADIUS,
};
pub use minimax::{
    apply_minimax_f32_controlled, minimax_frame_f32, MinimaxChannel, MinimaxDirection,
    MinimaxError, MinimaxGrade, MinimaxOperation, MinimaxPass, MINIMAX_CHANNEL_COUNT,
    MINIMAX_DEFAULT_DONT_SHRINK_EDGES, MINIMAX_DEFAULT_RADIUS, MINIMAX_MAX_RADIUS,
};
pub use ripple::{
    ripple_frame_f32, ripple_phase_turns, validate_wave_speed, RippleError, RippleGrade,
    RippleType, RIPPLE_MAX_CENTRE_PIXELS, RIPPLE_MAX_PHASE_TURNS, RIPPLE_MAX_RADIUS_PERCENT,
    RIPPLE_MAX_WAVE_HEIGHT, RIPPLE_MAX_WAVE_SPEED, RIPPLE_MAX_WAVE_WIDTH,
    RIPPLE_MIN_RADIUS_PERCENT, RIPPLE_MIN_WAVE_WIDTH, RIPPLE_TYPE_COUNT,
};
pub use roughen_edges::{
    roughen_edges_frame_f32, roughen_edges_halo_pixels, RoughenEdgesError, RoughenEdgesGrade,
    RoughenEdgesType, ROUGHEN_EDGES_FEATURE_PIXELS, ROUGHEN_EDGES_FIELD_SALT,
    ROUGHEN_EDGES_GRADIENT_EPSILON, ROUGHEN_EDGES_MAX_BORDER, ROUGHEN_EDGES_MAX_COMPLEXITY,
    ROUGHEN_EDGES_MAX_CYCLE, ROUGHEN_EDGES_MAX_EVOLUTION_DEGREES, ROUGHEN_EDGES_MAX_INFLUENCE,
    ROUGHEN_EDGES_MAX_OFFSET_PIXELS, ROUGHEN_EDGES_MAX_RANDOM_SEED, ROUGHEN_EDGES_MAX_SCALE,
    ROUGHEN_EDGES_MAX_SHARPNESS, ROUGHEN_EDGES_MAX_STRETCH, ROUGHEN_EDGES_MIN_BORDER,
    ROUGHEN_EDGES_MIN_COMPLEXITY, ROUGHEN_EDGES_MIN_CYCLE, ROUGHEN_EDGES_MIN_INFLUENCE,
    ROUGHEN_EDGES_MIN_SAMPLE_ALPHA, ROUGHEN_EDGES_MIN_SCALE, ROUGHEN_EDGES_MIN_SHARPNESS,
    ROUGHEN_EDGES_MIN_STRETCH, ROUGHEN_EDGES_MIN_STRETCH_FACTOR, ROUGHEN_EDGES_RUST_SALT,
};
pub use shift_channels::{ChannelSource, ShiftChannelsGrade};
pub use test_signals::{
    test_signals_frame_f32, TestSignalsError, TestSignalsGrade, TestSignalsPattern,
    TEST_SIGNALS_DEFAULT_INTENSITY, TEST_SIGNALS_MAX_INTENSITY, TEST_SIGNALS_MIN_INTENSITY,
    TEST_SIGNALS_PATTERN_COUNT,
};
pub use wave_warp::{
    validate_wave_speed as validate_wave_warp_speed, wave_warp_frame_f32, wave_warp_phase_turns,
    WavePin, WaveQuality, WaveType, WaveWarpError, WaveWarpGrade, WAVE_WARP_CENTRE_PIN_FRACTION,
    WAVE_WARP_MAX_HEIGHT, WAVE_WARP_MAX_PHASE_DEGREES, WAVE_WARP_MAX_SPEED, WAVE_WARP_MAX_WIDTH,
    WAVE_WARP_MIN_HEIGHT, WAVE_WARP_MIN_WIDTH, WAVE_WARP_PIN_COUNT, WAVE_WARP_PIN_MARGIN,
    WAVE_WARP_QUALITY_COUNT, WAVE_WARP_TYPE_COUNT,
};
pub use wire_removal::{
    wire_removal_frame_f32, RemovalStyle, WireRemovalError, WireRemovalGrade,
    WIRE_REMOVAL_MAX_COORDINATE, WIRE_REMOVAL_MAX_FRAME_OFFSET, WIRE_REMOVAL_MAX_THICKNESS,
};
