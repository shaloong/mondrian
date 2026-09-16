//! Sequence-owned grading hierarchy and stable authoring operations.

use mondrian_core::{
    AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintError, GradeDefinition,
    GradeDefinitionId, GradeGroupId, GradeVersion, GradeVersionId, MondrianError, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Resolve-style group processing around a Clip's own processing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradeGroup {
    pub id: GradeGroupId,
    pub name: String,
    #[serde(default)]
    pub pre_clip_grade: Option<GradeDefinitionId>,
    #[serde(default)]
    pub post_clip_grade: Option<GradeDefinitionId>,
}

impl GradeGroup {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: GradeGroupId::new(),
            name: name.into(),
            pre_clip_grade: None,
            post_clip_grade: None,
        }
    }
}

impl AuthoringFootprint for GradeGroup {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        collector.collect(&self.name)
    }
}

/// One product-visible assignment target for a shared grade definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "scope",
    content = "target",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum GradeScope {
    Clip(mondrian_core::ClipId),
    GroupPre(GradeGroupId),
    GroupPost(GradeGroupId),
    Timeline,
}

impl crate::Sequence {
    /// Add a new identity grade definition and return its stable identity.
    pub fn add_grade_definition(&mut self, name: impl Into<String>) -> GradeDefinitionId {
        let definition = GradeDefinition::new(name);
        let id = definition.id;
        self.grade_definitions.push(definition);
        id
    }

    /// Add one version by structurally sharing/copying the supplied graph.
    pub fn add_grade_version(
        &mut self,
        definition_id: GradeDefinitionId,
        name: impl Into<String>,
        graph: mondrian_core::GradeGraph,
    ) -> Result<GradeVersionId> {
        let graph = graph.duplicate_with_fresh_author_identities()?;
        let definition = self
            .grade_definitions
            .iter_mut()
            .find(|definition| definition.id == definition_id)
            .ok_or_else(|| {
                grade_hierarchy_error(format!("grade definition {definition_id} does not exist"))
            })?;
        if definition.versions.len() >= mondrian_core::MAX_GRADE_VERSIONS {
            return Err(grade_hierarchy_error(format!(
                "grade definition {definition_id} reached the version limit"
            )));
        }
        let version = GradeVersion::new(name, graph);
        let id = version.id;
        definition.versions.push(version);
        Ok(id)
    }

    /// Switch the active graph without copying any Effect or graph author state.
    pub fn activate_grade_version(
        &mut self,
        definition_id: GradeDefinitionId,
        version_id: GradeVersionId,
    ) -> Result<bool> {
        let definition = self
            .grade_definitions
            .iter_mut()
            .find(|definition| definition.id == definition_id)
            .ok_or_else(|| {
                grade_hierarchy_error(format!("grade definition {definition_id} does not exist"))
            })?;
        if !definition.versions.iter().any(|version| version.id == version_id) {
            return Err(grade_hierarchy_error(format!(
                "grade version {version_id} does not belong to definition {definition_id}"
            )));
        }
        if definition.active_version == version_id {
            return Ok(false);
        }
        definition.active_version = version_id;
        Ok(true)
    }

    /// Assign or clear one shared grade at a typed hierarchy scope.
    pub fn assign_grade(
        &mut self,
        scope: GradeScope,
        definition_id: Option<GradeDefinitionId>,
    ) -> Result<bool> {
        if let Some(id) = definition_id
            && !self.grade_definitions.iter().any(|definition| definition.id == id)
        {
            return Err(grade_hierarchy_error(format!(
                "grade definition {id} does not exist"
            )));
        }
        let target = match scope {
            GradeScope::Clip(clip_id) => {
                &mut self
                    .find_clip_mut(clip_id)
                    .ok_or_else(|| grade_hierarchy_error(format!("Clip {clip_id} does not exist")))?
                    .grade
            }
            GradeScope::GroupPre(group_id) => {
                &mut self
                    .grade_groups
                    .iter_mut()
                    .find(|group| group.id == group_id)
                    .ok_or_else(|| {
                        grade_hierarchy_error(format!("grade group {group_id} does not exist"))
                    })?
                    .pre_clip_grade
            }
            GradeScope::GroupPost(group_id) => {
                &mut self
                    .grade_groups
                    .iter_mut()
                    .find(|group| group.id == group_id)
                    .ok_or_else(|| {
                        grade_hierarchy_error(format!("grade group {group_id} does not exist"))
                    })?
                    .post_clip_grade
            }
            GradeScope::Timeline => &mut self.timeline_grade,
        };
        if *target == definition_id {
            return Ok(false);
        }
        *target = definition_id;
        Ok(true)
    }

