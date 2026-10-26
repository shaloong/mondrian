//! Pre-materialization Effect-route preparation for Preview Timeline frames.
//!
//! Dynamic graph evaluation and finite-temporal extraction happen before this
//! Module. It then prepares the exact Effect route of every remaining
//! placement without decoding media or touching pixels. The root Sequence may
//! use either the complete CPU compositor or a complete Viewer Effect route.
//! Other Viewer compositing constraints remain owned by Viewer lowering. A
//! nested child still has to materialize through its complete CPU route, but
//! that resulting CPU working frame is a valid input to the parent Nested
//! Clip's own heterogeneous Effect route.

use std::sync::Arc;

use mondrian_core::timeline_data::TimelineClipExecutionRef;
use mondrian_effects::{CompiledEffectGraph, EffectFrameExtent, EffectGraphExecutionBudget};

use crate::{
    admit_timeline_render_plan_for_cpu_compositor, PreparedHeterogeneousEffectRoute,
    TimelineCompositeScratch, TimelineRenderPlan, TimelineRenderPlanElement,
    TimelineTransitionInputPlan,
};

/// Exact route selected for one evaluated placement's current-frame Effect
/// graph.
#[derive(Debug, Clone)]
pub enum PreparedTimelinePreviewEffectRoute {
    /// The complete graph lowers to the Viewer GPU implementation.
    Gpu,
    /// The complete graph has one prepared CPU-F32 prefix and GPU-F32 suffix.
    Heterogeneous(PreparedHeterogeneousEffectRoute),
    /// The graph requires the complete CPU compositor path.
    Cpu {
        processor_input: Option<Arc<crate::PreparedTimelineClipInputPlan>>,
    },
}

impl PreparedTimelinePreviewEffectRoute {
    pub fn processor_input_plan(&self) -> Option<&Arc<crate::PreparedTimelineClipInputPlan>> {
        match self {
            Self::Gpu => None,
            Self::Cpu { processor_input } => processor_input.as_ref(),
            Self::Heterogeneous(route) => route.clip_input_plan(),
        }
    }

    /// Whether source materialization must provide a CPU working-linear frame.
    pub const fn requires_cpu_working_frame(&self) -> bool {
        !matches!(self, Self::Gpu)
    }

