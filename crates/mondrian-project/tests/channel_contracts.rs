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
fn world_bezier_equal_endpoint_excursion_survives_durable_archive_reopen() {
    use mondrian_core::automation::{ExactBezierHandle, KeyframeInterpolation};
    let parameter_id = ParameterId::new_static("test.archive.world_bezier");
    let mut document: ProjectDocument =
        serde_json::from_str(include_str!("fixtures/current/project.json"))
            .expect("canonical fixture");
    let mut effect = EffectNode::new(EffectType::Plugin("test.archive.world_bezier".to_owned()));
    effect.define_property(
        PropertyDescriptor::new("amount", "Amount", PropertyValue::Double(2.0))
            .with_parameter_id(parameter_id.clone())
            .with_numeric_contract(
                ParameterUnit::Unitless,
                ParameterNumericContract::closed(
                    0.0,
                    10.0,
                    None,
                    ParameterInvalidValuePolicy::Reject,
                )
                .expect("range"),
            ),
    );
    let mut left = Keyframe::linear(
        TimelineTime::new(2, 1).expect("left"),
        PropertyValue::Double(2.0),
    );
    left.interp_out = KeyframeInterpolation::Bezier(ExactBezierHandle {
        time_offset: TimelineTime::new(2, 1).expect("two seconds"),
        value_offset: 4.0,
    });
    let mut right = Keyframe::linear(
        TimelineTime::new(8, 1).expect("right"),
        PropertyValue::Double(2.0),
    );
    right.interp_in = KeyframeInterpolation::Bezier(ExactBezierHandle {
        time_offset: TimelineTime::new(-2, 1).expect("minus two seconds"),
        value_offset: 4.0,
    });
    for keyframe in [left, right] {
        effect
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: "amount".to_owned(),
                keyframe,
            })
            .expect("world author coordinates");
    }
    let mut clip = Clip::new(
        AssetId::new(),
        TimelineTime::ZERO,
        TimelineTime::new(9, 1).expect("nine seconds"),
    )
    .expect("clip");
    clip.add_effect_node(effect);
    let expected = clip.effects[0].clone();
    document.sequences.active_mut().expect("sequence").video_tracks[0]
        .add_clip(clip)
        .expect("clip admission");
    let root = std::env::temp_dir().join(format!("mondrian-world-曲线-{}", AssetId::new()));
    std::fs::create_dir_all(&root).expect("owned fixture");
    let database = root.join("index.db");
    std::fs::write(&database, b"sqlite placeholder").expect("opaque library fixture");
    let archive = root.join("世界坐标.mdp");
    save_project_archive(&document, &database, &archive).expect("durable save");
    let reopened = read_project_document_from_archive(&archive).expect("fresh reopen");
    reopened.validate().expect("canonical world coordinates");
    let actual =
        &reopened.sequences.active().expect("sequence").video_tracks[0].clips[0].effects[0];
    assert_eq!(
        actual, &expected,
        "exact controls and key identities survive"
    );
    for sample in 0_i32..=24 {
        let time = TimelineTime::new(i64::from(8 + sample), 4).expect("quarter seconds");
        let t = f64::from(sample) / 24.0;
        // Independent Bernstein polynomial with control values [2,6,6,2].
        let expected_value = 2.0 + 12.0 * t * (1.0 - t);
        let Some(PropertyValue::Double(value)) = actual.evaluate_parameter(&parameter_id, time)
        else {
            panic!("Double parameter");
        };
        assert!(
            (value - expected_value).abs() <= 16.0 * f64::EPSILON,
            "sample {sample}: {value} versus {expected_value}"
        );
    }
    std::fs::remove_dir_all(root).expect("remove owned fixture");
}

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

#[test]
fn precise_compound_types_defaults_and_keys_survive_durable_archive_reopen() {
    let precise = 0.12500000000000003;
    let cases = [
        PropertyValue::DoubleVec2([precise; 2]),
        PropertyValue::DoubleVec3([precise; 3]),
        PropertyValue::DoubleRgb([precise; 3]),
        PropertyValue::DoubleColor([precise; 4]),
        PropertyValue::IntVec2([i32::MIN, i32::MAX]),
        PropertyValue::IntVec3([i32::MIN, 0, i32::MAX]),
    ];
    let mut document: ProjectDocument =
        serde_json::from_str(include_str!("fixtures/current/project.json"))
            .expect("current canonical fixture");
    let mut effect = EffectNode::new(EffectType::Plugin("test.precise.compound".to_owned()));
    for (index, value) in cases.into_iter().enumerate() {
        let path = format!("component{index}");
        let descriptor = PropertyDescriptor::new(path.clone(), path.clone(), value.clone());
        effect.define_property(descriptor);
        effect
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path,
                keyframe: Keyframe::from_preset(TimelineTime::ZERO, value, InterpolationType::Hold),
            })
            .expect("complete precise key");
    }
    let mut clip = Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).expect("clip");
    clip.add_effect_node(effect);
    let expected = clip.effects[0].clone();
    document.sequences.active_mut().expect("sequence").video_tracks[0]
        .add_clip(clip)
        .expect("clip admission");
    let root = std::env::temp_dir().join(format!("mondrian-precise-compounds-{}", AssetId::new()));
    std::fs::create_dir_all(&root).expect("owned directory");
    let database = root.join("index.db");
    std::fs::write(&database, b"sqlite placeholder").expect("library fixture");
    let archive = root.join("precise.mdp");
    save_project_archive(&document, &database, &archive).expect("durable publication");
    let reopened = read_project_document_from_archive(&archive).expect("fresh archive reopen");
    reopened.validate().expect("canonical precise values");
    let actual =
        &reopened.sequences.active().expect("sequence").video_tracks[0].clips[0].effects[0];
    assert_eq!(
        actual, &expected,
        "all six types, component precision, defaults and keys survive"
    );
    std::fs::remove_dir_all(&root).expect("remove owned fixture");
}

