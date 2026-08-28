//! Closed, persistable color-grade authoring graph and shared-version model.
//!
//! This is author state, not an execution graph. `mondrian-effects` is the
//! sole owner of binding definitions and lowering this algebra into the
//! executable `CompiledEffectGraph` used by Preview and Export.

use crate::effect_data::EffectNode;
use crate::{
    AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintError, AuthoringList,
    BlendMode, EffectId, GradeDefinitionId, GradeGraphNodeId, GradeVersionId, MondrianError,
    Result, ShotMatchEvidence,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Hard authoring limit protecting validation, preparation, and UI traversal.
pub const MAX_GRADE_GRAPH_NODES: usize = 256;
/// Hard fan-in limit for one parallel compositor node.
pub const MAX_GRADE_GRAPH_INPUTS: usize = 32;
/// Hard version count for one shared grade definition.
pub const MAX_GRADE_VERSIONS: usize = 128;

/// One stable node in a grade authoring DAG.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradeGraphNode {
    pub id: GradeGraphNodeId,
    pub kind: GradeGraphNodeKind,
}

/// Closed grade-graph node algebra.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GradeGraphNodeKind {
    /// The picture entering this grade scope.
    Input,
    /// Apply one ordinary Effect definition to one upstream picture.
    Effect {
        input: GradeGraphNodeId,
        effect: EffectNode,
    },
    /// Ordered N-input branch compositor.
    Parallel {
        inputs: AuthoringList<GradeGraphNodeId>,
        blend_mode: BlendMode,
        opacity: f32,
    },
    /// Two-input layer mix with explicit base/overlay ownership.
    Layer {
        base: GradeGraphNodeId,
        overlay: GradeGraphNodeId,
        blend_mode: BlendMode,
        opacity: f32,
    },
}

impl GradeGraphNodeKind {
    /// Direct authoring dependencies of this node.
    pub fn inputs(&self) -> Vec<GradeGraphNodeId> {
        match self {
            Self::Input => Vec::new(),
            Self::Effect { input, .. } => vec![*input],
            Self::Parallel { inputs, .. } => inputs.iter().copied().collect(),
            Self::Layer { base, overlay, .. } => vec![*base, *overlay],
        }
    }
}

impl AuthoringFootprint for GradeGraphNodeKind {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        match self {
            Self::Input => Ok(()),
            Self::Effect { input: _, effect } => collector.collect(effect),
            Self::Parallel { inputs, blend_mode: _, opacity: _ } => collector.collect(inputs),
            Self::Layer { .. } => Ok(()),
        }
    }
}

impl AuthoringFootprint for GradeGraphNode {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        collector.collect(&self.kind)
    }
}

/// One closed grade authoring DAG with exactly one input and one output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradeGraph {
    pub nodes: AuthoringList<GradeGraphNode>,
    pub output: GradeGraphNodeId,
}

impl Default for GradeGraph {
    fn default() -> Self {
        Self::identity()
    }
}

impl GradeGraph {
    /// Create an identity graph with a stable input/output node.
    pub fn identity() -> Self {
        let input = GradeGraphNodeId::new();
        Self {
            nodes: AuthoringList::from([GradeGraphNode {
                id: input,
                kind: GradeGraphNodeKind::Input,
            }]),
            output: input,
        }
    }

    /// Return the unique graph input when the author graph is valid enough to identify it.
    pub fn input(&self) -> Option<GradeGraphNodeId> {
        let mut inputs = self
            .nodes
            .iter()
            .filter_map(|node| matches!(node.kind, GradeGraphNodeKind::Input).then_some(node.id));
        let input = inputs.next()?;
        inputs.next().is_none().then_some(input)
    }

