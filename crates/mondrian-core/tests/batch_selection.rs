//! Public complete-key set edits, including independent channel keys and exact translation.
use glam::Vec2;
use mondrian_core::automation::{
    AnimationChannelValue, InterpolationType, Keyframe, PropertyBag, PropertyDescriptor,
    PropertyMutation, PropertyValue,
};
use mondrian_core::{KeyframeId, TimelineTime};

#[test]
fn complete_key_batch_removal_rejects_independent_channels_and_preserves_last_value() {
    let mut bag = PropertyBag::default();
    bag.define(PropertyDescriptor::new(
        "position",
        "Position",
        PropertyValue::Vec2(Vec2::ZERO),
    ));
    let mut ids = Vec::new();
    for index in 0..3 {
        let key = Keyframe::linear(
            TimelineTime::new(index, 1).expect("time"),
            PropertyValue::Vec2(Vec2::new(index as f32, index as f32 * 2.0)),
        );
        ids.push(key.id);
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "position".into(),
            keyframe: key,
        })
        .expect("seed");
    }
    let original = bag.clone();
    let retain_time = TimelineTime::new(3, 2).expect("retention");
    for invalid in [
        vec![],
        vec![ids[0], ids[0]],
        vec![ids[0], KeyframeId::new()],
    ] {
        assert!(bag
            .apply_mutation(PropertyMutation::RemoveKeyframes {
                path: "position".into(),
                keyframe_ids: invalid,
                retain_time
            })
            .is_err());
        assert_eq!(bag, original);
    }
    let mut independent = original.clone();
    let independent_key = Keyframe::linear(
        TimelineTime::new(3, 1).expect("independent time"),
        AnimationChannelValue::Number(9.0),
    );
    let incomplete_id = independent_key.id;
    independent
        .apply_mutation(PropertyMutation::SetChannelKeyframe {
            path: "position".into(),
            channel_index: 1,
            keyframe: independent_key,
        })
        .expect("independent component");
    let before = independent.clone();
    assert!(independent
        .apply_mutation(PropertyMutation::RemoveKeyframes {
            path: "position".into(),
            keyframe_ids: vec![ids[0], incomplete_id],
            retain_time,
        })
        .is_err());
    assert_eq!(
        independent, before,
        "a complete valid first key cannot leak through a rejected batch"
    );
    let retained = bag.evaluate("position", retain_time).expect("original evaluation");
    bag.apply_mutation(
        PropertyMutation::RemoveKeyframes {
            path: "nested.position".into(),
            keyframe_ids: ids,
            retain_time,
        }
        .map_path(|path| path.trim_start_matches("nested.").to_owned()),
    )
    .expect("mapped batch");
    let property = bag.property("position").expect("property");
    property.validate().expect("unkeyed contract");
    assert!(property.channels().iter().all(|channel| channel.keyframes().is_empty()));
    assert_eq!(
        bag.evaluate("position", TimelineTime::ZERO).expect("unkeyed"),
        retained
    );
    let reopened: PropertyBag =
        serde_json::from_slice(&serde_json::to_vec(&bag).expect("serialize")).expect("reopen");
    assert_eq!(reopened, bag);
}

#[test]
fn selected_key_translation_preserves_large_integer_values_and_exact_times() {
    let mut bag = PropertyBag::default();
    bag.define(PropertyDescriptor::new(
        "counter",
        "Counter",
        PropertyValue::Int(0),
    ));
    let base = 1_i64 << 54;
    let mut keys = Vec::new();
    for (offset, value) in [
        (0, (1_i64 << 60) + 37),
        (1, i64::MIN + 17),
        (4, i64::MAX - 31),
    ] {
        let key = Keyframe::from_preset(
            TimelineTime::new(base + offset, 1).expect("exact time"),
            PropertyValue::Int(value),
            InterpolationType::Hold,
        );
        keys.push(key.clone());
        bag.apply_mutation(PropertyMutation::SetKeyframe { path: "counter".into(), keyframe: key })
            .expect("seed");
    }
    let original = bag.clone();
    let selected = vec![keys[1].id, keys[0].id];
    bag.apply_mutation(
        PropertyMutation::ShiftKeyframes {
            path: "nested.counter".into(),
            keyframe_ids: selected.clone(),
            delta: TimelineTime::ONE,
        }
        .map_path(|path| path.trim_start_matches("nested.").to_owned()),
    )
    .expect("move selected neighbors together");
    let property = bag.property("counter").expect("counter");
    for (index, key) in keys.iter().enumerate() {
        let changed = property.keyframe_by_id(key.id).expect("identity retained");
        assert_eq!(changed.value, key.value);
        assert_eq!(
            changed.time,
            key.time
                .checked_add(if index < 2 {
                    TimelineTime::ONE
                } else {
                    TimelineTime::ZERO
                })
                .expect("expected")
        );
    }
    let changed = bag.clone();
    for (ids, delta) in [
        (
            selected.clone(),
            TimelineTime::new(3, 1).expect("collision delta"),
        ),
        (vec![], TimelineTime::ZERO),
        (vec![keys[0].id, keys[0].id], TimelineTime::ZERO),
        (vec![keys[0].id, KeyframeId::new()], TimelineTime::ZERO),
    ] {
        assert!(bag
            .apply_mutation(PropertyMutation::ShiftKeyframes {
                path: "counter".into(),
                keyframe_ids: ids,
                delta
            })
            .is_err());
        assert_eq!(bag, changed);
    }
    bag.apply_mutation(PropertyMutation::ShiftKeyframes {
        path: "counter".into(),
        keyframe_ids: selected,
        delta: TimelineTime::new(-1, 1).expect("reverse"),
    })
    .expect("reverse exact translation");
    assert_eq!(bag, original);
    let reopened: PropertyBag =
        serde_json::from_slice(&serde_json::to_vec(&changed).expect("serialize")).expect("reopen");
    assert_eq!(reopened, changed);
}