    /// Prepared heterogeneous route, when that exact path was selected.
    pub const fn heterogeneous(&self) -> Option<&PreparedHeterogeneousEffectRoute> {
        match self {
            Self::Heterogeneous(route) => Some(route),
            Self::Gpu | Self::Cpu { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
struct PreparedTimelinePreviewEffectRouteEntry {
    placement: TimelineClipExecutionRef,
    graph_fingerprint: [u8; 32],
    source: TimelinePreviewEffectSource,
    requires_input_service: bool,
    route: PreparedTimelinePreviewEffectRoute,
}

/// Physical source shape available before one placement's Effect graph runs.
///
/// This is deliberately more precise than a generic "heterogeneous supported"
/// flag: a prepared CPU prefix needs an independently materialized CPU working
/// frame. A procedural Solid can be generated under that same execution
/// authority, while an Adjustment would require reading back the
/// already-composited GPU accumulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelinePreviewEffectSource {
    MediaCpuWorkingFrame,
    NodeRasterCpuWorkingFrame,
    NestedCpuWorkingFrame,
    CompositedAccumulator,
}

impl TimelinePreviewEffectSource {
    fn cpu_prefix_extent(
        self,
        placement: TimelineClipExecutionRef,
        node_extent: EffectFrameExtent,
        nested_extent: &mut impl FnMut(TimelineClipExecutionRef) -> Option<EffectFrameExtent>,
    ) -> Result<Option<EffectFrameExtent>, TimelinePreviewEffectRouteError> {
        match self {
            Self::MediaCpuWorkingFrame | Self::NodeRasterCpuWorkingFrame => Ok(Some(node_extent)),
            Self::NestedCpuWorkingFrame => nested_extent(placement).map(Some).ok_or(
                TimelinePreviewEffectRouteError::MissingNestedMaterializationExtent { placement },
            ),
            Self::CompositedAccumulator => Ok(None),
        }
    }
}

/// Immutable per-Sequence route ledger prepared before source
/// materialization.
#[derive(Debug, Clone)]
pub struct PreparedTimelinePreviewEffectRoutes {
    entries: Arc<[PreparedTimelinePreviewEffectRouteEntry]>,
    cpu_compositor_blocker: Option<Arc<str>>,
}

impl PreparedTimelinePreviewEffectRoutes {
    /// Carry original Clip input semantics through the route's actual CPU job.
    /// Scheduling authority is supplied only when that job starts.
    pub fn bind_clip_processor_inputs(
        mut self,
        program: &Arc<crate::PreparedVisualProgram>,
        owner: &crate::TimelineFrameExecutionRequest,
    ) -> Result<Self, TimelinePreviewEffectRouteError> {
        for entry in Arc::make_mut(&mut self.entries) {
            if !entry.requires_input_service
                || entry.source == TimelinePreviewEffectSource::CompositedAccumulator
            {
                continue;
            }
            let plan = Arc::new(
                program
                    .clip_processor_input_plan(entry.placement, owner)
                    .map_err(TimelinePreviewEffectRouteError::ProcessorInput)?,
            );
            match &mut entry.route {
                PreparedTimelinePreviewEffectRoute::Heterogeneous(route) => {
                    route.bind_clip_input(plan)
                }
                PreparedTimelinePreviewEffectRoute::Cpu { processor_input } => {
                    *processor_input = Some(plan)
                }
                PreparedTimelinePreviewEffectRoute::Gpu => {}
            }
        }
        Ok(self)
    }
    /// Prepare every current-frame graph route under exact source rasters and
    /// one heterogeneous graph-planning grant.
    ///
    /// `nested_extent` must resolve the canonical current child binding from
    /// the already-prepared visual closure. It is queried only when a Nested
    /// placement cannot use the complete GPU route and needs a CPU prefix.
    pub fn prepare(
        plan: &TimelineRenderPlan,
        node_extent: EffectFrameExtent,
        heterogeneous_budget: EffectGraphExecutionBudget,
        scratch: &mut TimelineCompositeScratch,
        mut nested_extent: impl FnMut(TimelineClipExecutionRef) -> Option<EffectFrameExtent>,
    ) -> Result<Self, TimelinePreviewEffectRouteError> {
        let cpu_compositor_blocker =
            admit_timeline_render_plan_for_cpu_compositor(plan).err().map(|error| {
                let detail: Arc<str> = error.to_string().into();
                detail
            });
        let mut entries = Vec::new();
        visit_plan_graphs(plan, &mut |placement, graph, source| {
            let route = if source == TimelinePreviewEffectSource::MediaCpuWorkingFrame
                && graph.image_scale_requirement()
                    == mondrian_effects::EffectImageScaleRequirement::Unit
            {
                // A Full media raster is independent of the node canvas. The current
                // heterogeneous ledger is sized to that canvas, so keep the complete
                // graph on the existing source-before-composite CPU route.
                PreparedTimelinePreviewEffectRoute::Cpu { processor_input: None }
            } else if scratch.get_or_lower_effect_gpu_plan(graph).is_ok() {
                PreparedTimelinePreviewEffectRoute::Gpu
            } else if let Some(extent) =
                source.cpu_prefix_extent(placement, node_extent, &mut nested_extent)?
            {
                PreparedHeterogeneousEffectRoute::prepare(
                    Arc::clone(graph),
                    extent,
                    heterogeneous_budget,
                )
                .map(PreparedTimelinePreviewEffectRoute::Heterogeneous)
                .unwrap_or(PreparedTimelinePreviewEffectRoute::Cpu { processor_input: None })
            } else {
                PreparedTimelinePreviewEffectRoute::Cpu { processor_input: None }
            };
            entries.push(PreparedTimelinePreviewEffectRouteEntry {
                placement,
                graph_fingerprint: graph.semantic_fingerprint(),
                source,
                requires_input_service: graph.requires_processor_input_service(),
                route,
            });
            Ok(())
        })?;
        Ok(Self { entries: entries.into(), cpu_compositor_blocker })
    }

    /// Prove that the root Sequence has one complete Effect execution route.
    pub fn admit_root(&self) -> Result<(), TimelinePreviewEffectRouteError> {
        if self.cpu_compositor_blocker.is_none()
            || self
                .entries
                .iter()
                .all(|entry| !matches!(entry.route, PreparedTimelinePreviewEffectRoute::Cpu { .. }))
        {
            return Ok(());
        }
        let viewer_blocked_placements = self
            .entries
            .iter()
            .filter(|entry| matches!(entry.route, PreparedTimelinePreviewEffectRoute::Cpu { .. }))
            .count();
        let Some(cpu_blocker) = &self.cpu_compositor_blocker else {
            return Ok(());
        };
        Err(TimelinePreviewEffectRouteError::NoCompleteRootRoute {
            cpu_blocker: Arc::clone(cpu_blocker),
            viewer_blocked_placements,
        })
    }

    /// Prove that a nested Sequence can materialize through the current CPU
    /// working-frame Adapter.
    pub fn admit_nested_materialization(&self) -> Result<(), TimelinePreviewEffectRouteError> {
        match &self.cpu_compositor_blocker {
            None => Ok(()),
            Some(cpu_blocker) => Err(
                TimelinePreviewEffectRouteError::NestedMaterializationRequiresCpu {
                    cpu_blocker: Arc::clone(cpu_blocker),
                },
            ),
        }
    }

    /// Resolve the frozen route for one exact placement and graph identity.
    pub fn route_for(
        &self,
        placement: TimelineClipExecutionRef,
        graph: &CompiledEffectGraph,
    ) -> Result<&PreparedTimelinePreviewEffectRoute, TimelinePreviewEffectRouteError> {
        self.entry_for(placement, graph).map(|entry| &entry.route)
    }

    pub fn processor_input_plan_for(
        &self,
        placement: TimelineClipExecutionRef,
        graph: &CompiledEffectGraph,
    ) -> Result<Option<&Arc<crate::PreparedTimelineClipInputPlan>>, TimelinePreviewEffectRouteError>
    {
        self.entry_for(placement, graph).map(|entry| entry.route.processor_input_plan())
    }

    fn entry_for(
        &self,
        placement: TimelineClipExecutionRef,
        graph: &CompiledEffectGraph,
    ) -> Result<&PreparedTimelinePreviewEffectRouteEntry, TimelinePreviewEffectRouteError> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.placement == placement)
            .ok_or(TimelinePreviewEffectRouteError::MissingPlacement { placement })?;
        let actual = graph.semantic_fingerprint();
        if entry.graph_fingerprint != actual {
            return Err(TimelinePreviewEffectRouteError::GraphIdentityChanged { placement });
        }
        Ok(entry)
    }
}

/// Fail-closed route-ledger validation error.
#[derive(Debug, Clone, thiserror::Error)]
pub enum TimelinePreviewEffectRouteError {
    /// Clip input semantics did not belong to this frame's original program.
    #[error("Timeline processor input binding failed: {0}")]
    ProcessorInput(crate::TimelineTemporalPreparationError),
    /// Full-source temporal ROI coordinates have no producer in this slice.
    #[error("Unit-scale temporal source materialization is unsupported for {placement:?}")]
    UnitScaleTemporalUnsupported {
        /// Exact original temporal graph placement, before current-frame rewriting.
        placement: TimelineClipExecutionRef,
    },
    /// Neither the complete CPU compositor nor the complete Viewer Effect
    /// route can execute the root plan.
    #[error(
        "root Timeline has no complete Effect route: CPU compositor blocked by {cpu_blocker}; Viewer Effect route has {viewer_blocked_placements} CPU-only placement(s)"
    )]
    NoCompleteRootRoute {
        /// Canonical CPU admission diagnostic.
        cpu_blocker: Arc<str>,
        /// Placements that cannot enter GPU or heterogeneous execution.
        viewer_blocked_placements: usize,
    },
    /// Nested Sequence materialization currently produces a CPU working frame.
    #[error("nested Timeline cannot materialize through the CPU Adapter: {cpu_blocker}")]
    NestedMaterializationRequiresCpu {
        /// Canonical CPU admission diagnostic.
        cpu_blocker: Arc<str>,
    },
    /// The canonical closure did not expose the current child raster needed by
    /// a parent Nested Clip's CPU prefix.
    #[error("Timeline Effect route has no nested materialization extent for {placement:?}")]
    MissingNestedMaterializationExtent {
        /// Parent placement whose exact child binding is missing.
        placement: TimelineClipExecutionRef,
    },
    /// Materialization requested a placement absent from the prepared ledger.
    #[error("Timeline Effect route ledger has no placement {placement:?}")]
    MissingPlacement {
        /// Missing evaluated placement.
        placement: TimelineClipExecutionRef,
    },
    /// The graph no longer matches the immutable pre-materialization decision.
    #[error("Timeline Effect graph identity changed after route preparation for {placement:?}")]
    GraphIdentityChanged {
        /// Placement whose graph changed.
        placement: TimelineClipExecutionRef,
    },
}

