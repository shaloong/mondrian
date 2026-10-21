//! Bind author resource input to its explicit Project directory before commit.

use super::AppState;
use mondrian_core::automation::ParameterValueWrite;
use mondrian_core::{MondrianError, ParameterResourceReference, PropertyValue, Result};
use std::borrow::Cow;

impl AppState {
    /// Bind incoming graph values without changing definition defaults or author identities.
    /// Resource parameters cannot carry animation, as enforced by graph validation.
    pub(crate) fn resolve_grade_graph_resources(
        &self,
        graph: &mut mondrian_core::GradeGraph,
    ) -> Result<()> {
        graph.validate_author_state()?;
        for node in &mut graph.nodes {
            let mondrian_core::GradeGraphNodeKind::Effect { effect, .. } = &mut node.kind else {
                continue;
            };
            self.resolve_effect_resources(effect)?;
        }
        graph.validate_author_state()
    }

    /// Bind an incoming Effect instance's values before it enters canonical author state.
    /// Provider defaults remain templates; only static instance values are changed.
    pub(crate) fn resolve_effect_resources(
        &self,
        effect: &mut mondrian_core::effect_data::EffectNode,
    ) -> Result<()> {
        effect.validate_author_state()?;
        let mut writes = Vec::new();
        for (path, property) in effect.properties.iter() {
            if !matches!(property.static_value(), PropertyValue::Resource(
                ParameterResourceReference::ExternalFile { path }) if !path.is_absolute())
            {
                continue;
            }
            let write = ParameterValueWrite::Value {
                parameter: property.address(),
                value: property.static_value().clone(),
            };
            if let Cow::Owned(ParameterValueWrite::Value { value, .. }) =
                self.resolve_resource_parameter_write(&write)?
            {
                writes.push((path.to_owned(), value));
            }
        }
        for (path, value) in writes {
            effect.properties.set_static_value(&path, value)?;
        }
        Ok(())
    }

