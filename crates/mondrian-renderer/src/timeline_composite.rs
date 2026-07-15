use crate::CpuColorFrame;
use mondrian_core::{
    types::{BlendMode, Color},
    ColorEngine, OcioColorSpaceIdentity, WorkingColorSpace, WorkingRgbaF32Frame,
};
use mondrian_effects::{
    apply_compiled_effect_graph, apply_compiled_effect_graph_pass,
    apply_compiled_effect_graph_pass_rgba_f32,
    apply_compiled_effect_graph_pass_rgba_f32_with_domain_processor,
    apply_compiled_effect_graph_rgba_f32,
    apply_compiled_effect_graph_rgba_f32_with_domain_processor, blend_rgba_f32_pixel_seeded,
    blend_rgba_pixel_seeded, compiled_effect_graph_supports_rgba_f32_with_domain_processor,
    CompiledEffectGraph, EffectColorDomain, EffectDomainTransition, EffectFloatExecutionError,
};
use serde::{Deserialize, Serialize};
use std::{
    hash::{Hash, Hasher},
    sync::Arc,
};

#[derive(Debug, Clone)]
pub struct TimelineMediaLayer<'a> {
    pub frame: &'a CpuColorFrame,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub transform: [f32; 6],
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub struct TimelineAdjustmentLayer {
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub opacity: f32,
    pub blend_mode: Option<BlendMode>,
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub struct TimelineSolidColorLayer {
    pub color: Color,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub transform: [f32; 6],
    pub effect_graph: Arc<CompiledEffectGraph>,
    pub frame_seed: i64,
}

#[derive(Debug, Clone)]
pub enum TimelineCompositeElement<'a> {
    Media(TimelineMediaLayer<'a>),
    Adjustment(TimelineAdjustmentLayer),
    SolidColor(TimelineSolidColorLayer),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TimelineCompositeOptions {
    pub empty_canvas_transparent: bool,
}

/// Renderer-owned runtime for resolving effect RGB domains through stock OCIO.
#[derive(Debug, Clone, Copy)]
pub struct TimelineEffectColorRuntime<'a> {
    /// Exact project color engine/config provider.
    pub engine: &'a ColorEngine,
    /// Sequence working space represented by `SceneLinearRgb`.
    pub working_color_space: WorkingColorSpace,
}

impl<'a> TimelineEffectColorRuntime<'a> {
    /// Bind a project color engine to one sequence working space.
    pub const fn new(engine: &'a ColorEngine, working_color_space: WorkingColorSpace) -> Self {
        Self { engine, working_color_space }
    }

    fn cache_key(self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.engine.hash(&mut hasher);
        self.working_color_space.hash(&mut hasher);
        mondrian_core::ocio::ocio_config_generation().hash(&mut hasher);
        hasher.finish()
    }

    fn process_transition(
        self,
        pixels: &mut [[f32; 4]],
        transition: EffectDomainTransition,
    ) -> Result<(), String> {
        let src = self.identity(transition.from)?;
        let dst = self.identity(transition.to)?;
        self.engine.convert_identity_float(pixels.as_flattened_mut(), src, dst)
    }

    fn identity(self, domain: EffectColorDomain) -> Result<OcioColorSpaceIdentity, String> {
        match domain {
            EffectColorDomain::SceneLinearRgb => Ok(self.working_color_space.into()),
            EffectColorDomain::LogPerceptualRgb { color_space }
            | EffectColorDomain::DisplayLinearRgb { color_space }
            | EffectColorDomain::DisplayEncodedRgb { color_space } => Ok(color_space.into()),
            EffectColorDomain::Data | EffectColorDomain::AlphaMask => {
                Err(format!("{domain:?} is not a color-managed RGB domain"))
            }
        }
    }
}

#[derive(Default)]
pub struct TimelineCompositeScratch {
    media_source: Vec<u8>,
    media_effect: Vec<u8>,
    adjustment: Vec<u8>,
    solid_fill: Vec<u8>,
    solid_fill_f32: Vec<[f32; 4]>,
    solid_effect_f32: Vec<[f32; 4]>,
}

/// A CPU composite result paired with color-path diagnostics for the plan.
#[derive(Debug, Clone)]
pub struct TimelineCompositeFrame {
    /// The composited frame in the requested working color context.
    pub frame: CpuColorFrame,
    /// Per-plan diagnostics describing whether compositing stayed float/linear
    /// or fell back to the legacy RGBA8 path.
    pub diagnostics: TimelineCompositeDiagnostics,
}

/// Counters describing which timeline composite path was used and why.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCompositeDiagnostics {
    /// Number of timeline elements evaluated for the composite plan.
    pub elements: u64,
    /// Composite plans that stayed on the float/linear path.
    pub float_linear_composites: u64,
    /// Composite plans that fell back to the legacy RGBA8 path.
    pub legacy_rgba8_composites: u64,
    /// Media layers that required legacy RGBA8 because of blend mode support.
    pub legacy_media_blend_mode: u64,
    /// Media layers that required legacy RGBA8 because of transform support.
    pub legacy_media_transform: u64,
    /// Media layers that required legacy RGBA8 because of effect graph support.
    pub legacy_media_effect: u64,
    /// Solid layers that required legacy RGBA8 because of blend mode support.
    pub legacy_solid_blend_mode: u64,
    /// Solid layers that required legacy RGBA8 because of transform support.
    pub legacy_solid_transform: u64,
    /// Solid layers that required legacy RGBA8 because of effect graph support.
    pub legacy_solid_effect: u64,
    /// Adjustment layers that required legacy RGBA8 because of blend mode support.
    pub legacy_adjustment_blend_mode: u64,
    /// Adjustment layers that required legacy RGBA8 because of effect graph support.
    pub legacy_adjustment_effect: u64,
    /// Composite plans blocked because an effect-domain transition was not resolved.
    pub blocked_color_domain_composites: u64,
    /// Media effects with unresolved or invalid color-domain edges.
    pub blocked_media_effect_domain: u64,
    /// Solid effects with unresolved or invalid color-domain edges.
    pub blocked_solid_effect_domain: u64,
    /// Adjustment effects with unresolved or invalid color-domain edges.
    pub blocked_adjustment_effect_domain: u64,
    /// Effect nodes that could not execute on GPU (structured blockers).
    pub effect_gpu_blockers: u64,
    /// Effect nodes that executed on GPU successfully.
    pub effect_gpu_executed: u64,
}

/// High-level compositing color path selected by timeline compositing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineCompositeColorPath {
    /// Every composite plan stayed in the float/linear working path.
    #[default]
    FloatLinear,
    /// At least one composite plan required the legacy RGBA8 path.
    LegacyRgba8,
    /// Compositing failed closed because an effect-domain contract was unresolved.
    Blocked,
}

/// Structured effect-domain blockers for one or more composite plans.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCompositeDomainBlockerBreakdown {
    /// Media effects with unresolved domain transitions or invalid domain edges.
    pub media_effect: u64,
    /// Solid effects with unresolved domain transitions or invalid domain edges.
    pub solid_effect: u64,
    /// Adjustment effects with unresolved domain transitions or invalid domain edges.
    pub adjustment_effect: u64,
}

impl TimelineCompositeDomainBlockerBreakdown {
    /// Total number of blocked effect-domain inputs.
    pub fn total(self) -> u64 {
        self.media_effect
            .saturating_add(self.solid_effect)
            .saturating_add(self.adjustment_effect)
    }

    /// Whether no effect-domain blocker was recorded.
    pub fn is_empty(self) -> bool {
        self.total() == 0
    }
}

/// Structured reasons a composite plan required the legacy RGBA8 path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCompositeLegacyBreakdown {
    /// Media layers that required legacy RGBA8 because of blend mode support.
    pub media_blend_mode: u64,
    /// Media layers that required legacy RGBA8 because of transform support.
    pub media_transform: u64,
    /// Media layers that required legacy RGBA8 because of effect graph support.
    pub media_effect: u64,
    /// Solid layers that required legacy RGBA8 because of blend mode support.
    pub solid_blend_mode: u64,
    /// Solid layers that required legacy RGBA8 because of transform support.
    pub solid_transform: u64,
    /// Solid layers that required legacy RGBA8 because of effect graph support.
    pub solid_effect: u64,
    /// Adjustment layers that required legacy RGBA8 because of blend mode support.
    pub adjustment_blend_mode: u64,
    /// Adjustment layers that required legacy RGBA8 because of effect graph support.
    pub adjustment_effect: u64,
}

