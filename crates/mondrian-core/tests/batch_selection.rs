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

#[test]
fn selected_value_command_preserves_wide_integers_identity_and_codec() {
    use mondrian_core::automation::{
        ParameterInvalidValuePolicy, ParameterNumericContract, ParameterUnit,
    };
    let numeric = ParameterNumericContract::closed(
        i64::MIN as f64,
        i64::MAX as f64,
        Some(1.0),
        ParameterInvalidValuePolicy::Reject,
    )
    .expect("integer contract");
    let mut bag = PropertyBag::default();
    bag.define(
        PropertyDescriptor::new("counter", "Counter", PropertyValue::Int(0))
            .with_numeric_contract(ParameterUnit::Unitless, numeric),
    );
    let mut keys = Vec::new();
    for (time, value) in [
        (1, (1_i64 << 54) + 1),
        (2, (1_i64 << 54) + 3),
        (3, i64::MAX),
    ] {
        let key = Keyframe::from_preset(
            TimelineTime::new(time, 7).expect("exact time"),
            PropertyValue::Int(value),
            InterpolationType::Hold,
        );
        keys.push(key.clone());
        bag.apply_mutation(PropertyMutation::SetKeyframe { path: "counter".into(), keyframe: key })
            .expect("seed");
    }
    let command = PropertyMutation::OffsetKeyframeValues {
        path: "nested.counter".into(),
        channel_index: 0,
        keyframe_ids: vec![keys[1].id, keys[0].id],
        delta: 1.0,
        steps: 1,
    }
    .map_path(|path| path.trim_start_matches("nested.").to_owned());
    assert_eq!(command.path(), "counter");
    bag.apply_mutation(command).expect("common exact offset");
    let property = bag.property("counter").expect("counter");
    for (index, key) in keys.iter().enumerate() {
        let changed = property.keyframe_by_id(key.id).expect("identity retained");
        assert_eq!(changed.time, key.time);
        let PropertyValue::Int(value) = key.value else {
            panic!("integer source")
        };
        assert_eq!(
            changed.value,
            PropertyValue::Int(if index < 2 { value + 1 } else { value })
        );
    }
    let before = bag.clone();
    bag.apply_mutation(PropertyMutation::OffsetKeyframeValues {
        path: "counter".into(),
        channel_index: 0,
        keyframe_ids: keys.iter().map(|key| key.id).collect(),
        delta: f64::MAX,
        steps: 5,
    })
    .expect("common boundary no-op");
    assert_eq!(bag, before);
    let reopened: PropertyBag =
        serde_json::from_slice(&serde_json::to_vec(&bag).expect("serialize")).expect("reopen");
    assert_eq!(reopened, bag);
}

#[test]
fn selected_value_command_scopes_channel_and_rejects_incomplete_selection_atomically() {
    use mondrian_core::automation::{
        ParameterInvalidValuePolicy, ParameterNumericContract, ParameterUnit,
    };
    let numeric =
        ParameterNumericContract::closed(0.0, 1.0, Some(0.1), ParameterInvalidValuePolicy::Reject)
            .expect("bounded contract");
    let mut bag = PropertyBag::default();
    bag.define(
        PropertyDescriptor::new("position", "Position", PropertyValue::Vec2(Vec2::ZERO))
            .with_numeric_contract(ParameterUnit::Normalized, numeric),
    );
    let mut keys = Vec::new();
    for (time, x, y) in [(1, 0.25, 0.125), (2, 0.75, 0.875)] {
        let key = Keyframe::linear(
            TimelineTime::new(time, 7).expect("exact time"),
            PropertyValue::Vec2(Vec2::new(x, y)),
        );
        keys.push(key.clone());
        bag.apply_mutation(PropertyMutation::SetKeyframe {
            path: "position".into(),
            keyframe: key,
        })
        .expect("seed");
    }
    bag.apply_mutation(PropertyMutation::OffsetKeyframeValues {
        path: "position".into(),
        channel_index: 0,
        keyframe_ids: keys.iter().map(|key| key.id).collect(),
        delta: 0.125,
        steps: 5,
    })
    .expect("common boundary");
    let property = bag.property("position").expect("position");
    for (index, key) in keys.iter().enumerate() {
        let changed = property.keyframe_by_id(key.id).expect("identity");
        assert_eq!(changed.time, key.time);
        let PropertyValue::Vec2(original) = key.value else {
            panic!("vector source")
        };
        assert_eq!(
            changed.value,
            PropertyValue::Vec2(Vec2::new(if index == 0 { 0.5 } else { 1.0 }, original.y))
        );
    }
    let independent = Keyframe::linear(
        TimelineTime::new(3, 7).expect("time"),
        AnimationChannelValue::Number(0.5),
    );
    let independent_id = independent.id;
    bag.apply_mutation(PropertyMutation::SetChannelKeyframe {
        path: "position".into(),
        channel_index: 1,
        keyframe: independent,
    })
    .expect("independent channel");
    let before = bag.clone();
    for (ids, channel, delta, steps) in [
        (vec![], 0, 0.0, 0),
        (vec![keys[0].id, keys[0].id], 0, 0.0, 0),
        (vec![keys[0].id, KeyframeId::new()], 0, 0.0, 0),
        (vec![keys[0].id, independent_id], 0, 0.125, 1),
        (vec![keys[0].id], 2, 0.125, 1),
        (vec![keys[0].id], 0, f64::NAN, 0),
    ] {
        assert!(bag
            .apply_mutation(PropertyMutation::OffsetKeyframeValues {
                path: "position".into(),
                channel_index: channel,
                keyframe_ids: ids,
                delta,
                steps
            })
            .is_err());
        assert_eq!(bag, before, "rejected command must retain every channel");
    }
    let reopened: PropertyBag =
        serde_json::from_slice(&serde_json::to_vec(&bag).expect("serialize")).expect("reopen");
    assert_eq!(reopened, bag);
}