    /// Fork this graph into an independent author branch while preserving its topology and values.
    ///
    /// Creating a Grade Version is a copy-on-author operation: node, Effect,
    /// and automation identities must be fresh so later edits cannot alias the
    /// source version. Activating an existing version remains an O(1) identity
    /// switch and performs no graph copy.
    pub fn duplicate_with_fresh_author_identities(&self) -> Result<Self> {
        self.validate_author_state()?;
        let node_ids = self
            .nodes
            .iter()
            .map(|node| (node.id, GradeGraphNodeId::new()))
            .collect::<HashMap<_, _>>();
        let remap = |id: GradeGraphNodeId| {
            node_ids.get(&id).copied().ok_or_else(|| {
                grade_graph_error(format!(
                    "grade graph identity fork could not resolve node {id}"
                ))
            })
        };
        let mut duplicated = self.clone();
        duplicated.output = remap(duplicated.output)?;
        for node in &mut duplicated.nodes {
            node.id = remap(node.id)?;
            match &mut node.kind {
                GradeGraphNodeKind::Input => {}
                GradeGraphNodeKind::Effect { input, effect } => {
                    *input = remap(*input)?;
                    effect.id = EffectId::new();
                    effect.properties.fork_author_identities();
                }
                GradeGraphNodeKind::Parallel { inputs, .. } => {
                    for input in inputs {
                        *input = remap(*input)?;
                    }
                }
                GradeGraphNodeKind::Layer { base, overlay, .. } => {
                    *base = remap(*base)?;
                    *overlay = remap(*overlay)?;
                }
            }
        }
        duplicated.validate_author_state()?;
        Ok(duplicated)
    }

    /// Validate size, identity, reference closure, acyclicity, and full output reachability.
    pub fn validate_author_state(&self) -> Result<()> {
        if self.nodes.is_empty() || self.nodes.len() > MAX_GRADE_GRAPH_NODES {
            return Err(grade_graph_error(format!(
                "grade graph must contain 1..={MAX_GRADE_GRAPH_NODES} nodes"
            )));
        }
        let mut by_id = HashMap::with_capacity(self.nodes.len());
        let mut effect_ids = HashSet::new();
        let mut input_count = 0_usize;
        for node in &self.nodes {
            if by_id.insert(node.id, node).is_some() {
                return Err(grade_graph_error(format!(
                    "duplicate grade graph node identity {}",
                    node.id
                )));
            }
            match &node.kind {
                GradeGraphNodeKind::Input => input_count += 1,
                GradeGraphNodeKind::Effect { effect, .. } => {
                    effect.validate_author_state()?;
                    if !effect_ids.insert(effect.id) {
                        return Err(grade_graph_error(format!(
                            "duplicate Effect identity {} inside grade graph",
                            effect.id
                        )));
                    }
                }
                GradeGraphNodeKind::Parallel { inputs, opacity, .. } => {
                    if inputs.is_empty() || inputs.len() > MAX_GRADE_GRAPH_INPUTS {
                        return Err(grade_graph_error(format!(
                            "parallel node {} must contain 1..={MAX_GRADE_GRAPH_INPUTS} inputs",
                            node.id
                        )));
                    }
                    validate_opacity(node.id, *opacity)?;
                }
                GradeGraphNodeKind::Layer { opacity, .. } => {
                    validate_opacity(node.id, *opacity)?;
                }
            }
        }
        if input_count != 1 {
            return Err(grade_graph_error(format!(
                "grade graph must contain exactly one Input node, found {input_count}"
            )));
        }
        if !by_id.contains_key(&self.output) {
            return Err(grade_graph_error("grade graph output does not exist"));
        }

        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();
        visit_grade_node(self.output, &by_id, &mut visiting, &mut visited)?;
        if visited.len() != self.nodes.len() {
            return Err(grade_graph_error(
                "every grade graph node must be reachable from the selected output",
            ));
        }
        Ok(())
    }
}

impl AuthoringFootprint for GradeGraph {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        collector.collect(&self.nodes)
    }
}

/// Named immutable author variant within one shared grade definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradeVersion {
    pub id: GradeVersionId,
    pub name: String,
    pub graph: GradeGraph,
    /// Auditable origin of this version's initial graph.
    #[serde(default)]
    pub origin: GradeVersionOrigin,
}

/// Provenance of one authored Grade Version.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GradeVersionOrigin {
    /// Manually created or edited author version.
    #[default]
    Manual,
    /// Deterministic Shot Match result with complete input/output evidence.
    ShotMatch { evidence: ShotMatchEvidence },
}