impl TimelineCompositeLegacyBreakdown {
    /// Total number of recorded legacy RGBA8 causes.
    pub fn total(self) -> u64 {
        self.media_blend_mode
            .saturating_add(self.media_transform)
            .saturating_add(self.media_effect)
            .saturating_add(self.solid_blend_mode)
            .saturating_add(self.solid_transform)
            .saturating_add(self.solid_effect)
            .saturating_add(self.adjustment_blend_mode)
            .saturating_add(self.adjustment_effect)
    }

    /// Returns true when no legacy RGBA8 causes were recorded.
    pub fn is_empty(self) -> bool {
        self.total() == 0
    }
}

/// Renderer-owned summary of composite color-path safety for a diagnostics snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCompositeColorPathSummary {
    /// Selected color path after folding all composite diagnostics.
    pub path: TimelineCompositeColorPath,
    /// Number of timeline elements evaluated by the composite plans.
    pub elements: u64,
    /// Composite plans that stayed on the float/linear path.
    pub float_linear_composites: u64,
    /// Composite plans that fell back to the legacy RGBA8 path.
    pub legacy_rgba8_composites: u64,
    /// Structured reasons for any legacy RGBA8 fallback.
    pub legacy_breakdown: TimelineCompositeLegacyBreakdown,
    /// Composite plans that failed closed on unresolved effect-domain semantics.
    pub blocked_composites: u64,
    /// Structured effect-domain blocker counts.
    pub domain_blockers: TimelineCompositeDomainBlockerBreakdown,
}

impl TimelineCompositeColorPathSummary {
    /// Number of composite plans represented by this summary.
    pub fn composite_plans(self) -> u64 {
        self.float_linear_composites
            .saturating_add(self.legacy_rgba8_composites)
            .saturating_add(self.blocked_composites)
    }

    /// Returns true when at least one composite plan used or required legacy RGBA8.
    pub fn uses_legacy_rgba8(self) -> bool {
        matches!(self.path, TimelineCompositeColorPath::LegacyRgba8)
    }

    /// Returns true when all represented plans stayed on the float/linear path.
    pub fn is_fully_float_linear(self) -> bool {
        self.composite_plans() > 0
            && !self.uses_legacy_rgba8()
            && self.blocked_composites == 0
            && self.float_linear_composites == self.composite_plans()
    }
}

impl TimelineCompositeDiagnostics {
    /// Merge another diagnostic snapshot into this one using saturating counters.
    pub fn accumulate(&mut self, other: Self) {
        self.elements = self.elements.saturating_add(other.elements);
        self.float_linear_composites =
            self.float_linear_composites.saturating_add(other.float_linear_composites);
        self.legacy_rgba8_composites =
            self.legacy_rgba8_composites.saturating_add(other.legacy_rgba8_composites);
        self.legacy_media_blend_mode =
            self.legacy_media_blend_mode.saturating_add(other.legacy_media_blend_mode);
        self.legacy_media_transform =
            self.legacy_media_transform.saturating_add(other.legacy_media_transform);
        self.legacy_media_effect =
            self.legacy_media_effect.saturating_add(other.legacy_media_effect);
        self.legacy_solid_blend_mode =
            self.legacy_solid_blend_mode.saturating_add(other.legacy_solid_blend_mode);
        self.legacy_solid_transform =
            self.legacy_solid_transform.saturating_add(other.legacy_solid_transform);
        self.legacy_solid_effect =
            self.legacy_solid_effect.saturating_add(other.legacy_solid_effect);
        self.legacy_adjustment_blend_mode = self
            .legacy_adjustment_blend_mode
            .saturating_add(other.legacy_adjustment_blend_mode);
        self.legacy_adjustment_effect =
            self.legacy_adjustment_effect.saturating_add(other.legacy_adjustment_effect);
        self.blocked_color_domain_composites = self
            .blocked_color_domain_composites
            .saturating_add(other.blocked_color_domain_composites);
        self.blocked_media_effect_domain = self
            .blocked_media_effect_domain
            .saturating_add(other.blocked_media_effect_domain);
        self.blocked_solid_effect_domain = self
            .blocked_solid_effect_domain
            .saturating_add(other.blocked_solid_effect_domain);
        self.blocked_adjustment_effect_domain = self
            .blocked_adjustment_effect_domain
            .saturating_add(other.blocked_adjustment_effect_domain);
        self.effect_gpu_blockers =
            self.effect_gpu_blockers.saturating_add(other.effect_gpu_blockers);
        self.effect_gpu_executed =
            self.effect_gpu_executed.saturating_add(other.effect_gpu_executed);
    }

    /// Returns true when the composite plan used any legacy RGBA8 fallback.
    pub fn uses_legacy_rgba8(self) -> bool {
        self.color_path_summary().uses_legacy_rgba8()
    }

    /// Return structured legacy RGBA8 fallback reasons.
    pub fn legacy_breakdown(self) -> TimelineCompositeLegacyBreakdown {
        TimelineCompositeLegacyBreakdown {
            media_blend_mode: self.legacy_media_blend_mode,
            media_transform: self.legacy_media_transform,
            media_effect: self.legacy_media_effect,
            solid_blend_mode: self.legacy_solid_blend_mode,
            solid_transform: self.legacy_solid_transform,
            solid_effect: self.legacy_solid_effect,
            adjustment_blend_mode: self.legacy_adjustment_blend_mode,
            adjustment_effect: self.legacy_adjustment_effect,
        }
    }

    /// Return structured unresolved effect-domain reasons.
    pub fn domain_blockers(self) -> TimelineCompositeDomainBlockerBreakdown {
        TimelineCompositeDomainBlockerBreakdown {
            media_effect: self.blocked_media_effect_domain,
            solid_effect: self.blocked_solid_effect_domain,
            adjustment_effect: self.blocked_adjustment_effect_domain,
        }
    }

    /// Whether this plan failed closed on effect-domain semantics.
    pub fn is_color_domain_blocked(self) -> bool {
        self.blocked_color_domain_composites > 0 || !self.domain_blockers().is_empty()
    }

    /// Return the high-level composite color path for this diagnostics snapshot.
    pub fn color_path(self) -> TimelineCompositeColorPath {
        if self.is_color_domain_blocked() {
            TimelineCompositeColorPath::Blocked
        } else if self.legacy_rgba8_composites > 0 || !self.legacy_breakdown().is_empty() {
            TimelineCompositeColorPath::LegacyRgba8
        } else {
            TimelineCompositeColorPath::FloatLinear
        }
    }

    /// Return a renderer-owned summary that callers can use for reports and budgets.
    pub fn color_path_summary(self) -> TimelineCompositeColorPathSummary {
        TimelineCompositeColorPathSummary {
            path: self.color_path(),
            elements: self.elements,
            float_linear_composites: self.float_linear_composites,
            legacy_rgba8_composites: self.legacy_rgba8_composites,
            legacy_breakdown: self.legacy_breakdown(),
            blocked_composites: self.blocked_color_domain_composites,
            domain_blockers: self.domain_blockers(),
        }
    }
}

/// Composite timeline elements through the diagnosed legacy RGBA8 boundary.
///
/// Unresolved effect-domain contracts return a structured error and are never
/// bypassed or evaluated as encoded pixels.
pub fn composite_timeline_elements(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    scratch: &mut TimelineCompositeScratch,
) -> Result<Vec<u8>, mondrian_effects::EffectExecutionError> {
    let mut out = Vec::new();
    composite_timeline_elements_into(&mut out, width, height, elements, options, scratch)?;
    Ok(out)
}

/// Composite timeline elements into a typed color-managed working frame.
pub fn composite_timeline_elements_color_frame(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    runtime: TimelineEffectColorRuntime<'_>,
    scratch: &mut TimelineCompositeScratch,
) -> CpuColorFrame {
    composite_timeline_elements_color_frame_with_diagnostics(
        width, height, elements, options, runtime, scratch,
    )
    .frame
}

/// Composite timeline elements and return both the working frame and
/// diagnostics for the selected color path.
pub fn composite_timeline_elements_color_frame_with_diagnostics(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    runtime: TimelineEffectColorRuntime<'_>,
    scratch: &mut TimelineCompositeScratch,
) -> TimelineCompositeFrame {
    let mut diagnostics = composite_path_diagnostics(elements);
    let frame = if diagnostics.is_color_domain_blocked() {
        blocked_working_frame(width, height, runtime.working_color_space)
    } else if diagnostics.uses_legacy_rgba8() {
        let rgba = composite_timeline_elements(width, height, elements, options, scratch)
            .unwrap_or_else(|_| vec![0; width as usize * height as usize * 4]);
        working_frame_from_normalized_rgba8(width, height, &rgba, runtime.working_color_space)
    } else {
        match composite_supported_elements_to_working_frame(
            width,
            height,
            elements,
            options,
            runtime.working_color_space,
            runtime,
            scratch,
        ) {
            Ok(frame) => frame,
            Err(_) => {
                mark_effect_domain_execution_blocked(elements, &mut diagnostics);
                blocked_working_frame(width, height, runtime.working_color_space)
            }
        }
    };
    TimelineCompositeFrame { frame: CpuColorFrame::working(frame), diagnostics }
}

