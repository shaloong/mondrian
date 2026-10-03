use mondrian_core::automation::{
    InterpolationType, Keyframe, ParameterInvalidValuePolicy, ParameterNumericContract,
    ParameterUnit, PropertyDescriptor, PropertyHost, PropertyMutation, PropertyValue,
    PropertyValueType,
};
use mondrian_core::effect_data::{EffectNode, EffectType};
use mondrian_core::{AssetId, ParameterId, TimelineTime};
use mondrian_project::{read_project_document_from_archive, save_project_archive, ProjectDocument};
use mondrian_timeline::Clip;

#[test]
fn asymmetric_channel_contracts_and_keys_survive_a_durable_archive_reopen() {
    let vector = |x, y| {
        PropertyValue::from_channel_values(
            PropertyValueType::Vec2,
            &[x, y],
            &PropertyValue::Float(0.0),
        )
    };
    let mut document: ProjectDocument =
        serde_json::from_str(include_str!("fixtures/current/project.json"))
            .expect("current canonical fixture");
    let parameter_id = ParameterId::new_static("test.channel_contracts.point");
    let mut effect = EffectNode::new(EffectType::Plugin("test.channel_contracts".to_owned()));
    let path = "point".to_owned();
    effect.define_property(
        PropertyDescriptor::new(path.clone(), "Point", vector(0.25, 20.0))
            .with_parameter_id(parameter_id.clone())
            .with_channel_numeric_contracts(
                ParameterUnit::Unitless,
                vec![
                    ParameterNumericContract::closed(
                        0.0,
                        1.0,
                        Some(0.01),
                        ParameterInvalidValuePolicy::Clamp,
                    )
                    .expect("X"),
                    ParameterNumericContract::closed(
                        10.0,
                        30.0,
                        Some(0.1),
                        ParameterInvalidValuePolicy::Reject,
                    )
                    .expect("Y"),
                ],
            ),
    );
    for (time, value) in [
        (TimelineTime::ZERO, vector(0.25, 20.0)),
        (TimelineTime::ONE, vector(0.75, 30.0)),
    ] {
        effect
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: path.clone(),
                keyframe: Keyframe::linear(time, value),
            })
            .expect("atomic complete key");
    }
    let mut clip = Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).expect("clip");
    clip.add_effect_node(effect);
    let expected = clip.effects[0].clone();
    document.sequences.active_mut().expect("sequence").video_tracks[0]
        .add_clip(clip)
        .expect("clip admission");
    let root = std::env::temp_dir().join(format!("mondrian-channel-contracts-{}", AssetId::new()));
    std::fs::create_dir_all(&root).expect("owned directory");
    let database = root.join("index.db");
    std::fs::write(&database, b"sqlite placeholder").expect("library fixture");
    let archive = root.join("channels.mdp");
    save_project_archive(&document, &database, &archive).expect("durable publication");
    let reopened = read_project_document_from_archive(&archive).expect("fresh archive reopen");
    reopened.validate().expect("reopened canonical contracts");
    let restored =
        &reopened.sequences.active().expect("sequence").video_tracks[0].clips[0].effects[0];
    assert_eq!(
        restored, &expected,
        "channel ranges, policies, identities and exact keys survive"
    );
    let midpoint = TimelineTime::new(1, 2).expect("midpoint");
    assert_eq!(
        restored.evaluate_parameter(&parameter_id, midpoint),
        Some(vector(0.5, 25.0))
    );
    let mut edited = restored.clone();
    let before = edited.clone();
    let authored_path = edited
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.parameter_id() == &parameter_id)
        .expect("persisted parameter address")
        .0
        .to_owned();
    let error = edited
        .properties
        .write_channels(
            &authored_path,
            midpoint,
            &[(0, -1.0), (1, 31.0)],
            InterpolationType::Linear,
        )
        .expect_err("Y rejects the whole update");
    assert!(error.to_string().contains("outside [10, 30]"), "{error}");
    assert_eq!(
        edited, before,
        "reopened mixed clamp/reject policies remain atomic"
    );
    std::fs::remove_dir_all(&root).expect("remove owned fixture");
}