#[test]
fn selected_value_command_preserves_finite_overshoot_and_refuses_nonfinite_controls() {
    use mondrian_core::automation::{
        ExactBezierHandle, KeyframeInterpolation, ParameterInvalidValuePolicy,
        ParameterNumericContract, ParameterUnit,
    };
    for scale in [1.0, f64::MAX] {
        let mut bag = PropertyBag::default();
        let numeric =
            ParameterNumericContract::closed(0.0, scale, None, ParameterInvalidValuePolicy::Reject)
                .expect("bounded contract");
        bag.define(
            PropertyDescriptor::new("gain", "Gain", PropertyValue::Double(0.0))
                .with_numeric_contract(ParameterUnit::Normalized, numeric),
        );
        let mut left = Keyframe::linear(TimelineTime::ZERO, PropertyValue::Double(scale * 0.25));
        left.interp_out = KeyframeInterpolation::Bezier(ExactBezierHandle {
            time_offset: TimelineTime::ONE_THIRD,
            value_offset: scale * 0.5,
        });
        let right = Keyframe::linear(TimelineTime::ONE, PropertyValue::Double(scale * 0.5));
        let ids = vec![left.id, right.id];
        for keyframe in [right, left] {
            bag.apply_mutation(PropertyMutation::SetKeyframe { path: "gain".into(), keyframe })
                .expect("legal authored control");
        }
        let before = bag.clone();
        let result = bag.apply_mutation(PropertyMutation::OffsetKeyframeValues {
            path: "gain".into(),
            channel_index: 0,
            keyframe_ids: ids.clone(),
            delta: scale * 0.5,
            steps: 1,
        });
        if scale == 1.0 {
            result.expect("finite visual overshoot remains legal beyond key bounds");
            let key =
                &bag.property("gain").expect("gain").channel(0).expect("channel").keyframes()[0];
            assert_eq!(
                key.interp_out,
                before
                    .property("gain")
                    .expect("original gain")
                    .channel(0)
                    .expect("original channel")
                    .keyframes()[0]
                    .interp_out
            );
            assert_eq!(
                bag.evaluate("gain", TimelineTime::ZERO),
                Some(PropertyValue::Double(0.75))
            );
        } else {
            assert!(
                result.is_err(),
                "finite keys cannot admit a nonfinite absolute control"
            );
            assert_eq!(
                bag, before,
                "refused controls cannot publish selected values"
            );
            bag.apply_mutation(PropertyMutation::OffsetKeyframeValues {
                path: "gain".into(),
                channel_index: 0,
                keyframe_ids: ids,
                delta: scale * 0.125,
                steps: 1,
            })
            .expect("legal shared displacement");
        }
        bag.property("gain").expect("gain").validate().expect("control admission");
        let reopened: PropertyBag =
            serde_json::from_slice(&serde_json::to_vec(&bag).expect("serialize")).expect("reopen");
        assert_eq!(reopened, bag);
    }
}