fn visit_plan_graphs(
    plan: &TimelineRenderPlan,
    visitor: &mut impl FnMut(
        TimelineClipExecutionRef,
        &Arc<CompiledEffectGraph>,
        TimelinePreviewEffectSource,
    ) -> Result<(), TimelinePreviewEffectRouteError>,
) -> Result<(), TimelinePreviewEffectRouteError> {
    for element in &plan.elements {
        match element {
            TimelineRenderPlanElement::Media(layer) => {
                visitor(
                    layer.placement,
                    &layer.effect_graph,
                    TimelinePreviewEffectSource::MediaCpuWorkingFrame,
                )?;
            }
            TimelineRenderPlanElement::BasicTitle(layer) => {
                visitor(
                    layer.placement,
                    &layer.effect_graph,
                    TimelinePreviewEffectSource::NodeRasterCpuWorkingFrame,
                )?;
            }
            TimelineRenderPlanElement::NestedSequence(layer) => {
                visitor(
                    layer.placement,
                    &layer.effect_graph,
                    TimelinePreviewEffectSource::NestedCpuWorkingFrame,
                )?;
            }
            TimelineRenderPlanElement::SolidColor(layer) => {
                visitor(
                    layer.placement,
                    &layer.effect_graph,
                    TimelinePreviewEffectSource::NodeRasterCpuWorkingFrame,
                )?;
            }
            TimelineRenderPlanElement::Adjustment(layer) => {
                visitor(
                    layer.placement,
                    &layer.effect_graph,
                    TimelinePreviewEffectSource::CompositedAccumulator,
                )?;
            }
            TimelineRenderPlanElement::CrossDissolve(transition) => {
                visit_transition_graph(&transition.left, visitor)?;
                visit_transition_graph(&transition.right, visitor)?;
            }
            TimelineRenderPlanElement::TimelineGrade(_) => {}
        }
    }
    Ok(())
}

