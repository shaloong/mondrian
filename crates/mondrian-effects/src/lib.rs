//! # mondrian-effects
//!
//! 高级效果系统：LUT 调色 / 滤镜 / 转场 / 文字动画 / 蒙版

pub mod adjustment;
pub mod effect;
pub mod execution;
pub mod graph;
pub mod lut;
pub mod mask;
pub mod mask_raster;
pub mod plugin_contract;
pub mod plugin_sdk;
pub mod text;
pub mod transition;

pub use adjustment::{blend_rgba_pixel, blend_rgba_pixel_seeded};
pub use effect::{
    build_effect_render_graph, compile_clip_effect_graph, effect_category_tree, effect_definition,
    effect_display_name, effect_library_types, register_effect_definition, EffectCacheKeyBuilder,
    EffectCachePolicy, EffectCapabilities, EffectCategoryNode, EffectDefinition, EffectEvalContext,
    EffectGraphBuilder, EffectNode, EffectNodeExt, EffectRenderOp, EffectRenderParamsBuilder,
    EffectRenderPlan, EffectType,
};
pub use execution::{
    apply_compiled_effect_graph, apply_compiled_effect_graph_pass,
    apply_compiled_effect_graph_pass_rgba_f32, apply_compiled_effect_graph_rgba_f32,
    apply_compiled_effect_graph_with_gpu, apply_effect_render_graph,
    apply_effect_render_graph_pass, apply_effect_render_plan, apply_effect_render_plan_pass,
    compiled_effect_graph_supports_rgba_f32, register_custom_render_processor,
    set_global_gpu_executor, CustomEffectRenderProcessor, EffectFloatExecutionError,
    EffectFloatUnsupportedReason, EffectGpuExecutor,
};
pub use graph::{
    compile_effect_render_graph, compile_scheduled_effect_graph,
    get_or_compile_scheduled_effect_graph, get_or_compile_scheduled_render_graph,
    schedule_effect_render_graph, CompiledEffectGraph, EffectExecutionSchedule,
    EffectGraphBuilderState, EffectGraphNode, EffectGraphNodeId, EffectGraphNodeKind,
    EffectGraphValue, EffectRenderGraph,
};
pub use lut::{Lut3D, LutCache, LutLibrary, LutLibraryEntry};
pub use mask::{BezierPoint, MaskComponent, MaskId, MaskKeyframe, MaskOp, MaskShape};
pub use mask_raster::rasterize_mask_shape;
pub use plugin_contract::{
    effect_plugin_is_library_visible, effect_plugin_is_runtime_available,
    effect_plugin_runtime_status, plugin_contract, record_plugin_runtime_failure,
    register_plugin_contract, EffectPluginApiVersion, EffectPluginContract,
    EffectPluginDegradationPolicy, EffectPluginFailurePolicy, EffectPluginRuntimeStatus,
    CURRENT_EFFECT_PLUGIN_API_VERSION,
};
pub use plugin_sdk::{EffectGraphDsl, EffectPluginDefinitionBuilder};
pub use transition::Transition;