impl GradeVersion {
    pub fn new(name: impl Into<String>, graph: GradeGraph) -> Self {
        Self {
            id: GradeVersionId::new(),
            name: name.into(),
            graph,
            origin: GradeVersionOrigin::Manual,
        }
    }
}

impl AuthoringFootprint for GradeVersionOrigin {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        match self {
            Self::Manual => Ok(()),
            Self::ShotMatch { evidence } => collector.collect(evidence),
        }
    }
}

impl AuthoringFootprint for GradeVersion {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        collector.collect(&self.name)?;
        collector.collect(&self.graph)?;
        collector.collect(&self.origin)
    }
}

/// Sequence-owned grade state referenced by clip, group, or timeline scopes.
///
/// Reusing this identity from multiple scopes is the canonical shared-grade
/// mechanism; switching `active_version` never duplicates graph state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradeDefinition {
    pub id: GradeDefinitionId,
    pub name: String,
    pub versions: AuthoringList<GradeVersion>,
    pub active_version: GradeVersionId,
}

impl GradeDefinition {
    pub fn new(name: impl Into<String>) -> Self {
        let version = GradeVersion::new("Version 1", GradeGraph::identity());
        let active_version = version.id;
        Self {
            id: GradeDefinitionId::new(),
            name: name.into(),
            versions: AuthoringList::from([version]),
            active_version,
        }
    }

    pub fn active(&self) -> Option<&GradeVersion> {
        self.versions.iter().find(|version| version.id == self.active_version)
    }

    pub fn validate_author_state(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(grade_graph_error(format!(
                "grade definition {} has an empty name",
                self.id
            )));
        }
        if self.versions.is_empty() || self.versions.len() > MAX_GRADE_VERSIONS {
            return Err(grade_graph_error(format!(
                "grade definition {} must contain 1..={MAX_GRADE_VERSIONS} versions",
                self.id
            )));
        }
        let mut ids = HashSet::with_capacity(self.versions.len());
        for version in &self.versions {
            if version.name.trim().is_empty() {
                return Err(grade_graph_error(format!(
                    "grade version {} has an empty name",
                    version.id
                )));
            }
            if !ids.insert(version.id) {
                return Err(grade_graph_error(format!(
                    "duplicate grade version identity {}",
                    version.id
                )));
            }
            version.graph.validate_author_state()?;
            if let GradeVersionOrigin::ShotMatch { evidence } = &version.origin {
                evidence.validate()?;
            }
        }
        if !ids.contains(&self.active_version) {
            return Err(grade_graph_error(format!(
                "grade definition {} active version {} does not exist",
                self.id, self.active_version
            )));
        }
        Ok(())
    }
}

impl AuthoringFootprint for GradeDefinition {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        collector.collect(&self.name)?;
        collector.collect(&self.versions)
    }
}

fn visit_grade_node(
    id: GradeGraphNodeId,
    by_id: &HashMap<GradeGraphNodeId, &GradeGraphNode>,
    visiting: &mut HashSet<GradeGraphNodeId>,
    visited: &mut HashSet<GradeGraphNodeId>,
) -> Result<()> {
    if visited.contains(&id) {
        return Ok(());
    }
    if !visiting.insert(id) {
        return Err(grade_graph_error(format!(
            "grade graph contains a cycle at node {id}"
        )));
    }
    let node = by_id
        .get(&id)
        .ok_or_else(|| grade_graph_error(format!("grade graph references missing node {id}")))?;
    for input in node.kind.inputs() {
        visit_grade_node(input, by_id, visiting, visited)?;
    }
    visiting.remove(&id);
    visited.insert(id);
    Ok(())
}

fn validate_opacity(node_id: GradeGraphNodeId, opacity: f32) -> Result<()> {
    if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
        return Err(grade_graph_error(format!(
            "grade graph node {node_id} opacity must be finite and in 0..=1"
        )));
    }
    Ok(())
}