#[test]
fn float_group_uses_representable_inward_bounds_and_rejects_invalid_contracts() {
    use mondrian_core::automation::{ParameterInvalidValuePolicy, ParameterNumericContract};
    let contract =
        ParameterNumericContract::closed(0.1, 0.2, None, ParameterInvalidValuePolicy::Reject)
            .expect("float bounds");
    let values = [PropertyValue::Float(0.125), PropertyValue::Float(0.1875)];
    let shifted = contract
        .offset_scalars(&values, f64::MAX, 5)
        .expect("bounded Float displacement");
    let max = 0.2_f32.next_down();
    assert_eq!(
        shifted,
        vec![
            PropertyValue::Float(max - 0.0625),
            PropertyValue::Float(max)
        ]
    );
    for value in &shifted {
        let PropertyValue::Float(value) = value else {
            panic!("Float group")
        };
        assert!((0.1..=0.2).contains(&f64::from(*value)));
    }
    assert_eq!(
        contract.offset_scalars(&values, 0.0, 5),
        Some(values.to_vec())
    );
    assert!(contract.offset_scalars(&[PropertyValue::Float(0.1)], 0.0, 0).is_some());
    assert!(contract.offset_scalars(&[PropertyValue::Float(0.2)], 0.0, 0).is_none());
    let mut invalid = contract;
    invalid.hard_range.min = f64::NAN;
    assert!(invalid.offset_scalars(&values, 0.0, 0).is_none());
    let unrepresentable = ParameterNumericContract::closed(
        0.125_f64.next_up(),
        0.125_f64.next_up(),
        None,
        ParameterInvalidValuePolicy::Reject,
    )
    .expect("valid Double-only range");
    assert!(unrepresentable.offset_scalars(&[PropertyValue::Float(0.125)], 0.0, 0).is_none());
}

#[test]
fn exact_selected_value_offset_normalizes_constraints_atomically() {
    use mondrian_core::automation::{ParameterInvalidValuePolicy, ParameterNumericContract};
    use mondrian_core::{ExactAutomationCurve, ExactAutomationKeyframe, ParameterId};
    let contract =
        ParameterNumericContract::closed(0.0, 4.0, None, ParameterInvalidValuePolicy::Reject)
            .expect("range");
    for mode in [
        InterpolationType::AutoBezier,
        InterpolationType::ContinuousBezier,
    ] {
        let mut curve =
            ExactAutomationCurve::new(ParameterId::new_static("test.group.values"), 0.0)
                .expect("curve");
        for (time, value) in [(1, 1.0), (2, 2.0), (3, 3.0)] {
            curve
                .set_keyframe(ExactAutomationKeyframe::linear(
                    TimelineTime::new(time, 7).expect("exact time"),
                    value,
                ))
                .expect("seed");
        }
        let ids = curve.keyframes.iter().map(|key| key.id).collect::<Vec<_>>();
        for id in &ids {
            curve.set_keyframe_interpolation(*id, mode).expect("constraint");
        }
        let before = curve.clone();
        for selection in [
            vec![],
            vec![ids[0], ids[0]],
            vec![ids[0], KeyframeId::new()],
        ] {
            assert!(curve.offset_keyframe_values(&selection, contract, 0.0, 0).is_err());
            assert_eq!(curve, before);
        }
        assert!(curve.offset_keyframe_values(&ids, contract, f64::NAN, 0).is_err());
        assert_eq!(curve, before);
        curve.offset_keyframe_values(&ids, contract, 1.0, 5).expect("common boundary");
        curve.validate().expect("constraints");
        for (index, key) in curve.keyframes.iter().enumerate() {
            assert_eq!(key.id, before.keyframes[index].id);
            assert_eq!(key.time, before.keyframes[index].time);
            assert_eq!(key.tangent_mode, before.keyframes[index].tangent_mode);
            assert_eq!(key.value, before.keyframes[index].value + 1.0);
        }
        let edited = curve.clone();
        curve.offset_keyframe_values(&ids, contract, 1.0, 1).expect("bound no-op");
        assert_eq!(curve, edited);
        let reopened: ExactAutomationCurve =
            serde_json::from_slice(&serde_json::to_vec(&curve).expect("encode")).expect("decode");
        assert_eq!(reopened, curve);
    }
}