fn blocked_working_frame(
    width: u32,
    height: u32,
    working_color_space: WorkingColorSpace,
) -> WorkingRgbaF32Frame {
    WorkingRgbaF32Frame {
        width,
        height,
        data: vec![[0.0, 0.0, 0.0, 1.0]; width as usize * height as usize],
        color_space: working_color_space,
    }
}

fn composite_supported_elements_to_working_frame(
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    working_color_space: WorkingColorSpace,
    runtime: TimelineEffectColorRuntime<'_>,
    scratch: &mut TimelineCompositeScratch,
) -> Result<WorkingRgbaF32Frame, EffectFloatExecutionError> {
    let pixel_count = width as usize * height as usize;
    if pixel_count == 0 {
        return Ok(WorkingRgbaF32Frame {
            width,
            height,
            data: Vec::new(),
            color_space: working_color_space,
        });
    }

    let mut canvas = vec![[0.0, 0.0, 0.0, 1.0]; pixel_count];
    let mut has_composited_layer = false;

    for element in elements {
        match element {
            TimelineCompositeElement::Media(layer) => {
                let frame = layer.frame.rgba_f32();
                let effect_output;
                let (src_data, src_width, src_height) = if layer.effect_graph.graph.is_identity() {
                    (&frame.data, frame.width, frame.height)
                } else {
                    effect_output = apply_effect_graph_f32(
                        &frame.data,
                        frame.width,
                        frame.height,
                        &layer.effect_graph,
                        layer.frame_seed,
                        runtime,
                    )?;
                    (&effect_output, frame.width, frame.height)
                };
                alpha_blend_f32_layer(
                    &mut canvas,
                    width as usize,
                    height as usize,
                    src_data,
                    src_width as usize,
                    src_height as usize,
                    layer.opacity,
                    layer.blend_mode,
                    layer.transform,
                    layer.frame_seed,
                );
                has_composited_layer = true;
            }
            TimelineCompositeElement::SolidColor(layer) => {
                let color = [layer.color.r, layer.color.g, layer.color.b, layer.color.a];
                if layer.effect_graph.graph.is_identity() && is_identity_transform(layer.transform)
                {
                    alpha_blend_f32_solid(
                        &mut canvas,
                        color,
                        layer.opacity,
                        layer.blend_mode,
                        layer.frame_seed,
                    );
                } else {
                    scratch.solid_fill_f32.resize(pixel_count, color);
                    scratch.solid_fill_f32.fill(color);
                    let source = if layer.effect_graph.graph.is_identity() {
                        scratch.solid_fill_f32.as_slice()
                    } else {
                        scratch.solid_effect_f32 = apply_effect_graph_f32(
                            &scratch.solid_fill_f32,
                            width,
                            height,
                            &layer.effect_graph,
                            layer.frame_seed,
                            runtime,
                        )?;
                        scratch.solid_effect_f32.as_slice()
                    };
                    alpha_blend_f32_layer(
                        &mut canvas,
                        width as usize,
                        height as usize,
                        source,
                        width as usize,
                        height as usize,
                        layer.opacity,
                        layer.blend_mode,
                        layer.transform,
                        layer.frame_seed,
                    );
                }
                has_composited_layer = true;
            }
            TimelineCompositeElement::Adjustment(layer) => {
                if !has_composited_layer
                    || layer.opacity <= 1.0e-4
                    || layer.effect_graph.graph.is_identity()
                {
                    continue;
                }
                canvas = apply_effect_graph_pass_f32(
                    &canvas,
                    width,
                    height,
                    &layer.effect_graph,
                    layer.opacity,
                    layer.blend_mode,
                    layer.frame_seed,
                    runtime,
                )?;
            }
        }
    }

    if !has_composited_layer && options.empty_canvas_transparent {
        canvas.fill([0.0, 0.0, 0.0, 0.0]);
    }

    Ok(WorkingRgbaF32Frame {
        width,
        height,
        data: canvas,
        color_space: working_color_space,
    })
}