fn grade_graph_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "validate_grade_graph".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_data::EffectType;

    #[test]
    fn parallel_grade_graph_round_trips_and_validates() {
        let input = GradeGraphNodeId::new();
        let first = GradeGraphNodeId::new();
        let second = GradeGraphNodeId::new();
        let output = GradeGraphNodeId::new();
        let graph = GradeGraph {
            nodes: AuthoringList::from([
                GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
                GradeGraphNode {
                    id: first,
                    kind: GradeGraphNodeKind::Effect {
                        input,
                        effect: EffectNode::new(EffectType::BasicCorrection),
                    },
                },
                GradeGraphNode {
                    id: second,
                    kind: GradeGraphNodeKind::Effect {
                        input,
                        effect: EffectNode::new(EffectType::ColorWheel),
                    },
                },
                GradeGraphNode {
                    id: output,
                    kind: GradeGraphNodeKind::Parallel {
                        inputs: AuthoringList::from([first, second]),
                        blend_mode: BlendMode::Normal,
                        opacity: 1.0,
                    },
                },
            ]),
            output,
        };
        graph.validate_author_state().expect("valid graph");
        let json = serde_json::to_string(&graph).expect("serialize");
        let reopened: GradeGraph = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(reopened, graph);
    }

    #[test]
    fn grade_graph_duplicate_forks_all_instance_identities_and_preserves_topology() {
        let input = GradeGraphNodeId::new();
        let graded = GradeGraphNodeId::new();
        let graph = GradeGraph {
            nodes: AuthoringList::from([
                GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
                GradeGraphNode {
                    id: graded,
                    kind: GradeGraphNodeKind::Effect {
                        input,
                        effect: EffectNode::new(EffectType::BasicCorrection),
                    },
                },
            ]),
            output: graded,
        };
        let duplicated = graph
            .duplicate_with_fresh_author_identities()
            .expect("duplicate valid Grade Graph");
        duplicated.validate_author_state().expect("valid duplicate");

        let original_nodes = graph.nodes.iter().map(|node| node.id).collect::<HashSet<_>>();
        assert!(duplicated.nodes.iter().all(|node| !original_nodes.contains(&node.id)));
        let original_effect = graph.nodes.iter().find_map(|node| match &node.kind {
            GradeGraphNodeKind::Effect { effect, .. } => Some(effect.id),
            _ => None,
        });
        let duplicated_effect = duplicated.nodes.iter().find_map(|node| match &node.kind {
            GradeGraphNodeKind::Effect { effect, .. } => Some(effect.id),
            _ => None,
        });
        assert_ne!(duplicated_effect, original_effect);
        assert_eq!(duplicated.nodes.len(), graph.nodes.len());
    }

    #[test]
    fn grade_graph_rejects_cycles_and_unreachable_nodes() {
        let input = GradeGraphNodeId::new();
        let a = GradeGraphNodeId::new();
        let b = GradeGraphNodeId::new();
        let cyclic = GradeGraph {
            nodes: AuthoringList::from([
                GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
                GradeGraphNode {
                    id: a,
                    kind: GradeGraphNodeKind::Layer {
                        base: b,
                        overlay: input,
                        blend_mode: BlendMode::Normal,
                        opacity: 1.0,
                    },
                },
                GradeGraphNode {
                    id: b,
                    kind: GradeGraphNodeKind::Layer {
                        base: a,
                        overlay: input,
                        blend_mode: BlendMode::Normal,
                        opacity: 1.0,
                    },
                },
            ]),
            output: a,
        };
        assert!(cyclic.validate_author_state().is_err());

        let mut unreachable = GradeGraph::identity();
        let input = unreachable.output;
        unreachable.nodes.push(GradeGraphNode {
            id: GradeGraphNodeId::new(),
            kind: GradeGraphNodeKind::Effect {
                input,
                effect: EffectNode::new(EffectType::BasicCorrection),
            },
        });
        assert!(unreachable.validate_author_state().is_err());
    }

    #[test]
    fn layer_graph_and_grade_versions_round_trip_without_copying_active_identity() {
        let input = GradeGraphNodeId::new();
        let graded = GradeGraphNodeId::new();
        let output = GradeGraphNodeId::new();
        let graph = GradeGraph {
            nodes: AuthoringList::from([
                GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
                GradeGraphNode {
                    id: graded,
                    kind: GradeGraphNodeKind::Effect {
                        input,
                        effect: EffectNode::new(EffectType::BasicCorrection),
                    },
                },
                GradeGraphNode {
                    id: output,
                    kind: GradeGraphNodeKind::Layer {
                        base: input,
                        overlay: graded,
                        blend_mode: BlendMode::Normal,
                        opacity: 0.5,
                    },
                },
            ]),
            output,
        };
        let mut definition = GradeDefinition::new("Shared look");
        let version = GradeVersion::new("Alternate", graph);
        definition.active_version = version.id;
        definition.versions.push(version);
        definition.validate_author_state().expect("valid shared grade");

        let json = serde_json::to_string(&definition).expect("serialize shared grade");
        let reopened: GradeDefinition =
            serde_json::from_str(&json).expect("deserialize shared grade");
        assert_eq!(reopened, definition);
        assert_eq!(
            reopened.active().expect("active version").id,
            definition.active_version
        );
    }

    #[test]
    fn graph_validation_rejects_missing_references_duplicate_identities_and_limits() {
        let input = GradeGraphNodeId::new();
        let output = GradeGraphNodeId::new();
        let missing = GradeGraph {
            nodes: AuthoringList::from([
                GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
                GradeGraphNode {
                    id: output,
                    kind: GradeGraphNodeKind::Effect {
                        input: GradeGraphNodeId::new(),
                        effect: EffectNode::new(EffectType::BasicCorrection),
                    },
                },
            ]),
            output,
        };
        assert!(missing.validate_author_state().is_err());

        let duplicate_node = GradeGraph {
            nodes: AuthoringList::from([
                GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
                GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
            ]),
            output: input,
        };
        assert!(duplicate_node.validate_author_state().is_err());

        let effect = EffectNode::new(EffectType::BasicCorrection);
        let left = GradeGraphNodeId::new();
        let right = GradeGraphNodeId::new();
        let layer = GradeGraphNodeId::new();
        let duplicate_effect = GradeGraph {
            nodes: AuthoringList::from([
                GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
                GradeGraphNode {
                    id: left,
                    kind: GradeGraphNodeKind::Effect { input, effect: effect.clone() },
                },
                GradeGraphNode {
                    id: right,
                    kind: GradeGraphNodeKind::Effect { input, effect },
                },
                GradeGraphNode {
                    id: layer,
                    kind: GradeGraphNodeKind::Layer {
                        base: left,
                        overlay: right,
                        blend_mode: BlendMode::Normal,
                        opacity: 1.0,
                    },
                },
            ]),
            output: layer,
        };
        assert!(duplicate_effect.validate_author_state().is_err());

        let too_many_inputs = GradeGraph {
            nodes: AuthoringList::from([
                GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
                GradeGraphNode {
                    id: output,
                    kind: GradeGraphNodeKind::Parallel {
                        inputs: std::iter::repeat_n(input, MAX_GRADE_GRAPH_INPUTS + 1).collect(),
                        blend_mode: BlendMode::Normal,
                        opacity: 1.0,
                    },
                },
            ]),
            output,
        };
        assert!(too_many_inputs.validate_author_state().is_err());

        let too_many_nodes = GradeGraph {
            nodes: std::iter::repeat_with(|| GradeGraphNode {
                id: GradeGraphNodeId::new(),
                kind: GradeGraphNodeKind::Input,
            })
            .take(MAX_GRADE_GRAPH_NODES + 1)
            .collect(),
            output: input,
        };
        assert!(too_many_nodes.validate_author_state().is_err());
    }

    #[test]
    fn grade_definition_rejects_duplicate_versions_and_missing_active_version() {
        let mut definition = GradeDefinition::new("Look");
        let duplicate = definition.versions[0].clone();
        definition.versions.push(duplicate);
        assert!(definition.validate_author_state().is_err());

        definition.versions.pop();
        definition.active_version = GradeVersionId::new();
        assert!(definition.validate_author_state().is_err());
    }
}
