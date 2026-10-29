use super::*;
use mondrian_core::{
    automation::PropertyDescriptor, GradeGraph, GradeGraphNode, GradeGraphNodeKind, ParameterId,
    SampleAspectRatio,
};
use mondrian_effects::{
    EffectColorDomainContract, EffectDefinition, EffectExecutionContract, EffectExecutionModes,
    EffectFrameContext, EffectProjectGeometry, LutPreparationCache, PreparedEffectProgram,
};

fn kind(key: &str) -> EffectType {
    mondrian_effects::register_effect_definition(
        EffectDefinition::new(
            key,
            key,
            Default::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_graph_builder(Arc::new(|_, _, _| Ok(())))
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: mondrian_effects::EffectDeterminism::Deterministic,
            state_model: mondrian_effects::EffectStateModel::Stateless,
            temporal_input: mondrian_effects::EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: mondrian_effects::EffectRoiPropagation::UnknownRequiresFullFrame,
            resource_lifetime: mondrian_effects::EffectResourceLifetime::Frame,
            topology: mondrian_effects::EffectGraphTopology::LinearChain,
        })
        .with_project_properties(
            Arc::new(|geometry, properties| {
                properties.define(
                    PropertyDescriptor::new(
                        "size",
                        "Size",
                        PropertyValue::DoubleVec2(geometry.canonical_size()),
                    )
                    .with_parameter_id(ParameterId::new("test.app.geometry.size").unwrap()),
                );
                Ok(())
            }),
            0,
        ),
    )
    .unwrap();
    EffectType::External(key.into())
}
fn effect(kind: EffectType) -> mondrian_effects::EffectNode {
    mondrian_effects::instantiate_effect_node_with_project_geometry(
        kind,
        EffectProjectGeometry::new(16, 16, SampleAspectRatio::SQUARE).unwrap(),
    )
    .unwrap()
}
fn sequence() -> Sequence {
    let mut s = Sequence::new("Owner");
    s.settings.resolution = Resolution { width: 16, height: 16 };
    s
}
fn graph(effect: mondrian_effects::EffectNode) -> GradeGraph {
    let mut graph = GradeGraph::identity();
    let id = mondrian_core::GradeGraphNodeId::new();
    graph.nodes.push(GradeGraphNode {
        id,
        kind: GradeGraphNodeKind::Effect { input: graph.output, effect },
    });
    graph.output = id;
    graph
}
fn properties(sequence: &Sequence) -> Vec<mondrian_effects::EffectNode> {
    sequence
        .video_tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .flat_map(|c| c.effects.iter())
        .cloned()
        .chain(
            sequence
                .grade_definitions
                .iter()
                .flat_map(|d| d.versions.iter())
                .flat_map(|v| v.graph.nodes.iter())
                .filter_map(|n| match &n.kind {
                    GradeGraphNodeKind::Effect { effect, .. } => Some(effect.clone()),
                    _ => None,
                }),
        )
        .collect()
}

