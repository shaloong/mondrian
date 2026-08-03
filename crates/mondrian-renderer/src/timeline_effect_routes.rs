//! Pre-materialization Effect-route preparation for Preview Timeline frames.
//!
//! Dynamic graph evaluation and finite-temporal extraction happen before this
//! Module. It then prepares the exact Effect route of every remaining
//! placement without decoding media or touching pixels. The root Sequence may
//! use either the complete CPU compositor or a complete Viewer Effect route.
//! Other Viewer compositing constraints remain owned by Viewer lowering.
//! Nested Sequences currently materialize to CPU working frames and therefore
//! retain the stricter complete-CPU obligation.

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
    Cpu,
}

impl PreparedTimelinePreviewEffectRoute {
    /// Whether source materialization must provide a CPU working-linear frame.
    pub const fn requires_cpu_working_frame(&self) -> bool {
        !matches!(self, Self::Gpu)
    }

    /// Prepared heterogeneous route, when that exact path was selected.
    pub const fn heterogeneous(&self) -> Option<&PreparedHeterogeneousEffectRoute> {
        match self {
            Self::Heterogeneous(route) => Some(route),
            Self::Gpu | Self::Cpu => None,
        }
    }
}

#[derive(Debug, Clone)]
struct PreparedTimelinePreviewEffectRouteEntry {
    placement: TimelineClipExecutionRef,
    graph_fingerprint: [u8; 32],
    route: PreparedTimelinePreviewEffectRoute,
}

/// Immutable per-Sequence route ledger prepared before source
/// materialization.
#[derive(Debug, Clone)]
pub struct PreparedTimelinePreviewEffectRoutes {
    entries: Arc<[PreparedTimelinePreviewEffectRouteEntry]>,
    cpu_compositor_blocker: Option<Arc<str>>,
}

impl PreparedTimelinePreviewEffectRoutes {
    /// Prepare every current-frame graph route under one exact raster and
    /// heterogeneous graph-planning grant.
    pub fn prepare(
        plan: &TimelineRenderPlan,
        extent: EffectFrameExtent,
        heterogeneous_budget: EffectGraphExecutionBudget,
        scratch: &mut TimelineCompositeScratch,
    ) -> Self {
        let cpu_compositor_blocker =
            admit_timeline_render_plan_for_cpu_compositor(plan).err().map(|error| {
                let detail: Arc<str> = error.to_string().into();
                detail
            });
        let mut entries = Vec::new();
        visit_plan_graphs(plan, &mut |placement, graph, heterogeneous_supported| {
            let route = if scratch.get_or_lower_effect_gpu_plan(graph).is_ok() {
                PreparedTimelinePreviewEffectRoute::Gpu
            } else if heterogeneous_supported {
                PreparedHeterogeneousEffectRoute::prepare(
                    Arc::clone(graph),
                    extent,
                    heterogeneous_budget,
                )
                .map(PreparedTimelinePreviewEffectRoute::Heterogeneous)
                .unwrap_or(PreparedTimelinePreviewEffectRoute::Cpu)
            } else {
                PreparedTimelinePreviewEffectRoute::Cpu
            };
            entries.push(PreparedTimelinePreviewEffectRouteEntry {
                placement,
                graph_fingerprint: graph.semantic_fingerprint(),
                route,
            });
        });
        Self { entries: entries.into(), cpu_compositor_blocker }
    }

    /// Prove that the root Sequence has one complete Effect execution route.
    pub fn admit_root(&self) -> Result<(), TimelinePreviewEffectRouteError> {
        if self.cpu_compositor_blocker.is_none()
            || self
                .entries
                .iter()
                .all(|entry| !matches!(entry.route, PreparedTimelinePreviewEffectRoute::Cpu))
        {
            return Ok(());
        }
        let viewer_blocked_placements = self
            .entries
            .iter()
            .filter(|entry| matches!(entry.route, PreparedTimelinePreviewEffectRoute::Cpu))
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
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.placement == placement)
            .ok_or(TimelinePreviewEffectRouteError::MissingPlacement { placement })?;
        let actual = graph.semantic_fingerprint();
        if entry.graph_fingerprint != actual {
            return Err(TimelinePreviewEffectRouteError::GraphIdentityChanged { placement });
        }
        Ok(&entry.route)
    }
}

/// Fail-closed route-ledger validation error.
#[derive(Debug, Clone, thiserror::Error)]
pub enum TimelinePreviewEffectRouteError {
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
    visitor: &mut impl FnMut(TimelineClipExecutionRef, &Arc<CompiledEffectGraph>, bool),
) {
    for element in &plan.elements {
        match element {
            TimelineRenderPlanElement::Media(layer) => {
                visitor(layer.placement, &layer.effect_graph, true);
            }
            TimelineRenderPlanElement::BasicTitle(layer) => {
                visitor(layer.placement, &layer.effect_graph, true);
            }
            TimelineRenderPlanElement::NestedSequence(layer) => {
                visitor(layer.placement, &layer.effect_graph, false);
            }
            TimelineRenderPlanElement::SolidColor(layer) => {
                visitor(layer.placement, &layer.effect_graph, false);
            }
            TimelineRenderPlanElement::Adjustment(layer) => {
                visitor(layer.placement, &layer.effect_graph, false);
            }
            TimelineRenderPlanElement::CrossDissolve(transition) => {
                visit_transition_graph(&transition.left, visitor);
                visit_transition_graph(&transition.right, visitor);
            }
        }
    }
}

fn visit_transition_graph(
    input: &TimelineTransitionInputPlan,
    visitor: &mut impl FnMut(TimelineClipExecutionRef, &Arc<CompiledEffectGraph>, bool),
) {
    match input {
        TimelineTransitionInputPlan::Transparent => {}
        TimelineTransitionInputPlan::Media(layer) => {
            visitor(layer.placement, &layer.effect_graph, true);
        }
        TimelineTransitionInputPlan::BasicTitle(layer) => {
            visitor(layer.placement, &layer.effect_graph, true);
        }
        TimelineTransitionInputPlan::NestedSequence(layer) => {
            visitor(layer.placement, &layer.effect_graph, false);
        }
        TimelineTransitionInputPlan::SolidColor(layer) => {
            visitor(layer.placement, &layer.effect_graph, false);
        }
    }
}
