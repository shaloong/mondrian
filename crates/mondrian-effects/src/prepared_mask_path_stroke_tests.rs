use super::*;
use crate::{instantiate_effect_node, lower_effect_graph_to_gpu_pipeline, EffectType, MaskShape};
use mondrian_core::{
    automation::{InterpolationType, Keyframe, PropertyMutation},
    mask_data::{MaskParticipation, MaskShapeInterpolation},
    MaskId, PropertyHost,
};

const SPACE: WorkingColorSpace = WorkingColorSpace::LinearRec709;

fn rectangle_shape(x: f32) -> MaskShape {
    MaskShape::Rectangle {
        x,
        y: 0.25,
        width: 0.2,
        height: 0.5,
        corner_radius: 0.0,
    }
}

fn mask(x: f32) -> MaskComponent {
    let mut mask = MaskComponent::new(
        "path".into(),
        crate::MaskEvaluation { shape: rectangle_shape(x), ..Default::default() },
    );
    mask.participation = MaskParticipation::EffectOnly;
    mask
}

fn set(effect: &mut EffectNode, name: &str, value: PropertyValue) {
    effect
        .apply_property_mutation(PropertyMutation::SetStaticValue {
            path: effect.effect_type.property_path(name),
            value,
        })
        .unwrap();
}

fn stroke(path: Option<MaskId>) -> EffectNode {
    let mut effect = instantiate_effect_node(EffectType::MaskPathStroke).unwrap();
    set(&mut effect, "path", PropertyValue::MaskReference(path));
    set(&mut effect, "brush_size", PropertyValue::Float(3.0));
    set(&mut effect, "spacing", PropertyValue::Float(0.0));
    effect
}

fn prepare(
    effect: &EffectNode,
    masks: &[MaskComponent],
) -> Result<PreparedEffectProgram, EffectGraphBuildError> {
    PreparedEffectProgram::prepare(std::slice::from_ref(effect), masks, SPACE)
}

fn draws(graph: &CompiledEffectGraph) -> usize {
    lower_effect_graph_to_gpu_pipeline(graph).unwrap().spatial_stage_count()
}

#[test]
fn mask_path_stroke_resolves_an_exact_hold_key_by_id_and_ignores_unrelated_masks() {
    let key = TimelineTime::new(1, 3).unwrap();
    let before = TimelineTime::new(1, 4).unwrap();
    let mut selected = mask(0.1);
    let mut other = mask(0.7);
    selected.set_shape_animation_enabled(true, TimelineTime::ZERO).unwrap();
    selected
        .write_shape(key, rectangle_shape(0.3), MaskShapeInterpolation::Hold)
        .unwrap();
    selected
        .write_shape(
            TimelineTime::ONE,
            rectangle_shape(0.5),
            MaskShapeInterpolation::Hold,
        )
        .unwrap();
    let effect = stroke(Some(selected.id));
    let original = prepare(&effect, &[selected.clone(), other.clone()]).unwrap();
    selected.name = "renamed path".into();
    other.enabled = false;
    let reordered = prepare(&effect, &[other, selected]).unwrap();

    let at = original.evaluate(key).unwrap();
    assert_eq!(draws(&at), 1);
    assert_eq!(at.identity(), reordered.evaluate(key).unwrap().identity());
    assert_ne!(at.identity(), original.evaluate(before).unwrap().identity());
    assert_eq!(
        original.evaluate(before).unwrap().identity(),
        original.evaluate(TimelineTime::ZERO).unwrap().identity(),
    );
}

#[test]
fn mask_path_stroke_rejects_broken_references_and_all_masks_retains_only_enabled_geometry() {
    let first = mask(0.1);
    let second = mask(0.55);
    let selected = stroke(Some(first.id));
    let mut disabled = first.clone();
    disabled.enabled = false;
    for masks in [
        vec![second.clone()],
        vec![disabled.clone()],
        vec![first.clone(), disabled.clone()],
    ] {
        assert!(matches!(
            prepare(&selected, &masks),
            Err(EffectGraphBuildError::InvalidAuthorState { .. })
        ));
    }
    let mut all = stroke(Some(MaskId::new()));
    set(&mut all, "all_masks", PropertyValue::Bool(true));
    assert!(matches!(
        prepare(&all, &[disabled.clone()]),
        Err(EffectGraphBuildError::InvalidAuthorState { .. })
    ));
    assert!(matches!(
        prepare(&all, &[first, disabled.clone()]),
        Err(EffectGraphBuildError::InvalidAuthorState { .. })
    ));
    let clean = prepare(&all, std::slice::from_ref(&second))
        .unwrap()
        .evaluate(TimelineTime::ZERO)
        .unwrap();
    let with_disabled = prepare(&all, &[disabled, second])
        .unwrap()
        .evaluate(TimelineTime::ZERO)
        .unwrap();
    assert_eq!(draws(&clean), 1);
    assert_eq!(clean.identity(), with_disabled.identity());
}

#[test]
fn mask_path_stroke_can_animate_from_unbound_to_ordered_all_masks() {
    let later = TimelineTime::new(2, 3).unwrap();
    let mut effect = stroke(None);
    let path = EffectType::MaskPathStroke.property_path("all_masks");
    for (time, all) in [(TimelineTime::ZERO, false), (later, true)] {
        effect
            .properties
            .set_keyframe(
                &path,
                Keyframe::from_preset(time, PropertyValue::Bool(all), InterpolationType::Hold),
            )
            .unwrap();
    }
    let first = mask(0.1);
    let second = mask(0.55);
    let program = prepare(&effect, &[first.clone(), second.clone()]).unwrap();
    let reverse = prepare(&effect, &[second, first]).unwrap();
    assert_eq!(draws(&program.evaluate(TimelineTime::ZERO).unwrap()), 0);
    let active = program.evaluate(later).unwrap();
    assert_eq!(draws(&active), 1);
    assert_ne!(
        active.identity(),
        reverse.evaluate(later).unwrap().identity()
    );
}

#[test]
fn mask_path_stroke_empty_trim_is_identity_and_legacy_stroke_keeps_inert_controls() {
    let source = mask(0.15);
    let mut empty = stroke(Some(source.id));
    set(&mut empty, "start", PropertyValue::Float(80.0));
    set(&mut empty, "end", PropertyValue::Float(20.0));
    let compiled = prepare(&empty, std::slice::from_ref(&source))
        .unwrap()
        .evaluate(TimelineTime::ZERO)
        .unwrap();
    assert_eq!(draws(&compiled), 0);

    let baseline = instantiate_effect_node(EffectType::Stroke).unwrap();
    let mut edited = baseline.clone();
    for (name, value) in [
        ("path", 1.0),
        ("all_masks", 1.0),
        ("sequential_stroke", 0.0),
        ("start", 80.0),
        ("end", 20.0),
        ("spacing", 83.0),
    ] {
        set(&mut edited, name, PropertyValue::Float(value));
    }
    let baseline = prepare(&baseline, &[]).unwrap().evaluate(TimelineTime::ZERO).unwrap();
    let edited = prepare(&edited, &[source]).unwrap().evaluate(TimelineTime::ZERO).unwrap();
    assert_eq!(draws(&edited), 1);
    assert_eq!(baseline.identity(), edited.identity());
}