#[test]
fn invalid_visual_handle_document_cannot_replace_a_durable_archive() {
    use mondrian_core::automation::{ExactBezierHandle, KeyframeInterpolation};
    let parameter_id = ParameterId::new_static("test.archive.visual.handle");
    let mut document: ProjectDocument =
        serde_json::from_str(include_str!("fixtures/current/project.json"))
            .expect("current canonical fixture");
    let mut effect = EffectNode::new(EffectType::Plugin("test.archive.visual.handle".to_owned()));
    effect.define_property(
        PropertyDescriptor::new("amount", "Amount", PropertyValue::Double(0.0))
            .with_parameter_id(parameter_id.clone())
            .with_numeric_contract(
                ParameterUnit::Unitless,
                ParameterNumericContract::closed(
                    0.0,
                    1.0,
                    None,
                    ParameterInvalidValuePolicy::Reject,
                )
                .expect("bounded value contract"),
            ),
    );
    for (time, value) in [(TimelineTime::ZERO, 0.0), (TimelineTime::ONE, 1.0)] {
        effect
            .apply_property_mutation(PropertyMutation::SetKeyframe {
                path: "amount".to_owned(),
                keyframe: Keyframe::linear(time, PropertyValue::Double(value)),
            })
            .expect("legal author keys");
    }
    let mut clip = Clip::new(AssetId::new(), TimelineTime::ZERO, TimelineTime::ONE).expect("clip");
    clip.add_effect_node(effect);
    let expected = clip.effects[0].clone();
    let authored_path = expected
        .properties
        .iter()
        .find(|(_, property)| property.descriptor.parameter_id() == &parameter_id)
        .expect("stable parameter address")
        .0
        .to_owned();
    document.sequences.active_mut().expect("sequence").video_tracks[0]
        .add_clip(clip)
        .expect("clip admission");
    let root = std::env::temp_dir().join(format!("mondrian-visual-handle-{}", AssetId::new()));
    std::fs::create_dir_all(&root).expect("owned directory");
    let database = root.join("index.db");
    std::fs::write(&database, b"sqlite placeholder").expect("library fixture");
    let archive = root.join("曲线验收.mdp");
    save_project_archive(&document, &database, &archive).expect("durable baseline");
    let bytes = std::fs::read(&archive).expect("baseline bytes");
    for time_offset in [
        TimelineTime::NEGATIVE_ONE_THIRD,
        TimelineTime::new(2, 1).expect("two"),
    ] {
        let mut effect_json = serde_json::to_value(&expected).expect("serialize effect");
        let control = &mut effect_json["properties"]["properties"][&authored_path]["channels"][0]
            ["keyframes"][0]["interp_out"];
        assert_eq!(
            *control,
            serde_json::to_value(KeyframeInterpolation::Linear).expect("linear shape")
        );
        *control = serde_json::to_value(KeyframeInterpolation::Bezier(ExactBezierHandle {
            time_offset,
            value_offset: 0.25,
        }))
        .expect("finite but invalid temporal control");
        let mut invalid = document.clone();
        invalid.sequences.active_mut().expect("sequence").video_tracks[0].clips[0].effects[0] =
            serde_json::from_value(effect_json).expect("raw document decode");
        assert!(
            invalid.validate().is_err(),
            "invalid persisted handle must reject"
        );
        assert!(
            save_project_archive(&invalid, &database, &archive).is_err(),
            "invalid snapshot cannot publish"
        );
        assert_eq!(std::fs::read(&archive).expect("retained archive"), bytes);
        let reopened =
            read_project_document_from_archive(&archive).expect("valid archive still reopens");
        let restored =
            &reopened.sequences.active().expect("sequence").video_tracks[0].clips[0].effects[0];
        assert_eq!(restored, &expected);
        assert_eq!(
            restored.evaluate_parameter(&parameter_id, TimelineTime::new(1, 2).expect("half")),
            Some(PropertyValue::Double(0.5))
        );
        assert_eq!(
            std::fs::read_dir(&root).expect("owned directory").count(),
            2,
            "failed save leaves no temporary archive"
        );
    }
    std::fs::remove_dir_all(root).expect("remove owned fixture");
}