#[test]
fn public_geometry_edit_rebinds_all_owned_defaults_and_history_without_editing_child_sequences() {
    let _guard = crate::app_ui::test_utils::theme_test_guard();
    let kind = kind("test.app.geometry.owned");
    let mut child = sequence();
    child.name = "Child".into();
    let mut child_clip =
        Clip::new_basic_title("Child", "Arial", TimelineTime::ZERO, TimelineTime::ONE).unwrap();
    child_clip.add_effect_node(effect(kind.clone()));
    child.video_tracks[0].add_clip(child_clip).unwrap();
    let mut owner = sequence();
    let mut clip =
        Clip::new_nested_sequence(child.id, TimelineTime::ZERO, TimelineTime::ONE, None).unwrap();
    let mut animated = effect(kind.clone());
    let mut property = animated.properties.iter().next().unwrap().1.clone();
    property.set_static_value(PropertyValue::DoubleVec2([1.0, 1.0])).unwrap();
    property.set_animation_enabled(true);
    property
        .write_channels(
            TimelineTime::ZERO,
            &[(0, 1.0), (1, 1.0)],
            InterpolationType::Linear,
        )
        .unwrap();
    property
        .write_channels(
            TimelineTime::ONE,
            &[(0, 2.0), (1, 3.0)],
            InterpolationType::Linear,
        )
        .unwrap();
    animated.properties.upsert(property);
    clip.add_effect_node(animated);
    clip.is_disabled = true;
    owner.video_tracks[0].add_clip(clip).unwrap();
    let id = owner.add_grade_definition("Grade");
    owner.grade_definitions[0].versions[0].graph = graph(effect(kind.clone()));
    owner.add_grade_version(id, "Inactive", graph(effect(kind))).unwrap();
    let owner_id = owner.id;
    let child_id = child.id;
    let mut state = AppState::new();
    state.test_set_sequences(vec![owner, child]);
    state.test_set_active_sequence(owner_id);
    let before = state.active_sequence().unwrap().clone();
    let child = state.sequences().iter().find(|s| s.id == child_id).unwrap().clone();
    let before_effects = properties(&before);
    assert_eq!(before_effects.len(), 3);
    let mut settings = before.settings.clone();
    settings.resolution = Resolution { width: 32, height: 24 };
    settings.pixel_aspect_ratio = mondrian_timeline::sequence::PixelAspectRatio::Anamorphic2x;
    let generation = state.project_author_generation();
    state
        .update_active_sequence_settings(settings.clone())
        .expect("atomic contextual migration");
    assert_eq!(state.project_author_generation(), generation + 1);
    let after = state.active_sequence().unwrap().clone();
    for (old, new) in before_effects.iter().zip(properties(&after)) {
        let mut expected = old.clone();
        let mut p = expected.properties.iter().next().unwrap().1.clone();
        p.descriptor.schema.default_value = PropertyValue::DoubleVec2([64.0, 24.0]);
        expected.properties.upsert(p);
        expected.params = new.params.clone();
        assert_eq!(
            new, expected,
            "values, keys, paths, disabled state and identities survive"
        );
        let context = EffectFrameContext::new(
            settings.frame_rate,
            EffectProjectGeometry::new(32, 24, SampleAspectRatio::new(2, 1).unwrap()).unwrap(),
            mondrian_core::TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
        )
        .unwrap();
        PreparedEffectProgram::prepare_hierarchical_with_frame_context(
            &[new],
            &[],
            &[],
            &[],
            settings.color.working_color_space,
            context,
            &LutPreparationCache::uncached(),
        )
        .unwrap();
    }
    assert_eq!(
        state.sequences().iter().find(|s| s.id == child_id).unwrap(),
        &child
    );
    assert!(state.undo_timeline().unwrap());
    assert_eq!(state.active_sequence().unwrap().settings, before.settings);
    assert_eq!(properties(state.active_sequence().unwrap()), before_effects);
    assert!(state.redo_timeline().unwrap());
    assert_eq!(state.active_sequence().unwrap().settings, after.settings);
    assert_eq!(
        properties(state.active_sequence().unwrap()),
        properties(&after)
    );
    state
        .update_sequence_identity_and_settings(owner_id, "Renamed", before.settings.clone())
        .unwrap();
    assert_eq!(state.active_sequence().unwrap().name, "Renamed");
    assert_eq!(properties(state.active_sequence().unwrap()), before_effects);
    assert_eq!(
        state.sequences().iter().find(|s| s.id == child_id).unwrap(),
        &child
    );
}

#[test]
fn public_geometry_edit_refuses_unavailable_context_without_publishing_partial_changes() {
    let _guard = crate::app_ui::test_utils::theme_test_guard();
    let kind = kind("test.app.geometry.atomic");
    let mut owner = sequence();
    let mut clip =
        Clip::new_basic_title("Title", "Arial", TimelineTime::ZERO, TimelineTime::ONE).unwrap();
    clip.add_effect_node(effect(kind.clone()));
    let mut missing = effect(kind);
    missing.effect_type = EffectType::External("test.app.geometry.unavailable".into());
    clip.add_effect_node(missing);
    owner.video_tracks[0].add_clip(clip).unwrap();
    let mut state = AppState::new();
    state.test_set_sequence(Some(owner));
    let before = state.active_sequence().unwrap().clone();
    let generation = state.project_author_generation();
    let mut settings = before.settings.clone();
    settings.resolution = Resolution { width: 32, height: 24 };
    assert!(state.update_active_sequence_settings(settings.clone()).is_err());
    assert_eq!(state.active_sequence().unwrap(), &before);
    assert_eq!(state.project_author_generation(), generation);
    assert!(!state.can_undo_action());
    assert!(state
        .update_sequence_identity_and_settings(before.id, "Rejected", settings)
        .is_err());
    assert_eq!(state.active_sequence().unwrap(), &before);
    assert_eq!(state.project_author_generation(), generation);
    assert!(!state.can_undo_action());
}