    /// Resolve the exact grade definition referenced by one scope.
    pub fn grade_definition(&self, id: GradeDefinitionId) -> Option<&GradeDefinition> {
        self.grade_definitions.iter().find(|definition| definition.id == id)
    }

    /// Validate stable identity, active versions, and all hierarchy references.
    pub(crate) fn validate_grade_hierarchy(&self) -> Result<()> {
        let mut definitions = HashMap::with_capacity(self.grade_definitions.len());
        let mut graph_nodes = HashSet::new();
        let mut effect_ids = HashSet::new();
        let mut version_ids = HashSet::new();
        for definition in &self.grade_definitions {
            definition.validate_author_state()?;
            if definitions.insert(definition.id, definition).is_some() {
                return Err(grade_hierarchy_error(format!(
                    "duplicate grade definition identity {}",
                    definition.id
                )));
            }
            for version in &definition.versions {
                if !version_ids.insert(version.id) {
                    return Err(grade_hierarchy_error(format!(
                        "duplicate grade version identity {}",
                        version.id
                    )));
                }
                for node in &version.graph.nodes {
                    if !graph_nodes.insert(node.id) {
                        return Err(grade_hierarchy_error(format!(
                            "duplicate grade graph node identity {}",
                            node.id
                        )));
                    }
                    if let mondrian_core::GradeGraphNodeKind::Effect { effect, .. } = &node.kind
                        && !effect_ids.insert(effect.id)
                    {
                        return Err(grade_hierarchy_error(format!(
                            "duplicate grade Effect identity {}",
                            effect.id
                        )));
                    }
                }
            }
        }

        let mut groups = HashSet::with_capacity(self.grade_groups.len());
        for group in &self.grade_groups {
            if group.name.trim().is_empty() {
                return Err(grade_hierarchy_error(format!(
                    "grade group {} has an empty name",
                    group.id
                )));
            }
            if !groups.insert(group.id) {
                return Err(grade_hierarchy_error(format!(
                    "duplicate grade group identity {}",
                    group.id
                )));
            }
            validate_optional_definition(group.pre_clip_grade, &definitions)?;
            validate_optional_definition(group.post_clip_grade, &definitions)?;
        }
        validate_optional_definition(self.timeline_grade, &definitions)?;
        for clip in self.video_tracks.iter().flat_map(|track| &track.clips) {
            validate_optional_definition(clip.grade, &definitions)?;
            if let Some(group_id) = clip.grade_group
                && !groups.contains(&group_id)
            {
                return Err(grade_hierarchy_error(format!(
                    "Clip {} references missing grade group {group_id}",
                    clip.id
                )));
            }
        }
        Ok(())
    }
}

fn validate_optional_definition(
    id: Option<GradeDefinitionId>,
    definitions: &HashMap<GradeDefinitionId, &GradeDefinition>,
) -> Result<()> {
    if let Some(id) = id
        && !definitions.contains_key(&id)
    {
        return Err(grade_hierarchy_error(format!(
            "grade hierarchy references missing definition {id}"
        )));
    }
    Ok(())
}

