use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mondrian_app::app::product_action::{
    GradeActivateVersionPayload, GradeAddEffectPayload, GradeAddVersionPayload, GradeAssignPayload,
    GradeCreateDefinitionPayload, GradeCreateGroupPayload, GradeProductAction,
    GradeReplaceActiveGraphPayload, ProductAction, TimelineProductAction, TrackAuthorControl,
    TrackProductAction, TrackSetAuthorControlPayload,
};
use mondrian_app::app::AppState;
use mondrian_app::app_ui::panels::InspectorPanelModel;
use mondrian_core::effect_data::EffectType;
use mondrian_core::{GradeDefinitionId, GradeGraph, GradeGroupId, GradeVersionId, Rational};
use mondrian_editor_state::Action;
use mondrian_timeline::GradeScope;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct FixtureRoot(PathBuf);

impl FixtureRoot {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "mondrian-grade-authoring-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create Grade authoring fixture root");
        Self(path)
    }

    fn project_file(&self) -> PathBuf {
        self.0.join("grade-authoring.mdp")
    }
}

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn round_trip(action: ProductAction) {
    let external = action.clone().into_external_action();
    assert_eq!(
        ProductAction::decode_external(&external)
            .expect("decode Grade product action")
            .expect("Grade product namespace is recognized"),
        action
    );
}

#[test]
fn grade_external_codec_round_trips_every_operation_and_scope() {
    let definition_id = GradeDefinitionId::new();
    let version_id = GradeVersionId::new();
    let clip_id = mondrian_core::ClipId::new();
    let group_id = GradeGroupId::new();

    for scope in [
        GradeScope::Clip(clip_id),
        GradeScope::GroupPre(group_id),
        GradeScope::GroupPost(group_id),
        GradeScope::Timeline,
    ] {
        round_trip(ProductAction::Grade(GradeProductAction::Assign(
            GradeAssignPayload { scope, definition_id: Some(definition_id) },
        )));
    }
    for action in [
        GradeProductAction::CreateDefinition(GradeCreateDefinitionPayload {
            name: "Shared Look".to_owned(),
            assign_to: Some(GradeScope::Clip(clip_id)),
        }),
        GradeProductAction::CreateGroup(GradeCreateGroupPayload {
            name: "Scene".to_owned(),
            clip_id: Some(clip_id),
        }),
        GradeProductAction::AddVersion(GradeAddVersionPayload {
            definition_id,
            name: "Alternate".to_owned(),
            activate: false,
        }),
        GradeProductAction::ActivateVersion(GradeActivateVersionPayload {
            definition_id,
            version_id,
        }),
        GradeProductAction::ReplaceActiveGraph(Box::new(GradeReplaceActiveGraphPayload {
            definition_id,
            graph: GradeGraph::identity(),
        })),
        GradeProductAction::AddEffect(GradeAddEffectPayload {
            definition_id,
            effect_type: EffectType::BasicCorrection,
        }),
    ] {
        round_trip(ProductAction::Grade(action));
    }
}

fn dispatch_product(state: &mut AppState, action: ProductAction) -> mondrian_core::Result<()> {
    state.dispatch_action(action.into_external_action())
}

