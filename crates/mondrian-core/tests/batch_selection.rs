//! Public complete-key removal contract, including independent channel keys.
use glam::Vec2;
use mondrian_core::automation::{
    AnimationChannelValue, Keyframe, PropertyBag, PropertyDescriptor, PropertyMutation,
    PropertyValue,
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
