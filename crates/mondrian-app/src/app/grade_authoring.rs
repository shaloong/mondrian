use super::product_action::{
    GradeActivateVersionPayload, GradeAddEffectPayload, GradeAddVersionPayload, GradeAssignPayload,
    GradeCreateDefinitionPayload, GradeCreateGroupPayload, GradeProductAction,
    GradeReplaceActiveGraphPayload,
};
use super::AppState;
use mondrian_core::{GradeDefinitionId, Result};
use mondrian_timeline::{GradeGroup, GradeScope, Sequence};

impl AppState {
    pub(super) fn dispatch_grade_product_action(
        &mut self,
        action: GradeProductAction,
    ) -> Result<()> {
        match action {
            GradeProductAction::CreateDefinition(payload) => {
                self.create_grade_definition(payload)?;
            }
            GradeProductAction::Assign(payload) => self.assign_grade(payload)?,
            GradeProductAction::CreateGroup(payload) => self.create_grade_group(payload)?,
            GradeProductAction::AddVersion(payload) => self.add_grade_version(payload)?,
            GradeProductAction::ActivateVersion(payload) => self.activate_grade_version(payload)?,
            GradeProductAction::ReplaceActiveGraph(payload) => {
                self.replace_active_grade_graph(*payload)?
            }
            GradeProductAction::AddEffect(payload) => self.add_grade_effect(payload)?,
        }
        Ok(())
    }

    fn create_grade_definition(
        &mut self,
        payload: GradeCreateDefinitionPayload,
    ) -> Result<GradeDefinitionId> {
        let name = payload.name.trim().to_owned();
        if name.is_empty() {
            return Err(grade_error("grade definition name cannot be empty"));
        }
        let (_, id) = self.commit_active_sequence_edit("创建共享调色", move |sequence| {
            if let Some(scope) = payload.assign_to {
                validate_grade_scope_write(sequence, scope)?;
            }
            let id = sequence.add_grade_definition(name);
            if let Some(scope) = payload.assign_to {
                sequence.assign_grade(scope, Some(id))?;
            }
            Ok((sequence.id, id))
        })?;
        Ok(id)
    }

    fn assign_grade(&mut self, payload: GradeAssignPayload) -> Result<()> {
        self.commit_active_sequence_edit("分配共享调色", move |sequence| {
            validate_grade_scope_write(sequence, payload.scope)?;
            if !sequence.assign_grade(payload.scope, payload.definition_id)? {
                return Err(grade_error(
                    "grade scope already has the requested assignment",
                ));
            }
            Ok(sequence.id)
        })?;
        Ok(())
    }

    fn create_grade_group(&mut self, payload: GradeCreateGroupPayload) -> Result<()> {
        let name = payload.name.trim().to_owned();
        if name.is_empty() {
            return Err(grade_error("grade group name cannot be empty"));
        }
        self.commit_active_sequence_edit("创建调色组", move |sequence| {
            if let Some(clip_id) = payload.clip_id {
                validate_grade_scope_write(sequence, GradeScope::Clip(clip_id))?;
            }
            let group = GradeGroup::new(name);
            let group_id = group.id;
            sequence.grade_groups.push(group);
            if let Some(clip_id) = payload.clip_id {
                sequence
                    .find_clip_mut(clip_id)
                    .ok_or_else(|| grade_error(format!("Clip {clip_id} does not exist")))?
                    .grade_group = Some(group_id);
            }
            Ok(sequence.id)
        })?;
        Ok(())
    }

    fn add_grade_version(&mut self, payload: GradeAddVersionPayload) -> Result<()> {
        self.commit_active_sequence_edit("创建调色版本", move |sequence| {
            let graph = sequence
                .grade_definition(payload.definition_id)
                .and_then(|definition| definition.active())
                .map(|version| version.graph.clone())
                .ok_or_else(|| grade_error("active grade version does not exist"))?;
            let version = sequence.add_grade_version(payload.definition_id, payload.name, graph)?;
            if payload.activate {
                sequence.activate_grade_version(payload.definition_id, version)?;
            }
            Ok(sequence.id)
        })?;
        Ok(())
    }

    fn activate_grade_version(&mut self, payload: GradeActivateVersionPayload) -> Result<()> {
        self.commit_active_sequence_edit("切换调色版本", move |sequence| {
            if !sequence.activate_grade_version(payload.definition_id, payload.version_id)? {
                return Err(grade_error("grade version is already active"));
            }
            Ok(sequence.id)
        })?;
        Ok(())
    }

    fn replace_active_grade_graph(
        &mut self,
        payload: GradeReplaceActiveGraphPayload,
    ) -> Result<()> {
        payload.graph.validate_author_state()?;
        self.commit_active_sequence_edit("编辑调色图", move |sequence| {
            let definition = sequence
                .grade_definitions
                .iter_mut()
                .find(|definition| definition.id == payload.definition_id)
                .ok_or_else(|| grade_error("grade definition does not exist"))?;
            let version = definition
                .versions
                .iter_mut()
                .find(|version| version.id == definition.active_version)
                .ok_or_else(|| grade_error("active grade version does not exist"))?;
            if version.graph == payload.graph {
                return Err(grade_error("active grade graph is unchanged"));
            }
            version.graph = payload.graph;
            Ok(sequence.id)
        })?;
        Ok(())
    }

    fn add_grade_effect(&mut self, payload: GradeAddEffectPayload) -> Result<()> {
        let mut effect = mondrian_effects::instantiate_effect_node(payload.effect_type.clone())
            .map_err(|error| grade_error(error.to_string()))?;
        effect.instantiate_for_clip(format!("Grade · {}", payload.effect_type.display_name()));
        self.commit_active_sequence_edit("添加调色节点", move |sequence| {
            let definition = sequence
                .grade_definitions
                .iter_mut()
                .find(|definition| definition.id == payload.definition_id)
                .ok_or_else(|| grade_error("grade definition does not exist"))?;
            let version = definition
                .versions
                .iter_mut()
                .find(|version| version.id == definition.active_version)
                .ok_or_else(|| grade_error("active grade version does not exist"))?;
            if version.graph.nodes.len() >= mondrian_core::MAX_GRADE_GRAPH_NODES {
                return Err(grade_error("grade graph reached the node limit"));
            }
            let node_id = mondrian_core::GradeGraphNodeId::new();
            let input = version.graph.output;
            version.graph.nodes.push(mondrian_core::GradeGraphNode {
                id: node_id,
                kind: mondrian_core::GradeGraphNodeKind::Effect { input, effect },
            });
            version.graph.output = node_id;
            Ok(sequence.id)
        })?;
        Ok(())
    }
}

pub(super) fn validate_grade_scope_write(sequence: &Sequence, scope: GradeScope) -> Result<()> {
    let GradeScope::Clip(clip_id) = scope else {
        return Ok(());
    };
    let location = sequence
        .clip_track_location(clip_id)
        .ok_or_else(|| grade_error(format!("Clip {clip_id} does not exist")))?;
    if !location.is_video_track {
        return Err(grade_error("Clip grades require a video Clip"));
    }
    if location.is_locked {
        return Err(mondrian_core::MondrianError::TrackLocked {
            track_id: location.track_id.to_string(),
        });
    }
    Ok(())
}

fn grade_error(reason: impl Into<String>) -> mondrian_core::MondrianError {
    mondrian_core::MondrianError::WorkflowStepFailed {
        step_id: "grade_authoring".to_owned(),
        reason: reason.into(),
    }
}