#[test]
fn grade_authoring_is_atomic_undoable_and_lock_aware() {
    let fixture = FixtureRoot::new();
    let mut state = AppState::new();
    state
        .create_new_project_at(
            fixture.project_file(),
            "Grade Authoring",
            1920,
            1080,
            Rational::new(24, 1),
        )
        .expect("create Grade authoring project");
    dispatch_product(
        &mut state,
        ProductAction::Timeline(TimelineProductAction::CreateBasicTitle),
    )
    .expect("create video Clip fixture");

    let (clip_id, track_id) = {
        let sequence = state.active_sequence().expect("active Sequence");
        let track = sequence
            .video_tracks
            .iter()
            .find(|track| !track.clips.is_empty())
            .expect("video Track containing title");
        (track.clips[0].id, track.id)
    };
    let generation_before = state.project_author_generation();
    let revision_before = state.active_sequence().expect("Sequence").revision;
    let history_before = state.authoring_history().expect("History").diagnostics().undo_entries;
    let create_model = InspectorPanelModel::from_app_state(&state);
    let create_action = create_model
        .grade
        .create_clip_grade_action
        .expect("Inspector admits Clip Grade creation");
    assert_eq!(
        ProductAction::decode_external(&create_action)
            .expect("decode Inspector Grade action")
            .expect("recognized Inspector Grade action"),
        ProductAction::Grade(GradeProductAction::CreateDefinition(
            GradeCreateDefinitionPayload {
                name: "Clip Grade".to_owned(),
                assign_to: Some(GradeScope::Clip(clip_id)),
            }
        ))
    );

    dispatch_product(
        &mut state,
        ProductAction::Grade(GradeProductAction::CreateDefinition(
            GradeCreateDefinitionPayload {
                name: "Hero Look".to_owned(),
                assign_to: Some(GradeScope::Clip(clip_id)),
            },
        )),
    )
    .expect("create and assign Clip Grade");

    let definition_id = {
        let sequence = state.active_sequence().expect("Sequence");
        assert_eq!(sequence.grade_definitions.len(), 1);
        assert_eq!(
            sequence.find_clip(clip_id).expect("Clip").grade,
            Some(sequence.grade_definitions[0].id)
        );
        sequence.grade_definitions[0].id
    };
    assert_eq!(state.project_author_generation(), generation_before + 1);
    assert_eq!(
        state.active_sequence().expect("Sequence").revision.get(),
        revision_before.get() + 1
    );
    assert_eq!(
        state.authoring_history().expect("History").diagnostics().undo_entries,
        history_before + 1,
        "definition creation and Clip assignment must be one transaction"
    );

    dispatch_product(
        &mut state,
        ProductAction::Grade(GradeProductAction::AddEffect(GradeAddEffectPayload {
            definition_id,
            effect_type: EffectType::BasicCorrection,
        })),
    )
    .expect("append Grade Effect");
    assert_eq!(
        state
            .active_sequence()
            .expect("Sequence")
            .grade_definition(definition_id)
            .expect("definition")
            .active()
            .expect("active version")
            .graph
            .nodes
            .len(),
        2
    );

    state.dispatch_action(Action::Undo).expect("undo Grade Effect");
    assert_eq!(
        state
            .active_sequence()
            .expect("Sequence")
            .grade_definition(definition_id)
            .expect("definition")
            .active()
            .expect("active version")
            .graph
            .nodes
            .len(),
        1
    );
    state.dispatch_action(Action::Redo).expect("redo Grade Effect");
    assert_eq!(
        state
            .active_sequence()
            .expect("Sequence")
            .grade_definition(definition_id)
            .expect("definition")
            .active()
            .expect("active version")
            .graph
            .nodes
            .len(),
        2
    );

    dispatch_product(
        &mut state,
        ProductAction::Grade(GradeProductAction::AddVersion(GradeAddVersionPayload {
            definition_id,
            name: "Version 2".to_owned(),
            activate: false,
        })),
    )
    .expect("add Grade version");
    let version_id = state
        .active_sequence()
        .expect("Sequence")
        .grade_definition(definition_id)
        .expect("definition")
        .versions[1]
        .id;
    dispatch_product(
        &mut state,
        ProductAction::Grade(GradeProductAction::ActivateVersion(
            GradeActivateVersionPayload { definition_id, version_id },
        )),
    )
    .expect("activate Grade version");
    dispatch_product(
        &mut state,
        ProductAction::Grade(GradeProductAction::ReplaceActiveGraph(Box::new(
            GradeReplaceActiveGraphPayload { definition_id, graph: GradeGraph::identity() },
        ))),
    )
    .expect("replace active Grade Graph");
    dispatch_product(
        &mut state,
        ProductAction::Grade(GradeProductAction::CreateGroup(GradeCreateGroupPayload {
            name: "Scene".to_owned(),
            clip_id: Some(clip_id),
        })),
    )
    .expect("create and attach Grade group");
    let group_id = state.active_sequence().expect("Sequence").grade_groups[0].id;
    dispatch_product(
        &mut state,
        ProductAction::Grade(GradeProductAction::Assign(GradeAssignPayload {
            scope: GradeScope::GroupPre(group_id),
            definition_id: Some(definition_id),
        })),
    )
    .expect("assign Group Pre Grade");
    dispatch_product(
        &mut state,
        ProductAction::Grade(GradeProductAction::Assign(GradeAssignPayload {
            scope: GradeScope::Timeline,
            definition_id: Some(definition_id),
        })),
    )
    .expect("assign Timeline Grade");
    let hierarchy = InspectorPanelModel::from_app_state(&state).grade;
    assert_eq!(hierarchy.clip_definition_id, Some(definition_id));
    assert_eq!(hierarchy.clip_grade.as_deref(), Some("Hero Look"));
    assert_eq!(hierarchy.group.as_deref(), Some("Scene"));
    assert_eq!(hierarchy.group_pre_grade.as_deref(), Some("Hero Look"));
    assert_eq!(hierarchy.group_post_grade, None);
    assert_eq!(hierarchy.timeline_grade.as_deref(), Some("Hero Look"));
    assert_eq!(hierarchy.active_version.as_deref(), Some("Version 2"));
    assert_eq!(hierarchy.version_count, 2);
    assert_eq!(hierarchy.node_count, 1);
    assert!(hierarchy.create_clip_grade_action.is_none());
    assert!(hierarchy.add_node_actions.iter().any(|item| {
        ProductAction::decode_external(&item.action)
            .ok()
            .flatten()
            .is_some_and(|action| {
                action
                    == ProductAction::Grade(GradeProductAction::AddEffect(GradeAddEffectPayload {
                        definition_id,
                        effect_type: EffectType::Curves,
                    }))
            })
    }));

    dispatch_product(
        &mut state,
        ProductAction::Track(TrackProductAction::SetAuthorControl(
            TrackSetAuthorControlPayload {
                track_id,
                control: TrackAuthorControl::Lock,
                enabled: true,
            },
        )),
    )
    .expect("lock video Track");
    let assign_locked = ProductAction::Grade(GradeProductAction::Assign(GradeAssignPayload {
        scope: GradeScope::Clip(clip_id),
        definition_id: None,
    }));
    let create_locked = ProductAction::Grade(GradeProductAction::CreateDefinition(
        GradeCreateDefinitionPayload {
            name: "Rejected".to_owned(),
            assign_to: Some(GradeScope::Clip(clip_id)),
        },
    ));
    assert!(!state.product_action_availability().allows(&assign_locked));
    assert!(!state.product_action_availability().allows(&create_locked));
    let generation_locked = state.project_author_generation();
    let revision_locked = state.active_sequence().expect("Sequence").revision;
    let definitions_locked = state.active_sequence().expect("Sequence").grade_definitions.len();
    assert!(dispatch_product(&mut state, assign_locked).is_err());
    assert!(dispatch_product(&mut state, create_locked).is_err());
    assert_eq!(state.project_author_generation(), generation_locked);
    assert_eq!(
        state.active_sequence().expect("Sequence").revision,
        revision_locked
    );
    assert_eq!(
        state.active_sequence().expect("Sequence").grade_definitions.len(),
        definitions_locked
    );
    assert!(
        state.product_action_availability().allows(&ProductAction::Grade(
            GradeProductAction::AddEffect(GradeAddEffectPayload {
                definition_id,
                effect_type: EffectType::Curves,
            })
        ))
    );
    assert!(
        !InspectorPanelModel::from_app_state(&state).grade.add_node_actions.is_empty(),
        "Track lock cannot freeze Sequence-level Shared Definition editing"
    );

    state.close_project().expect("close Grade authoring project");
}