    /// Resolve complete external resource writes before schema admission.
    /// Already-bound resources and non-resource/channel edits allocate nothing.
    pub(crate) fn resolve_resource_parameter_write<'a>(
        &self,
        write: &'a ParameterValueWrite,
    ) -> Result<Cow<'a, ParameterValueWrite>> {
        let ParameterValueWrite::Value {
            parameter,
            value:
                PropertyValue::Resource(reference @ ParameterResourceReference::ExternalFile { path }),
        } = write
        else {
            return Ok(Cow::Borrowed(write));
        };
        if path.is_absolute() {
            return Ok(Cow::Borrowed(write));
        }
        let directory =
            self.current_project_path().and_then(std::path::Path::parent).ok_or_else(|| {
                MondrianError::WorkflowStepFailed {
                    step_id: "resource_path_owner".into(),
                    reason: "relative resources require an open Project directory".into(),
                }
            })?;
        let mut reference = reference.clone();
        reference.resolve_external_path(directory).map_err(|error| {
            MondrianError::WorkflowStepFailed {
                step_id: "resource_path_owner".into(),
                reason: error.to_string(),
            }
        })?;
        Ok(Cow::Owned(ParameterValueWrite::Value {
            parameter: parameter.clone(),
            value: PropertyValue::Resource(reference),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::{
        GradeAddEffectPayload, GradeCreateDefinitionPayload, GradeProductAction, ProductAction,
        TimelineProductAction,
    };
    use mondrian_core::{
        effect_data::EffectType, ParameterResourceContract, PropertyDescriptor, Rational,
        TimelineTime,
    };
    use mondrian_editor_state::Action;

    fn resource_definition(
        path: std::path::PathBuf,
        contract: ParameterResourceContract,
    ) -> EffectType {
        use mondrian_effects::*;
        let kind = EffectType::External(format!("test.resource.defaults.{}", uuid::Uuid::new_v4()));
        let definition = EffectExtensionDefinitionBuilder::new(
            kind.key(),
            "Resource defaults",
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        })
        .with_branching_graph(|_, _, graph| {
            graph.apply(EffectRenderOp::Sharpen { amount: 0.0 });
        })
        .property(
            PropertyDescriptor::new(
                "path",
                "Path",
                PropertyValue::Resource(ParameterResourceReference::ExternalFile {
                    path: path.into(),
                }),
            )
            .with_parameter_id(kind.parameter_id("path").expect("parameter"))
            .with_resource_contract(contract),
        )
        .build();
        register_effect_definition(definition).expect("definition");
        kind
    }

    #[test]
    fn relative_effect_defaults_are_owned_by_each_project_without_changing_templates() {
        let relative = std::path::PathBuf::from("resources").join("默认🙂.cube");
        let kind = resource_definition(
            relative.clone(),
            ParameterResourceContract::external_file(Vec::new()),
        );
        let path_id = kind.parameter_id("path").expect("parameter");
        let template = mondrian_effects::instantiate_effect_node(kind.clone()).expect("template");
        let original = template.evaluate_parameter(&path_id, TimelineTime::ZERO);
        let first = tempfile::tempdir().expect("first owner");
        let second = tempfile::tempdir().expect("second owner");
        for directory in [first.path(), second.path()] {
            let project = directory.join("owner.mdp");
            let mut state = AppState::new();
            state
                .create_new_project_at(project.clone(), "Owner", 16, 16, Rational::new(24, 1))
                .expect("Project");
            state
                .dispatch_action(
                    ProductAction::Timeline(TimelineProductAction::CreateBasicTitle)
                        .into_external_action(),
                )
                .expect("title");
            let clip_id = state
                .active_sequence()
                .expect("Sequence")
                .video_tracks
                .iter()
                .find_map(|track| track.clips.first().map(|clip| clip.id))
                .expect("title Clip");
            let history = state.authoring_history().expect("history").diagnostics().undo_entries;
            let id = state.add_effect_to_clip(clip_id, kind.clone()).expect("add Clip effect");
            let bound = Some(PropertyValue::Resource(
                ParameterResourceReference::ExternalFile { path: directory.join(&relative).into() },
            ));
            let clip_resource = |state: &AppState| {
                state
                    .active_sequence()
                    .expect("Sequence")
                    .find_clip(clip_id)
                    .expect("Clip")
                    .effects
                    .iter()
                    .find(|effect| effect.id == id)
                    .expect("effect")
                    .evaluate_parameter(&path_id, TimelineTime::ZERO)
            };
            assert_eq!(clip_resource(&state), bound);
            assert_eq!(
                state.authoring_history().expect("history").diagnostics().undo_entries,
                history + 1
            );
            state.dispatch_action(Action::Undo).expect("Undo");
            assert!(state
                .active_sequence()
                .expect("Sequence")
                .find_clip(clip_id)
                .expect("Clip")
                .effects
                .is_empty());
            state.dispatch_action(Action::Redo).expect("Redo");
            assert_eq!(clip_resource(&state), bound);
            state
                .dispatch_action(
                    ProductAction::Grade(GradeProductAction::CreateDefinition(
                        GradeCreateDefinitionPayload { name: "Look".into(), assign_to: None },
                    ))
                    .into_external_action(),
                )
                .expect("Grade");
            let definition_id = state.active_sequence().expect("Sequence").grade_definitions[0].id;
            let original_graph = state
                .active_sequence()
                .expect("Sequence")
                .grade_definition(definition_id)
                .expect("Grade")
                .active()
                .expect("version")
                .graph
                .clone();
            let grade_history =
                state.authoring_history().expect("history").diagnostics().undo_entries;
            state
                .dispatch_action(
                    ProductAction::Grade(GradeProductAction::AddEffect(GradeAddEffectPayload {
                        definition_id,
                        effect_type: kind.clone(),
                    }))
                    .into_external_action(),
                )
                .expect("Grade effect");
            let graph = state
                .active_sequence()
                .expect("Sequence")
                .grade_definition(definition_id)
                .expect("Grade")
                .active()
                .expect("version")
                .graph
                .clone();
            let mondrian_core::GradeGraphNodeKind::Effect { effect, .. } = &graph.nodes[1].kind
            else {
                panic!("Grade effect");
            };
            assert_eq!(
                effect.evaluate_parameter(&path_id, TimelineTime::ZERO),
                bound
            );
            assert_eq!(
                state.authoring_history().expect("history").diagnostics().undo_entries,
                grade_history + 1
            );
            state.dispatch_action(Action::Undo).expect("Undo Grade default binding");
            assert_eq!(
                state
                    .active_sequence()
                    .expect("Sequence")
                    .grade_definition(definition_id)
                    .expect("Grade")
                    .active()
                    .expect("version")
                    .graph,
                original_graph
            );
            state.dispatch_action(Action::Redo).expect("Redo Grade default binding");
            assert_eq!(
                state
                    .active_sequence()
                    .expect("Sequence")
                    .grade_definition(definition_id)
                    .expect("Grade")
                    .active()
                    .expect("version")
                    .graph,
                graph
            );
            let save = state.request_project_save().expect("save");
            state.wait_for_persistence_request(save).expect("durable save");
            state.close_project().expect("close");
            let mut reopened = AppState::new();
            reopened.open_project_file(project).expect("fresh owner reopen");
            assert_eq!(clip_resource(&reopened), bound);
            assert_eq!(
                reopened
                    .active_sequence()
                    .expect("Sequence")
                    .grade_definition(definition_id)
                    .expect("Grade")
                    .active()
                    .expect("version")
                    .graph,
                graph
            );
            reopened.close_project().expect("release");
            assert!(
                !directory.join("resources").exists(),
                "binding creates no external objects"
            );
            assert_eq!(
                mondrian_effects::instantiate_effect_node(kind.clone())
                    .expect("fresh template")
                    .evaluate_parameter(&path_id, TimelineTime::ZERO),
                original
            );
        }
    }

    #[test]
    fn effect_default_binding_failure_rejects_clip_and_grade_transactions() {
        use mondrian_core::{ResourcePathKind, ResourcePathSelection};
        let directory = tempfile::tempdir().expect("owner");
        let mut state = AppState::new();
        state
            .create_new_project_at(
                directory.path().join("owner.mdp"),
                "Owner",
                16,
                16,
                Rational::new(24, 1),
            )
            .expect("Project");
        state
            .dispatch_action(
                ProductAction::Timeline(TimelineProductAction::CreateBasicTitle)
                    .into_external_action(),
            )
            .expect("title");
        let clip_id = state
            .active_sequence()
            .expect("Sequence")
            .video_tracks
            .iter()
            .find_map(|track| track.clips.first().map(|clip| clip.id))
            .expect("title Clip");
        let relative = std::path::PathBuf::from("output.cube");
        let kind = resource_definition(
            relative,
            ParameterResourceContract::external_utf8_path(
                ResourcePathKind::File,
                ResourcePathSelection::ExistingOrNew,
                "output.cube".len(),
            ),
        );
        let history = state.authoring_history().expect("history").diagnostics();
        let generation = state.project_author_generation();
        assert!(
            state.add_effect_to_clip(clip_id, kind.clone()).is_err(),
            "the raw default fits but its owned absolute path exceeds the provider limit"
        );
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(
            state.authoring_history().expect("history").diagnostics(),
            history
        );
        assert!(state
            .active_sequence()
            .expect("Sequence")
            .find_clip(clip_id)
            .expect("Clip")
            .effects
            .is_empty());
        state
            .dispatch_action(
                ProductAction::Grade(GradeProductAction::CreateDefinition(
                    GradeCreateDefinitionPayload { name: "Look".into(), assign_to: None },
                ))
                .into_external_action(),
            )
            .expect("Grade");
        let definition_id = state.active_sequence().expect("Sequence").grade_definitions[0].id;
        let graph = state
            .active_sequence()
            .expect("Sequence")
            .grade_definition(definition_id)
            .expect("Grade")
            .active()
            .expect("version")
            .graph
            .clone();
        let history = state.authoring_history().expect("history").diagnostics();
        let generation = state.project_author_generation();
        assert!(state
            .dispatch_action(
                ProductAction::Grade(GradeProductAction::AddEffect(GradeAddEffectPayload {
                    definition_id,
                    effect_type: kind
                }))
                .into_external_action()
            )
            .is_err());
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(
            state.authoring_history().expect("history").diagnostics(),
            history
        );
        assert_eq!(
            state
                .active_sequence()
                .expect("Sequence")
                .grade_definition(definition_id)
                .expect("Grade")
                .active()
                .expect("version")
                .graph,
            graph
        );
        assert!(!directory.path().join("output.cube").exists());
        state.close_project().expect("release");
    }
}