fn visit_transition_graph(
    input: &TimelineTransitionInputPlan,
    visitor: &mut impl FnMut(
        TimelineClipExecutionRef,
        &Arc<CompiledEffectGraph>,
        TimelinePreviewEffectSource,
    ) -> Result<(), TimelinePreviewEffectRouteError>,
) -> Result<(), TimelinePreviewEffectRouteError> {
    match input {
        TimelineTransitionInputPlan::Transparent => {}
        TimelineTransitionInputPlan::Media(layer) => {
            visitor(
                layer.placement,
                &layer.effect_graph,
                TimelinePreviewEffectSource::MediaCpuWorkingFrame,
            )?;
        }
        TimelineTransitionInputPlan::BasicTitle(layer) => {
            visitor(
                layer.placement,
                &layer.effect_graph,
                TimelinePreviewEffectSource::NodeRasterCpuWorkingFrame,
            )?;
        }
        TimelineTransitionInputPlan::NestedSequence(layer) => {
            visitor(
                layer.placement,
                &layer.effect_graph,
                TimelinePreviewEffectSource::NestedCpuWorkingFrame,
            )?;
        }
        TimelineTransitionInputPlan::SolidColor(layer) => {
            visitor(
                layer.placement,
                &layer.effect_graph,
                TimelinePreviewEffectSource::NodeRasterCpuWorkingFrame,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod source_materialization_tests {
    use super::*;
    use crate::timeline_composite::source_materialization_tests::{source_graph, source_plan};
    #[test]
    fn preview_root_and_nested_routes_admit_no_image_sources_without_advancing_them() {
        let (graph, probe) = source_graph();
        let plan = source_plan(graph);
        let mut scratch = TimelineCompositeScratch::default();
        let routes = PreparedTimelinePreviewEffectRoutes::prepare(
            &plan,
            EffectFrameExtent::new(3, 2),
            EffectGraphExecutionBudget::new(1 << 20, 1 << 20, 1 << 20, 32, 64),
            &mut scratch,
            |_| None,
        )
        .unwrap();
        routes.admit_root().unwrap();
        routes.admit_nested_materialization().unwrap();
        let crate::TimelineRenderPlanElement::SolidColor(layer) = &plan.elements[0] else {
            panic!("solid source plan")
        };
        assert!(matches!(
            routes.route_for(layer.placement, &layer.effect_graph).unwrap(),
            PreparedTimelinePreviewEffectRoute::Cpu { .. }
        ));
        probe.assert_untouched();
    }
}