#[test]
fn selected_key_translation_rejects_second_channel_collision_and_incomplete_keys_atomically() {
    let mut bag = PropertyBag::default();
    bag.define(PropertyDescriptor::new(
        "position",
        "Position",
        PropertyValue::Vec2(Vec2::ZERO),
    ));
    let mut ids = Vec::new();
    for index in [0, 1, 4] {
        let key = Keyframe::linear(
            TimelineTime::new(index, 1).expect("time"),
            PropertyValue::Vec2(Vec2::new(index as f32, index as f32 * 2.0)),
        );
        ids.push(key.id);
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "position".into(),
            keyframe: key,
        })
        .expect("seed");
    }
    let independent = Keyframe::linear(
        TimelineTime::new(3, 1).expect("time"),
        AnimationChannelValue::Number(99.0),
    );
    let independent_id = independent.id;
    bag.apply_mutation(PropertyMutation::SetChannelKeyframe {
        path: "position".into(),
        channel_index: 1,
        keyframe: independent,
    })
    .expect("independent key");
    let original = bag.clone();
    for selection in [vec![ids[1], ids[0]], vec![ids[0], independent_id]] {
        assert!(bag
            .apply_mutation(PropertyMutation::ShiftKeyframes {
                path: "position".into(),
                keyframe_ids: selection,
                delta: TimelineTime::new(2, 1).expect("delta")
            })
            .is_err());
        assert_eq!(
            bag, original,
            "first-channel changes cannot escape second-channel refusal"
        );
    }
}

#[test]
fn exact_curve_selected_translation_normalizes_constraints_and_refuses_overflow() {
    use mondrian_core::{
        ExactAutomationCurve, ExactAutomationKeyframe, InterpolationType, ParameterId,
    };
    for mode in [
        InterpolationType::AutoBezier,
        InterpolationType::ContinuousBezier,
    ] {
        let mut curve =
            ExactAutomationCurve::new(ParameterId::new_static("test.group"), 0.0).expect("curve");
        for (time, value) in [(0, 0.0), (1, 4.0), (4, -2.0)] {
            curve
                .set_keyframe(ExactAutomationKeyframe::linear(
                    TimelineTime::new(time, 1).expect("time"),
                    value,
                ))
                .expect("seed");
        }
        let ids = curve.keyframes.iter().map(|key| key.id).collect::<Vec<_>>();
        for id in &ids {
            curve.set_keyframe_interpolation(*id, mode).expect("constraint");
        }
        let before = curve.clone();
        curve
            .shift_keyframes(&[ids[1], ids[0]], TimelineTime::ONE)
            .expect("translate neighbors together");
        curve.validate().expect("constraints");
        for key in &before.keyframes {
            let shifted =
                curve.keyframes.iter().find(|other| other.id == key.id).expect("identity");
            assert_eq!(shifted.value.to_bits(), key.value.to_bits());
            assert_eq!(shifted.tangent_mode, key.tangent_mode);
        }
        let moved = curve.clone();
        assert!(curve
            .shift_keyframes(&ids[..2], TimelineTime::new(3, 1).expect("collision"))
            .is_err());
        assert_eq!(curve, moved);
    }
    let mut curve =
        ExactAutomationCurve::new(ParameterId::new_static("test.overflow"), 1.0).expect("curve");
    for time in [1, i64::MAX] {
        curve
            .set_keyframe(ExactAutomationKeyframe::linear(
                TimelineTime::new(time, 1).expect("time"),
                0.0,
            ))
            .expect("seed");
    }
    let ids = curve.keyframes.iter().map(|key| key.id).collect::<Vec<_>>();
    let before = curve.clone();
    assert!(curve.shift_keyframes(&ids, TimelineTime::ONE).is_err());
    assert_eq!(curve, before);
}