fn grade_hierarchy_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "validate_grade_hierarchy".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{apply_split_edit, Clip, Sequence, SplitEditRequest};
    use mondrian_core::{
        effect_data::{EffectNode, EffectType},
        AssetId, AuthoringList, FramePosition, GradeGraph, GradeGraphNode, GradeGraphNodeId,
        GradeGraphNodeKind,
    };

    fn at(sequence: &Sequence, frame: i64) -> mondrian_core::TimelineTime {
        mondrian_core::TimelineTime::from_frame_position(FramePosition::new(
            frame,
            sequence.time_base(),
        ))
        .expect("test time")
    }

    fn add_video_clip(sequence: &mut Sequence, frame: i64) -> mondrian_core::ClipId {
        let clip =
            Clip::new(AssetId::new(), at(sequence, frame), at(sequence, 10)).expect("valid Clip");
        let id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("place Clip");
        id
    }

    fn serial_grade_graph() -> GradeGraph {
        let input = GradeGraphNodeId::new();
        let output = GradeGraphNodeId::new();
        GradeGraph {
            nodes: AuthoringList::from([
                GradeGraphNode { id: input, kind: GradeGraphNodeKind::Input },
                GradeGraphNode {
                    id: output,
                    kind: GradeGraphNodeKind::Effect {
                        input,
                        effect: EffectNode::new(EffectType::BasicCorrection),
                    },
                },
            ]),
            output,
        }
    }

    #[test]
    fn grade_scope_adjacent_tag_round_trips_every_variant() {
        let scopes = [
            GradeScope::Clip(mondrian_core::ClipId::new()),
            GradeScope::GroupPre(GradeGroupId::new()),
            GradeScope::GroupPost(GradeGroupId::new()),
            GradeScope::Timeline,
        ];
        for scope in scopes {
            let json = serde_json::to_string(&scope).expect("serialize GradeScope");
            let reopened: GradeScope = serde_json::from_str(&json).expect("deserialize GradeScope");
            assert_eq!(reopened, scope);
        }
    }

    #[test]
    fn shared_definition_resolves_from_clip_group_and_timeline_scopes() {
        let mut sequence = Sequence::new("shared-grade");
        let left = add_video_clip(&mut sequence, 0);
        let right = add_video_clip(&mut sequence, 20);
        let definition = sequence.add_grade_definition("Shared look");
        let group = GradeGroup::new("Scene");
        let group_id = group.id;
        sequence.grade_groups.push(group);

        sequence
            .assign_grade(GradeScope::Clip(left), Some(definition))
            .expect("left grade");
        sequence
            .assign_grade(GradeScope::Clip(right), Some(definition))
            .expect("right grade");
        sequence
            .assign_grade(GradeScope::GroupPre(group_id), Some(definition))
            .expect("group pre");
        sequence
            .assign_grade(GradeScope::GroupPost(group_id), Some(definition))
            .expect("group post");
        sequence
            .assign_grade(GradeScope::Timeline, Some(definition))
            .expect("timeline grade");
        sequence.find_clip_mut(left).expect("left Clip").grade_group = Some(group_id);
        sequence.find_clip_mut(right).expect("right Clip").grade_group = Some(group_id);

        sequence.validate_grade_hierarchy().expect("valid hierarchy");
        assert_eq!(
            sequence.grade_definitions.len(),
            1,
            "assignments only retain references"
        );
        assert_eq!(
            sequence.find_clip(left).expect("left Clip").grade,
            Some(definition)
        );
        assert_eq!(
            sequence.find_clip(right).expect("right Clip").grade,
            Some(definition)
        );
    }

    #[test]
    fn hierarchy_rejects_missing_definition_and_group_strong_references() {
        let mut sequence = Sequence::new("broken-grade");
        let clip_id = add_video_clip(&mut sequence, 0);
        sequence.find_clip_mut(clip_id).expect("Clip").grade = Some(GradeDefinitionId::new());
        assert!(sequence.validate_grade_hierarchy().is_err());

        sequence.find_clip_mut(clip_id).expect("Clip").grade = None;
        sequence.find_clip_mut(clip_id).expect("Clip").grade_group = Some(GradeGroupId::new());
        assert!(sequence.validate_grade_hierarchy().is_err());
    }

    #[test]
    fn adding_and_activating_version_preserves_existing_graph_and_identity() {
        let mut sequence = Sequence::new("versions");
        let definition_id = sequence.add_grade_definition("Look");
        let original = sequence
            .grade_definition(definition_id)
            .expect("definition")
            .active()
            .expect("active")
            .clone();
        let version_id = sequence
            .add_grade_version(definition_id, "Alternate", serial_grade_graph())
            .expect("add version");
        assert!(sequence.activate_grade_version(definition_id, version_id).expect("activate"));

        let definition = sequence.grade_definition(definition_id).expect("definition");
        assert_eq!(definition.versions.len(), 2);
        assert_eq!(
            definition.versions[0], original,
            "version switching cannot rewrite old graph"
        );
        assert_eq!(definition.active_version, version_id);
    }

    #[test]
    fn split_inherits_shared_grade_and_group_references() {
        let mut sequence = Sequence::new("grade-split");
        let clip_id = add_video_clip(&mut sequence, 0);
        let definition = sequence.add_grade_definition("Look");
        let group = GradeGroup::new("Scene");
        let group_id = group.id;
        sequence.grade_groups.push(group);
        sequence
            .assign_grade(GradeScope::Clip(clip_id), Some(definition))
            .expect("assign");
        sequence.find_clip_mut(clip_id).expect("Clip").grade_group = Some(group_id);

        let split_at = at(&sequence, 5);
        apply_split_edit(&mut sequence, &SplitEditRequest { clip_id, at: split_at })
            .expect("split grade Clip");
        assert_eq!(sequence.video_tracks[0].clips.len(), 2);
        for clip in &sequence.video_tracks[0].clips {
            assert_eq!(clip.grade, Some(definition));
            assert_eq!(clip.grade_group, Some(group_id));
        }
    }

    #[test]
    fn sequence_duplicate_rekeys_complete_grade_graph_and_preserves_internal_sharing() {
        let mut sequence = Sequence::new("grade-duplicate");
        let left = add_video_clip(&mut sequence, 0);
        let right = add_video_clip(&mut sequence, 20);
        let definition_id = sequence.add_grade_definition("Shared look");
        let version_id = sequence
            .add_grade_version(definition_id, "Active", serial_grade_graph())
            .expect("add active graph");
        sequence.activate_grade_version(definition_id, version_id).expect("activate");
        let group = GradeGroup::new("Scene");
        let group_id = group.id;
        sequence.grade_groups.push(group);
        for clip_id in [left, right] {
            sequence
                .assign_grade(GradeScope::Clip(clip_id), Some(definition_id))
                .expect("assign");
            sequence.find_clip_mut(clip_id).expect("Clip").grade_group = Some(group_id);
        }
        sequence
            .assign_grade(GradeScope::GroupPre(group_id), Some(definition_id))
            .expect("pre");
        sequence
            .assign_grade(GradeScope::Timeline, Some(definition_id))
            .expect("timeline");

        let original_definition = sequence.grade_definitions[0].clone();
        let original_group = sequence.grade_groups[0].clone();
        sequence.fork_author_identities_for_sequence_duplicate();
        sequence.validate_grade_hierarchy().expect("valid duplicated hierarchy");

        let duplicated = &sequence.grade_definitions[0];
        assert_ne!(duplicated.id, original_definition.id);
        assert_ne!(
            duplicated.active_version,
            original_definition.active_version
        );
        let duplicated_graph = &duplicated.active().expect("active version").graph;
        let original_graph = &original_definition.active().expect("original active").graph;
        assert_ne!(duplicated_graph.output, original_graph.output);
        assert!(duplicated_graph.nodes.iter().all(|node| {
            node.kind
                .inputs()
                .iter()
                .all(|input| duplicated_graph.nodes.iter().any(|candidate| candidate.id == *input))
        }));
        assert_ne!(sequence.grade_groups[0].id, original_group.id);
        assert_eq!(sequence.grade_groups[0].pre_clip_grade, Some(duplicated.id));
        assert_eq!(sequence.timeline_grade, Some(duplicated.id));
        assert!(sequence.video_tracks[0]
            .clips
            .iter()
            .all(|clip| clip.grade == Some(duplicated.id)
                && clip.grade_group == Some(sequence.grade_groups[0].id)));

        let original_effect = original_graph.nodes.iter().find_map(|node| match &node.kind {
            GradeGraphNodeKind::Effect { effect, .. } => Some(effect.id),
            _ => None,
        });
        let duplicated_effect = duplicated_graph.nodes.iter().find_map(|node| match &node.kind {
            GradeGraphNodeKind::Effect { effect, .. } => Some(effect.id),
            _ => None,
        });
        assert_ne!(duplicated_effect, original_effect);
    }
}