fn apply_effect_graph_f32(
    input: &[[f32; 4]],
    width: u32,
    height: u32,
    graph: &CompiledEffectGraph,
    frame_seed: i64,
    runtime: TimelineEffectColorRuntime<'_>,
) -> Result<Vec<[f32; 4]>, EffectFloatExecutionError> {
    if graph.domain_plan.requires_conversion() {
        apply_compiled_effect_graph_rgba_f32_with_domain_processor(
            input,
            width,
            height,
            graph,
            frame_seed,
            runtime.cache_key(),
            |pixels, transition| runtime.process_transition(pixels, transition),
        )
    } else {
        apply_compiled_effect_graph_rgba_f32(input, width, height, graph, frame_seed)
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_effect_graph_pass_f32(
    input: &[[f32; 4]],
    width: u32,
    height: u32,
    graph: &CompiledEffectGraph,
    opacity: f32,
    blend_mode: Option<BlendMode>,
    frame_seed: i64,
    runtime: TimelineEffectColorRuntime<'_>,
) -> Result<Vec<[f32; 4]>, EffectFloatExecutionError> {
    if graph.domain_plan.requires_conversion() {
        apply_compiled_effect_graph_pass_rgba_f32_with_domain_processor(
            input,
            width,
            height,
            graph,
            opacity,
            blend_mode,
            frame_seed,
            runtime.cache_key(),
            |pixels, transition| runtime.process_transition(pixels, transition),
        )
    } else {
        apply_compiled_effect_graph_pass_rgba_f32(
            input, width, height, graph, opacity, blend_mode, frame_seed,
        )
    }
}

/// Diagnose whether a set of timeline elements can stay on the float/linear
/// compositor path, or which capabilities force legacy RGBA8 fallback.
pub fn composite_path_diagnostics(
    elements: &[TimelineCompositeElement<'_>],
) -> TimelineCompositeDiagnostics {
    let mut diagnostics = TimelineCompositeDiagnostics {
        elements: elements.len() as u64,
        ..TimelineCompositeDiagnostics::default()
    };
    for element in elements {
        match element {
            TimelineCompositeElement::Media(layer) => {
                if effect_domain_is_blocked(&layer.effect_graph) {
                    diagnostics.blocked_media_effect_domain =
                        diagnostics.blocked_media_effect_domain.saturating_add(1);
                } else if !compiled_effect_graph_supports_rgba_f32_with_domain_processor(
                    &layer.effect_graph,
                ) {
                    diagnostics.legacy_media_effect =
                        diagnostics.legacy_media_effect.saturating_add(1);
                }
                diagnostics.effect_gpu_blockers =
                    diagnostics.effect_gpu_blockers.saturating_add(u64::from(
                        mondrian_effects::get_or_lower_effect_graph_to_gpu_plan(
                            &layer.effect_graph,
                        )
                        .is_err(),
                    ));
            }
            TimelineCompositeElement::SolidColor(layer) => {
                if effect_domain_is_blocked(&layer.effect_graph) {
                    diagnostics.blocked_solid_effect_domain =
                        diagnostics.blocked_solid_effect_domain.saturating_add(1);
                } else if !compiled_effect_graph_supports_rgba_f32_with_domain_processor(
                    &layer.effect_graph,
                ) {
                    diagnostics.legacy_solid_effect =
                        diagnostics.legacy_solid_effect.saturating_add(1);
                }
                diagnostics.effect_gpu_blockers =
                    diagnostics.effect_gpu_blockers.saturating_add(u64::from(
                        mondrian_effects::get_or_lower_effect_graph_to_gpu_plan(
                            &layer.effect_graph,
                        )
                        .is_err(),
                    ));
            }
            TimelineCompositeElement::Adjustment(layer) => {
                if effect_domain_is_blocked(&layer.effect_graph) {
                    diagnostics.blocked_adjustment_effect_domain =
                        diagnostics.blocked_adjustment_effect_domain.saturating_add(1);
                } else if !compiled_effect_graph_supports_rgba_f32_with_domain_processor(
                    &layer.effect_graph,
                ) {
                    diagnostics.legacy_adjustment_effect =
                        diagnostics.legacy_adjustment_effect.saturating_add(1);
                }
                diagnostics.effect_gpu_blockers =
                    diagnostics.effect_gpu_blockers.saturating_add(u64::from(
                        mondrian_effects::get_or_lower_effect_graph_to_gpu_plan(
                            &layer.effect_graph,
                        )
                        .is_err(),
                    ));
            }
        }
    }
    let legacy_reasons = diagnostics.legacy_media_blend_mode
        + diagnostics.legacy_media_transform
        + diagnostics.legacy_media_effect
        + diagnostics.legacy_solid_blend_mode
        + diagnostics.legacy_solid_transform
        + diagnostics.legacy_solid_effect
        + diagnostics.legacy_adjustment_blend_mode
        + diagnostics.legacy_adjustment_effect;
    let blocked_reasons = diagnostics
        .blocked_media_effect_domain
        .saturating_add(diagnostics.blocked_solid_effect_domain)
        .saturating_add(diagnostics.blocked_adjustment_effect_domain);
    if blocked_reasons > 0 {
        diagnostics.blocked_color_domain_composites = 1;
    } else if legacy_reasons == 0 {
        diagnostics.float_linear_composites = 1;
    } else {
        diagnostics.legacy_rgba8_composites = 1;
    }
    diagnostics
}

fn effect_domain_is_blocked(graph: &CompiledEffectGraph) -> bool {
    !graph.domain_plan.blockers.is_empty()
        || (graph.domain_plan.requires_conversion()
            && !compiled_effect_graph_supports_rgba_f32_with_domain_processor(graph))
}

fn mark_effect_domain_execution_blocked(
    elements: &[TimelineCompositeElement<'_>],
    diagnostics: &mut TimelineCompositeDiagnostics,
) {
    diagnostics.float_linear_composites = 0;
    diagnostics.legacy_rgba8_composites = 0;
    diagnostics.blocked_color_domain_composites = 1;
    for element in elements {
        match element {
            TimelineCompositeElement::Media(layer)
                if layer.effect_graph.domain_plan.requires_conversion() =>
            {
                diagnostics.blocked_media_effect_domain =
                    diagnostics.blocked_media_effect_domain.saturating_add(1);
            }
            TimelineCompositeElement::SolidColor(layer)
                if layer.effect_graph.domain_plan.requires_conversion() =>
            {
                diagnostics.blocked_solid_effect_domain =
                    diagnostics.blocked_solid_effect_domain.saturating_add(1);
            }
            TimelineCompositeElement::Adjustment(layer)
                if layer.effect_graph.domain_plan.requires_conversion() =>
            {
                diagnostics.blocked_adjustment_effect_domain =
                    diagnostics.blocked_adjustment_effect_domain.saturating_add(1);
            }
            _ => {}
        }
    }
}

fn alpha_blend_f32_solid(
    dst: &mut [[f32; 4]],
    color: [f32; 4],
    opacity: f32,
    blend_mode: BlendMode,
    frame_seed: i64,
) {
    let src_a = (color[3] * opacity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
    if src_a <= 1.0e-4 {
        return;
    }
    for (index, dst_px) in dst.iter_mut().enumerate() {
        *dst_px = blend_rgba_f32_pixel_seeded(
            *dst_px,
            color,
            opacity,
            blend_mode,
            dither_seed(index as u32, frame_seed),
        );
    }
}

fn alpha_blend_f32_layer(
    dst: &mut [[f32; 4]],
    dst_w: usize,
    dst_h: usize,
    src: &[[f32; 4]],
    src_w: usize,
    src_h: usize,
    opacity: f32,
    blend_mode: BlendMode,
    transform: [f32; 6],
    frame_seed: i64,
) {
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 1.0e-4 {
        return;
    }

    if is_identity_transform(transform) {
        let width = dst_w.min(src_w);
        let height = dst_h.min(src_h);
        for y in 0..height {
            for x in 0..width {
                let dst_px = &mut dst[y * dst_w + x];
                let src_px = src[y * src_w + x];
                *dst_px = blend_rgba_f32_pixel_seeded(
                    *dst_px,
                    src_px,
                    opacity,
                    blend_mode,
                    dither_seed((y * dst_w + x) as u32, frame_seed),
                );
            }
        }
        return;
    }

    let Some(inv) = invert_affine(transform) else {
        return;
    };

    for dy in 0..dst_h {
        for dx in 0..dst_w {
            let fx = dx as f32 + 0.5;
            let fy = dy as f32 + 0.5;
            let sx = inv[0] * fx + inv[1] * fy + inv[2];
            let sy = inv[3] * fx + inv[4] * fy + inv[5];
            let Some(src_px) = sample_src_f32_bilinear(src, src_w, src_h, sx - 0.5, sy - 0.5)
            else {
                continue;
            };
            let dst_idx = dy * dst_w + dx;
            dst[dst_idx] = blend_rgba_f32_pixel_seeded(
                dst[dst_idx],
                src_px,
                opacity,
                blend_mode,
                dither_seed((dy * dst_w + dx) as u32, frame_seed),
            );
        }
    }
}

fn sample_src_f32_bilinear(
    src: &[[f32; 4]],
    src_w: usize,
    src_h: usize,
    sx: f32,
    sy: f32,
) -> Option<[f32; 4]> {
    let x0 = sx.floor() as isize;
    let y0 = sy.floor() as isize;
    let x1 = x0 + 1;
    let y1 = y0 + 1;

    if x0 < 0 || y0 < 0 || x1 >= src_w as isize || y1 >= src_h as isize {
        if x0 >= 0 && y0 >= 0 && x0 < src_w as isize && y0 < src_h as isize {
            return Some(src[y0 as usize * src_w + x0 as usize]);
        }
        return None;
    }

    let fx = sx - x0 as f32;
    let fy = sy - y0 as f32;

    let tl = src[y0 as usize * src_w + x0 as usize];
    let tr = src[y0 as usize * src_w + x1 as usize];
    let bl = src[y1 as usize * src_w + x0 as usize];
    let br = src[y1 as usize * src_w + x1 as usize];

    let mut out = [0.0f32; 4];
    for c in 0..4 {
        let top = tl[c] + (tr[c] - tl[c]) * fx;
        let bot = bl[c] + (br[c] - bl[c]) * fx;
        out[c] = top + (bot - top) * fy;
    }
    Some(out)
}

fn dither_seed(pixel_index: u32, frame_seed: i64) -> u32 {
    pixel_index ^ (frame_seed as u32)
}

/// Composite into a caller-owned RGBA8 buffer with fail-closed effect domains.
pub fn composite_timeline_elements_into(
    out: &mut Vec<u8>,
    width: u32,
    height: u32,
    elements: &[TimelineCompositeElement<'_>],
    options: TimelineCompositeOptions,
    scratch: &mut TimelineCompositeScratch,
) -> Result<(), mondrian_effects::EffectExecutionError> {
    let required_len = width as usize * height as usize * 4;
    if out.len() != required_len {
        out.resize(required_len, 0);
    }
    if required_len == 0 {
        out.clear();
        return Ok(());
    }

    clear_canvas_black_opaque(out);
    let mut has_composited_media = false;

    for element in elements {
        match element {
            TimelineCompositeElement::Media(layer) => {
                let descriptor = layer.frame.descriptor();
                scratch.media_source = layer
                    .frame
                    .rgba_f32()
                    .data
                    .iter()
                    .flat_map(|pixel| {
                        pixel.iter().map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8)
                    })
                    .collect();
                let src_rgba = if layer.effect_graph.graph.is_identity() {
                    scratch.media_source.as_slice()
                } else {
                    scratch.media_effect = apply_compiled_effect_graph(
                        &scratch.media_source,
                        descriptor.width,
                        descriptor.height,
                        &layer.effect_graph,
                        layer.frame_seed,
                    )?;
                    scratch.media_effect.as_slice()
                };
                alpha_blend_layer(
                    out,
                    width,
                    height,
                    src_rgba,
                    descriptor.width,
                    descriptor.height,
                    layer.opacity,
                    layer.blend_mode,
                    layer.transform,
                );
                has_composited_media = true;
            }
            TimelineCompositeElement::SolidColor(layer) => {
                fill_solid_rgba(
                    &mut scratch.solid_fill,
                    width as usize,
                    height as usize,
                    layer.color,
                );
                let src_rgba = if layer.effect_graph.graph.is_identity() {
                    scratch.solid_fill.as_slice()
                } else {
                    scratch.media_effect = apply_compiled_effect_graph(
                        &scratch.solid_fill,
                        width,
                        height,
                        &layer.effect_graph,
                        layer.frame_seed,
                    )?;
                    scratch.media_effect.as_slice()
                };
                alpha_blend_layer(
                    out,
                    width,
                    height,
                    src_rgba,
                    width,
                    height,
                    layer.opacity,
                    layer.blend_mode,
                    layer.transform,
                );
                has_composited_media = true;
            }
            TimelineCompositeElement::Adjustment(layer) => {
                if !has_composited_media
                    || layer.opacity <= 1.0e-4
                    || layer.effect_graph.graph.is_identity()
                {
                    continue;
                }
                apply_compiled_effect_graph_pass(
                    out,
                    width,
                    height,
                    &layer.effect_graph,
                    layer.opacity,
                    layer.blend_mode,
                    layer.frame_seed,
                    &mut scratch.adjustment,
                )?;
                std::mem::swap(out, &mut scratch.adjustment);
            }
        }
    }

    if !has_composited_media && options.empty_canvas_transparent {
        out.fill(0);
    }
    Ok(())
}

fn working_frame_from_normalized_rgba8(
    width: u32,
    height: u32,
    rgba: &[u8],
    color_space: WorkingColorSpace,
) -> WorkingRgbaF32Frame {
    let data = rgba
        .chunks_exact(4)
        .map(|pixel| {
            [
                pixel[0] as f32 / 255.0,
                pixel[1] as f32 / 255.0,
                pixel[2] as f32 / 255.0,
                pixel[3] as f32 / 255.0,
            ]
        })
        .collect();
    WorkingRgbaF32Frame { width, height, data, color_space }
}

fn fill_solid_rgba(buf: &mut Vec<u8>, width: usize, height: usize, color: Color) {
    let r = (color.r.clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = (color.g.clamp(0.0, 1.0) * 255.0).round() as u8;
    let b = (color.b.clamp(0.0, 1.0) * 255.0).round() as u8;
    let a = (color.a.clamp(0.0, 1.0) * 255.0).round() as u8;
    let pixel = [r, g, b, a];
    buf.resize(width * height * 4, 0);
    for chunk in buf.chunks_exact_mut(4) {
        chunk.copy_from_slice(&pixel);
    }
}

pub fn is_identity_transform(transform: [f32; 6]) -> bool {
    const EPS: f32 = 1.0e-4;
    (transform[0] - 1.0).abs() <= EPS
        && transform[1].abs() <= EPS
        && transform[2].abs() <= EPS
        && transform[3].abs() <= EPS
        && (transform[4] - 1.0).abs() <= EPS
        && transform[5].abs() <= EPS
}

pub fn quantize_transform_signature(transform: [f32; 6]) -> [i32; 6] {
    const SCALE: f32 = 1024.0;
    [
        (transform[0] * SCALE).round() as i32,
        (transform[1] * SCALE).round() as i32,
        (transform[2] * SCALE).round() as i32,
        (transform[3] * SCALE).round() as i32,
        (transform[4] * SCALE).round() as i32,
        (transform[5] * SCALE).round() as i32,
    ]
}

fn clear_canvas_black_opaque(canvas: &mut [u8]) {
    canvas.fill(0);
    for px in canvas.chunks_exact_mut(4) {
        px[3] = 255;
    }
}

fn alpha_blend_layer(
    dst_rgba: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    src_rgba: &[u8],
    src_w: u32,
    src_h: u32,
    opacity: f32,
    blend_mode: BlendMode,
    transform: [f32; 6],
) {
    let width = dst_w.min(src_w) as usize;
    let height = dst_h.min(src_h) as usize;
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 1.0e-4 {
        return;
    }

    let dst_stride = dst_w as usize * 4;
    let src_stride = src_w as usize * 4;

    if is_identity_transform(transform) {
        for y in 0..height {
            let dst_row = &mut dst_rgba[y * dst_stride..(y + 1) * dst_stride];
            let src_row = &src_rgba[y * src_stride..(y + 1) * src_stride];
            for (x, (dst_px, src_px)) in
                dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)).take(width).enumerate()
            {
                let blended = blend_rgba_pixel_seeded(
                    [dst_px[0], dst_px[1], dst_px[2], dst_px[3]],
                    [src_px[0], src_px[1], src_px[2], src_px[3]],
                    opacity,
                    blend_mode,
                    (y * dst_w as usize + x) as u32,
                );
                dst_px.copy_from_slice(&blended);
            }
        }
        return;
    }

    let Some(inv) = invert_affine(transform) else {
        return;
    };

    let dst_width = dst_w as usize;
    let dst_height = dst_h as usize;
    let src_width = src_w as usize;
    let src_height = src_h as usize;

    for dy in 0..dst_height {
        for dx in 0..dst_width {
            let fx = dx as f32 + 0.5;
            let fy = dy as f32 + 0.5;
            let sx = inv[0] * fx + inv[1] * fy + inv[2];
            let sy = inv[3] * fx + inv[4] * fy + inv[5];
            let Some(src_px) = sample_src_rgba(src_rgba, src_width, src_height, sx - 0.5, sy - 0.5)
            else {
                continue;
            };
            let dst_idx = (dy * dst_width + dx) * 4;
            if dst_idx + 3 >= dst_rgba.len() {
                continue;
            }
            let dst_px = &mut dst_rgba[dst_idx..dst_idx + 4];
            let blended = blend_rgba_pixel_seeded(
                [dst_px[0], dst_px[1], dst_px[2], dst_px[3]],
                src_px,
                opacity,
                blend_mode,
                (dy * dst_width + dx) as u32,
            );
            dst_px.copy_from_slice(&blended);
        }
    }
}

fn invert_affine(transform: [f32; 6]) -> Option<[f32; 6]> {
    let a = transform[0];
    let c = transform[1];
    let tx = transform[2];
    let b = transform[3];
    let d = transform[4];
    let ty = transform[5];

    let det = a * d - b * c;
    if det.abs() <= 1.0e-6 {
        return None;
    }

    let inv_det = 1.0 / det;
    let ia = d * inv_det;
    let ic = -c * inv_det;
    let ib = -b * inv_det;
    let id = a * inv_det;
    let itx = -(ia * tx + ic * ty);
    let ity = -(ib * tx + id * ty);
    Some([ia, ic, itx, ib, id, ity])
}

fn sample_src_rgba(
    src_rgba: &[u8],
    src_w: usize,
    src_h: usize,
    sx: f32,
    sy: f32,
) -> Option<[u8; 4]> {
    let x = sx.round() as isize;
    let y = sy.round() as isize;
    if x < 0 || y < 0 || x >= src_w as isize || y >= src_h as isize {
        return None;
    }

    let idx = (y as usize * src_w + x as usize) * 4;
    if idx + 3 >= src_rgba.len() {
        return None;
    }
    Some([
        src_rgba[idx],
        src_rgba[idx + 1],
        src_rgba[idx + 2],
        src_rgba[idx + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_effects::{get_or_compile_scheduled_effect_graph, EffectRenderPlan};

    static TEST_COLOR_ENGINE: ColorEngine = ColorEngine::mondrian_standard();

    fn pinned_custom_engine(source: mondrian_core::OcioConfigSource) -> ColorEngine {
        ColorEngine::CustomOcio {
            identity: Box::new(
                mondrian_core::CustomOcioProjectIdentity::from_pinned_parts(
                    source,
                    "0".repeat(64),
                    "test-resolved-config".to_owned(),
                    "0".repeat(64),
                    "Linear Rec.709 (sRGB)".to_owned(),
                    "Test Display".to_owned(),
                    "Test View".to_owned(),
                    mondrian_core::CustomOcioLookIdentity::None,
                    Vec::new(),
                    Vec::new(),
                )
                .expect("structurally valid Custom OCIO test identity"),
            ),
        }
    }

    fn test_color_runtime(
        working_color_space: WorkingColorSpace,
    ) -> TimelineEffectColorRuntime<'static> {
        TimelineEffectColorRuntime::new(&TEST_COLOR_ENGINE, working_color_space)
    }

    fn working_frame(rgba: &[u8], width: u32, height: u32) -> CpuColorFrame {
        CpuColorFrame::working(working_frame_from_normalized_rgba8(
            width,
            height,
            rgba,
            WorkingColorSpace::LinearRec709,
        ))
    }

    fn identity_media<'a>(frame: &'a CpuColorFrame) -> TimelineCompositeElement<'a> {
        TimelineCompositeElement::Media(TimelineMediaLayer {
            frame,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
                .expect("compile identity graph"),
            frame_seed: 0,
        })
    }

    #[test]
    fn media_effects_are_applied_before_compositing() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[120, 80, 40, 255], 1, 1);
        let output = composite_timeline_elements(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                        exposure: 0.0,
                        contrast: 1.0,
                        saturation: 0.0,
                    }],
                })
                .expect("compile effect graph"),
                frame_seed: 0,
            })],
            TimelineCompositeOptions { empty_canvas_transparent: true },
            &mut scratch,
        )
        .expect("composite media effect");

        assert_eq!(output[0], output[1]);
        assert_eq!(output[1], output[2]);
        assert_eq!(output[3], 255);
    }

    #[test]
    fn custom_render_ops_flow_through_shared_compositor() {
        mondrian_effects::register_custom_render_processor(
            "plugin.render.test_invert",
            std::sync::Arc::new(|buffer, _, _, _, _| {
                for px in buffer.chunks_exact_mut(4) {
                    px[0] = 255u8.saturating_sub(px[0]);
                    px[1] = 255u8.saturating_sub(px[1]);
                    px[2] = 255u8.saturating_sub(px[2]);
                }
                Ok(())
            }),
        );

        let mut scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[10, 20, 30, 255], 1, 1);
        let output = composite_timeline_elements(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::Custom {
                        key: "plugin.render.test_invert".to_string(),
                        params: Default::default(),
                        cache_key: None,
                        cache_policy: mondrian_effects::EffectCachePolicy::Deterministic,
                    }],
                })
                .expect("compile custom graph"),
                frame_seed: 0,
            })],
            TimelineCompositeOptions { empty_canvas_transparent: true },
            &mut scratch,
        )
        .expect("composite custom effect");

        assert_eq!(&output[0..4], &[245, 235, 225, 255]);
    }

    #[test]
    fn adjustment_affects_only_layers_below_it() {
        let mut scratch = TimelineCompositeScratch::default();
        let lower = working_frame(&[255, 0, 0, 255, 255, 0, 0, 255], 2, 1);
        let upper = working_frame(&[0, 0, 0, 0, 0, 255, 0, 255], 2, 1);
        let output = composite_timeline_elements(
            2,
            1,
            &[
                identity_media(&lower),
                TimelineCompositeElement::Adjustment(TimelineAdjustmentLayer {
                    effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                        ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                            exposure: 0.0,
                            contrast: 1.0,
                            saturation: 0.0,
                        }],
                    })
                    .expect("compile adjustment graph"),
                    opacity: 1.0,
                    blend_mode: Some(BlendMode::Normal),
                    frame_seed: 0,
                }),
                identity_media(&upper),
            ],
            TimelineCompositeOptions::default(),
            &mut scratch,
        )
        .expect("composite adjustment stack");

        assert_eq!(&output[0..4], &[54, 54, 54, 255]);
        assert_eq!(&output[4..8], &[0, 255, 0, 255]);
    }

    #[test]
    fn media_blend_mode_is_applied_during_compositing() {
        let mut scratch = TimelineCompositeScratch::default();
        let base = working_frame(&[128, 64, 32, 255], 1, 1);
        let blend = working_frame(&[64, 192, 128, 255], 1, 1);
        let output = composite_timeline_elements(
            1,
            1,
            &[
                identity_media(&base),
                TimelineCompositeElement::Media(TimelineMediaLayer {
                    frame: &blend,
                    opacity: 1.0,
                    blend_mode: BlendMode::Multiply,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph: get_or_compile_scheduled_effect_graph(
                        &EffectRenderPlan::default(),
                    )
                    .expect("compile identity graph"),
                    frame_seed: 0,
                }),
            ],
            TimelineCompositeOptions::default(),
            &mut scratch,
        )
        .expect("composite blend mode");

        assert_eq!(&output[0..4], &[32, 48, 16, 255]);
    }

    #[test]
    fn float_linear_compositor_matches_normal_single_layer_and_preserves_alpha() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[64, 128, 192, 255], 1, 1);
        let frame = composite_timeline_elements_color_frame(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
                    .expect("compile identity graph"),
                frame_seed: 0,
            })],
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        );
        assert_eq!(frame.descriptor().domain, crate::ColorFrameDomain::Working);
        let output = frame
            .rgba_f32()
            .data
            .iter()
            .flat_map(|pixel| pixel.map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8))
            .collect::<Vec<_>>();

        assert_eq!(output[3], 255);
        assert!((output[0] as i16 - 64).abs() <= 1);
        assert!((output[1] as i16 - 128).abs() <= 1);
        assert!((output[2] as i16 - 192).abs() <= 1);
        assert!(scratch.media_source.is_empty());
        assert!(scratch.media_effect.is_empty());
    }

    #[test]
    fn float_linear_compositor_preserves_extended_solid_color_without_rgba8_scratch() {
        let mut scratch = TimelineCompositeScratch::default();
        let frame = composite_timeline_elements_color_frame(
            1,
            1,
            &[TimelineCompositeElement::SolidColor(
                TimelineSolidColorLayer {
                    color: Color { r: 1.25, g: 0.5, b: 0.125, a: 1.0 },
                    opacity: 1.0,
                    blend_mode: BlendMode::Normal,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph: get_or_compile_scheduled_effect_graph(
                        &EffectRenderPlan::default(),
                    )
                    .expect("compile identity graph"),
                    frame_seed: 0,
                },
            )],
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        );

        let px = frame.rgba_f32().data[0];
        assert_eq!(
            frame.descriptor().encoding,
            crate::ColorFrameEncoding::LinearFloat
        );
        assert!((px[0] - 1.25).abs() <= f32::EPSILON);
        assert!((px[1] - 0.5).abs() <= f32::EPSILON);
        assert!((px[2] - 0.125).abs() <= f32::EPSILON);
        assert!((px[3] - 1.0).abs() <= f32::EPSILON);
        assert!(scratch.solid_fill.is_empty());
        assert!(scratch.media_effect.is_empty());
    }

    #[test]
    fn float_linear_compositor_runs_color_adjust_effect_without_rgba8_scratch() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 1,
            height: 1,
            data: vec![[1.25, 0.25, 0.125, 1.0]],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let frame = composite_timeline_elements_color_frame(
            1,
            1,
            &[TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                        exposure: 1.0,
                        contrast: 1.0,
                        saturation: 1.0,
                    }],
                })
                .expect("compile float color adjust"),
                frame_seed: 0,
            })],
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        );

        let px = frame.rgba_f32().data[0];
        assert!((px[0] - 2.5).abs() <= 1.0e-6);
        assert!((px[1] - 0.5).abs() <= 1.0e-6);
        assert!((px[2] - 0.25).abs() <= 1.0e-6);
        assert!((px[3] - 1.0).abs() <= f32::EPSILON);
        assert!(scratch.media_source.is_empty());
        assert!(scratch.media_effect.is_empty());
    }

    #[test]
    fn float_linear_compositor_runs_normal_adjustment_without_rgba8_scratch() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 1,
            height: 1,
            data: vec![[1.25, 0.25, 0.125, 1.0]],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let frame = composite_timeline_elements_color_frame(
            1,
            1,
            &[
                identity_media(&media),
                TimelineCompositeElement::Adjustment(TimelineAdjustmentLayer {
                    effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                        ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                            exposure: 1.0,
                            contrast: 1.0,
                            saturation: 1.0,
                        }],
                    })
                    .expect("compile adjustment"),
                    opacity: 0.5,
                    blend_mode: Some(BlendMode::Normal),
                    frame_seed: 0,
                }),
            ],
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        );

        let px = frame.rgba_f32().data[0];
        assert!((px[0] - 1.875).abs() <= 1.0e-6);
        assert!((px[1] - 0.375).abs() <= 1.0e-6);
        assert!((px[2] - 0.1875).abs() <= 1.0e-6);
        assert!((px[3] - 1.0).abs() <= f32::EPSILON);
        assert!(scratch.media_source.is_empty());
        assert!(scratch.media_effect.is_empty());
        assert!(scratch.adjustment.is_empty());
    }

    #[test]
    fn float_linear_compositor_runs_media_and_solid_blend_modes_without_legacy_fallback() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[128, 96, 64, 255], 1, 1);
        let overlay = working_frame(&[64, 192, 128, 255], 1, 1);
        let elements = [
            identity_media(&media),
            TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &overlay,
                opacity: 0.75,
                blend_mode: BlendMode::Multiply,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
                    .expect("compile identity graph"),
                frame_seed: 0,
            }),
            TimelineCompositeElement::SolidColor(TimelineSolidColorLayer {
                color: Color { r: 0.25, g: 0.5, b: 1.0, a: 1.0 },
                opacity: 0.5,
                blend_mode: BlendMode::Screen,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
                    .expect("compile identity graph"),
                frame_seed: 0,
            }),
        ];

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        );

        assert_eq!(output.diagnostics.float_linear_composites, 1);
        assert_eq!(output.diagnostics.legacy_rgba8_composites, 0);
        assert_eq!(output.diagnostics.legacy_media_blend_mode, 0);
        assert_eq!(output.diagnostics.legacy_solid_blend_mode, 0);
        assert!(scratch.media_source.is_empty());
        assert!(scratch.solid_fill.is_empty());
    }

    #[test]
    fn float_linear_compositor_runs_adjustment_blend_modes_without_rgba8_scratch() {
        let mut float_scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[255, 0, 0, 255], 1, 1);
        let elements = [
            identity_media(&media),
            TimelineCompositeElement::Adjustment(TimelineAdjustmentLayer {
                effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                        exposure: 0.0,
                        contrast: 1.0,
                        saturation: 0.0,
                    }],
                })
                .expect("compile adjustment"),
                opacity: 1.0,
                blend_mode: Some(BlendMode::Multiply),
                frame_seed: 0,
            }),
        ];
        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut float_scratch,
        );

        let diagnostics = output.diagnostics;
        assert_eq!(diagnostics.float_linear_composites, 1);
        assert_eq!(diagnostics.legacy_rgba8_composites, 0);
        assert_eq!(diagnostics.legacy_adjustment_blend_mode, 0);
        assert_eq!(diagnostics.legacy_adjustment_effect, 0);
        assert!(output.frame.rgba_f32().data[0][0] < media.rgba_f32().data[0][0]);
        assert!(float_scratch.media_source.is_empty());
        assert!(float_scratch.adjustment.is_empty());
    }

    #[test]
    fn float_linear_compositor_runs_builtin_spatial_effects_without_rgba8_fallback() {
        let mut float_scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[120, 80, 40, 255], 1, 1);
        let builtin_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
            ops: vec![mondrian_effects::EffectRenderOp::GaussianBlur { radius: 1.0 }],
        })
        .expect("compile built-in effect");
        let elements = [
            TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &media,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: builtin_graph.clone(),
                frame_seed: 0,
            }),
            TimelineCompositeElement::SolidColor(TimelineSolidColorLayer {
                color: Color { r: 1.5, g: 0.25, b: 0.125, a: 1.0 },
                opacity: 0.5,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: builtin_graph,
                frame_seed: 0,
            }),
        ];

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut float_scratch,
        );

        assert_eq!(output.diagnostics.float_linear_composites, 1);
        assert_eq!(output.diagnostics.legacy_rgba8_composites, 0);
        assert_eq!(output.diagnostics.legacy_media_effect, 0);
        assert_eq!(output.diagnostics.legacy_solid_effect, 0);
        assert!(output.frame.rgba_f32().data[0][0] > 0.8);
    }

    #[test]
    fn float_linear_compositor_runs_clip_masks_without_rgba8_fallback() {
        let mut scratch = TimelineCompositeScratch::default();
        let graph = mondrian_effects::EffectRenderGraph {
            nodes: vec![
                mondrian_effects::EffectGraphNode {
                    id: mondrian_effects::EffectGraphNodeId(0),
                    kind: mondrian_effects::EffectGraphNodeKind::Source,
                },
                mondrian_effects::EffectGraphNode {
                    id: mondrian_effects::EffectGraphNodeId(1),
                    kind: mondrian_effects::EffectGraphNodeKind::MaskSource {
                        shape: mondrian_effects::MaskShape::Rectangle {
                            x: 0.0,
                            y: 0.0,
                            width: 1.0,
                            height: 1.0,
                            corner_radius: 0.0,
                        },
                        feather: 0.0,
                        expansion: 0.0,
                        opacity: 0.25,
                    },
                },
                mondrian_effects::EffectGraphNode {
                    id: mondrian_effects::EffectGraphNodeId(2),
                    kind: mondrian_effects::EffectGraphNodeKind::Mask {
                        input: mondrian_effects::EffectGraphNodeId(0),
                        mask: mondrian_effects::EffectGraphNodeId(1),
                        invert: false,
                        mask_op: mondrian_effects::MaskOp::Add,
                    },
                },
            ],
            output: Some(mondrian_effects::EffectGraphNodeId(2)),
        };
        let effect_graph = mondrian_effects::get_or_compile_scheduled_render_graph(graph)
            .expect("compile mask graph");
        let elements = [TimelineCompositeElement::SolidColor(
            TimelineSolidColorLayer {
                color: Color { r: 2.0, g: -0.25, b: 0.5, a: 1.0 },
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph,
                frame_seed: 0,
            },
        )];

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions { empty_canvas_transparent: true },
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        );
        let pixel = output.frame.rgba_f32().data[0];

        assert_eq!(output.diagnostics.float_linear_composites, 1);
        assert_eq!(output.diagnostics.legacy_rgba8_composites, 0);
        assert_eq!(output.diagnostics.legacy_solid_effect, 0);
        assert!((pixel[0] - 0.5).abs() <= 1.0e-6);
        assert!((pixel[1] + 0.0625).abs() <= 1.0e-6);
        assert!((pixel[3] - 1.0).abs() <= f32::EPSILON);
    }

    #[test]
    fn float_linear_compositor_applies_solid_affine_transform() {
        let mut scratch = TimelineCompositeScratch::default();
        let effect_graph = get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
            .expect("compile identity graph");
        let elements = [TimelineCompositeElement::SolidColor(
            TimelineSolidColorLayer {
                color: Color { r: 2.0, g: 0.25, b: 0.125, a: 1.0 },
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 1.0, 0.0, 1.0, 0.0],
                effect_graph,
                frame_seed: 0,
            },
        )];

        let output = composite_timeline_elements_color_frame(
            2,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        );
        let pixels = &output.rgba_f32().data;

        assert_eq!(pixels[0], [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(pixels[1], [2.0, 0.25, 0.125, 1.0]);
    }

    #[test]
    fn float_linear_compositor_falls_back_for_custom_effect_without_float_abi() {
        let media = working_frame(&[120, 80, 40, 255], 1, 1);
        let elements = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                ops: vec![mondrian_effects::EffectRenderOp::Custom {
                    key: "test.custom.rgba8-only".to_owned(),
                    params: serde_json::json!({}),
                    cache_key: None,
                    cache_policy: mondrian_effects::EffectCachePolicy::Deterministic,
                }],
            })
            .expect("compile custom media effect"),
            frame_seed: 0,
        })];

        let diagnostics = composite_path_diagnostics(&elements);
        assert_eq!(diagnostics.legacy_rgba8_composites, 1);
        assert_eq!(diagnostics.legacy_media_effect, 1);
    }

    #[test]
    fn unavailable_effect_domain_processor_blocks_instead_of_falling_back_to_rgba8() {
        let media = working_frame(&[120, 80, 40, 255], 1, 1);
        let display_domain = mondrian_effects::EffectColorDomain::DisplayEncodedRgb {
            color_space: mondrian_core::ColorSpace::Rec709,
        };
        let effect_graph = Arc::new(
            mondrian_effects::compile_scheduled_effect_graph_in_domain(
                &EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                        exposure: 0.25,
                        contrast: 1.0,
                        saturation: 1.0,
                    }],
                },
                mondrian_effects::EffectColorDomainContract::preserving(display_domain),
            )
            .expect("compile display-domain graph"),
        );
        let elements = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 0,
        })];
        let mut scratch = TimelineCompositeScratch::default();
        let unavailable_engine = pinned_custom_engine(mondrian_core::OcioConfigSource::Builtin {
            name: "test.invalid.effect-domain-config".to_owned(),
        });

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            TimelineEffectColorRuntime::new(&unavailable_engine, WorkingColorSpace::LinearRec709),
            &mut scratch,
        );

        assert_eq!(output.diagnostics.blocked_color_domain_composites, 1);
        assert_eq!(output.diagnostics.blocked_media_effect_domain, 1);
        assert_eq!(output.diagnostics.legacy_rgba8_composites, 0);
        assert_eq!(output.diagnostics.legacy_media_effect, 0);
        assert_eq!(
            output.diagnostics.color_path(),
            TimelineCompositeColorPath::Blocked
        );
        assert_eq!(output.frame.rgba_f32().data[0], [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(
            composite_timeline_elements(
                1,
                1,
                &elements,
                TimelineCompositeOptions::default(),
                &mut scratch,
            ),
            Err(
                mondrian_effects::EffectExecutionError::ColorDomainConversionRequired {
                    transitions: 2,
                }
            )
        );
    }

    #[test]
    fn stock_ocio_runtime_executes_display_domain_effect_and_returns_to_working_space() {
        let engine = ColorEngine::default();
        engine.ensure_loaded().expect("load Mondrian Standard OCIO package");
        let media = working_frame(&[120, 80, 40, 255], 1, 1);
        let input = media.rgba_f32().data[0];
        let display_domain =
            EffectColorDomain::DisplayEncodedRgb { color_space: mondrian_core::ColorSpace::Rec709 };
        let exposure = 0.25;
        let effect_graph = Arc::new(
            mondrian_effects::compile_scheduled_effect_graph_in_domain(
                &EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                        exposure,
                        contrast: 1.0,
                        saturation: 1.0,
                    }],
                },
                mondrian_effects::EffectColorDomainContract::preserving(display_domain),
            )
            .expect("compile display-domain graph"),
        );
        let elements = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 0,
        })];
        let mut scratch = TimelineCompositeScratch::default();

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            TimelineEffectColorRuntime::new(&engine, WorkingColorSpace::LinearRec709),
            &mut scratch,
        );

        let mut expected = [input];
        engine
            .convert_identity_float(
                expected.as_flattened_mut(),
                WorkingColorSpace::LinearRec709.into(),
                mondrian_core::ColorSpace::Rec709.into(),
            )
            .expect("convert working to display encoded");
        for channel in &mut expected[0][..3] {
            *channel *= 2.0f32.powf(exposure);
        }
        engine
            .convert_identity_float(
                expected.as_flattened_mut(),
                mondrian_core::ColorSpace::Rec709.into(),
                WorkingColorSpace::LinearRec709.into(),
            )
            .expect("convert display encoded to working");

        assert_eq!(
            output.diagnostics.color_path(),
            TimelineCompositeColorPath::FloatLinear
        );
        assert_eq!(output.diagnostics.blocked_color_domain_composites, 0);
        assert_eq!(output.diagnostics.blocked_media_effect_domain, 0);
        assert_eq!(output.diagnostics.legacy_rgba8_composites, 0);
        let actual = output.frame.rgba_f32().data[0];
        for channel in 0..4 {
            assert!(
                (actual[channel] - expected[0][channel]).abs() <= 2.0e-5,
                "channel {channel}: actual={} expected={}",
                actual[channel],
                expected[0][channel]
            );
        }
    }

    #[test]
    fn display_domain_graph_without_float_abi_stays_blocked_instead_of_claiming_legacy() {
        let engine = ColorEngine::default();
        let media = working_frame(&[120, 80, 40, 255], 1, 1);
        let display_domain =
            EffectColorDomain::DisplayEncodedRgb { color_space: mondrian_core::ColorSpace::Rec709 };
        let effect_graph = Arc::new(
            mondrian_effects::compile_scheduled_effect_graph_in_domain(
                &EffectRenderPlan {
                    ops: vec![mondrian_effects::EffectRenderOp::Custom {
                        key: "test.display.rgba8-only".to_owned(),
                        params: serde_json::json!({}),
                        cache_key: None,
                        cache_policy: mondrian_effects::EffectCachePolicy::Deterministic,
                    }],
                },
                mondrian_effects::EffectColorDomainContract::preserving(display_domain),
            )
            .expect("compile display-domain custom graph"),
        );
        let elements = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_graph,
            frame_seed: 0,
        })];

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            TimelineEffectColorRuntime::new(&engine, WorkingColorSpace::LinearRec709),
            &mut TimelineCompositeScratch::default(),
        );

        assert_eq!(
            output.diagnostics.color_path(),
            TimelineCompositeColorPath::Blocked
        );
        assert_eq!(output.diagnostics.blocked_media_effect_domain, 1);
        assert_eq!(output.diagnostics.legacy_media_effect, 0);
        assert_eq!(output.frame.rgba_f32().data[0], [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn float_linear_compositor_reports_clean_float_path_diagnostics() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = working_frame(&[64, 128, 192, 255], 1, 1);
        let elements = [identity_media(&media)];

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            1,
            1,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        );

        assert_eq!(output.diagnostics.elements, 1);
        assert_eq!(output.diagnostics.float_linear_composites, 1);
        assert_eq!(output.diagnostics.legacy_rgba8_composites, 0);
        assert!(!output.diagnostics.uses_legacy_rgba8());
        assert!(scratch.media_source.is_empty());
    }

    #[test]
    fn composite_color_path_summary_reports_clean_float_linear_path() {
        let diagnostics = TimelineCompositeDiagnostics {
            elements: 2,
            float_linear_composites: 2,
            ..TimelineCompositeDiagnostics::default()
        };

        let summary = diagnostics.color_path_summary();

        assert_eq!(summary.path, TimelineCompositeColorPath::FloatLinear);
        assert_eq!(summary.elements, 2);
        assert_eq!(summary.composite_plans(), 2);
        assert!(summary.legacy_breakdown.is_empty());
        assert!(summary.is_fully_float_linear());
        assert!(!diagnostics.uses_legacy_rgba8());
    }

    #[test]
    fn composite_color_path_summary_reports_structured_legacy_reasons() {
        let diagnostics = TimelineCompositeDiagnostics {
            elements: 4,
            float_linear_composites: 1,
            legacy_rgba8_composites: 1,
            legacy_media_transform: 1,
            legacy_solid_effect: 2,
            ..TimelineCompositeDiagnostics::default()
        };

        let summary = diagnostics.color_path_summary();

        assert_eq!(summary.path, TimelineCompositeColorPath::LegacyRgba8);
        assert_eq!(summary.composite_plans(), 2);
        assert_eq!(summary.legacy_breakdown.media_transform, 1);
        assert_eq!(summary.legacy_breakdown.solid_effect, 2);
        assert_eq!(summary.legacy_breakdown.total(), 3);
        assert!(!summary.is_fully_float_linear());
        assert!(diagnostics.uses_legacy_rgba8());
    }

    #[test]
    fn float_linear_compositor_handles_non_identity_transform() {
        let mut scratch = TimelineCompositeScratch::default();
        let media = working_frame(
            &[
                64, 128, 192, 255, 100, 150, 200, 255, 50, 100, 150, 255, 200, 50, 100, 255,
            ],
            2,
            2,
        );
        let elements = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [2.0, 0.0, 0.0, 0.0, 2.0, 0.0],
            effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
                .expect("compile identity graph"),
            frame_seed: 0,
        })];

        let output = composite_timeline_elements_color_frame_with_diagnostics(
            2,
            2,
            &elements,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch,
        );

        assert_eq!(output.diagnostics.float_linear_composites, 1);
        assert_eq!(output.diagnostics.legacy_rgba8_composites, 0);
        assert_eq!(output.diagnostics.legacy_media_transform, 0);
        assert!(!output.diagnostics.uses_legacy_rgba8());
    }

    #[test]
    fn float_linear_compositor_deterministic_across_identity_and_scale() {
        let mut scratch_a = TimelineCompositeScratch::default();
        let mut scratch_b = TimelineCompositeScratch::default();
        let media = working_frame(
            &[
                100, 150, 200, 255, 50, 100, 150, 255, 200, 50, 100, 255, 150, 200, 50, 255,
            ],
            2,
            2,
        );
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let scale_1x = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

        let elements_a = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 0.8,
            blend_mode: BlendMode::Multiply,
            transform: identity,
            effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
                .expect("compile identity graph"),
            frame_seed: 42,
        })];
        let elements_b = [TimelineCompositeElement::Media(TimelineMediaLayer {
            frame: &media,
            opacity: 0.8,
            blend_mode: BlendMode::Multiply,
            transform: scale_1x,
            effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan::default())
                .expect("compile identity graph"),
            frame_seed: 42,
        })];

        let out_a = composite_timeline_elements_color_frame_with_diagnostics(
            2,
            2,
            &elements_a,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch_a,
        );
        let out_b = composite_timeline_elements_color_frame_with_diagnostics(
            2,
            2,
            &elements_b,
            TimelineCompositeOptions::default(),
            test_color_runtime(WorkingColorSpace::LinearRec709),
            &mut scratch_b,
        );

        assert_eq!(out_a.frame.rgba_f32().data, out_b.frame.rgba_f32().data);
    }

    #[test]
    fn composite_color_path_summary_fails_closed_on_legacy_reason_mismatch() {
        let diagnostics = TimelineCompositeDiagnostics {
            float_linear_composites: 1,
            legacy_media_effect: 1,
            ..TimelineCompositeDiagnostics::default()
        };

        let summary = diagnostics.color_path_summary();

        assert_eq!(summary.path, TimelineCompositeColorPath::LegacyRgba8);
        assert_eq!(summary.legacy_rgba8_composites, 0);
        assert_eq!(summary.legacy_breakdown.total(), 1);
        assert!(diagnostics.uses_legacy_rgba8());
    }
}
