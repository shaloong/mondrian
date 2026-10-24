//! Definition-owned project defaults over the existing persisted instance metadata.
//! Parameter values and concrete schemas remain solely in the author PropertyBag.
use std::sync::Arc;

use mondrian_core::automation::PropertyBag;
use serde::{Deserialize, Serialize};

use crate::{EffectDefinitionError, EffectGraphBuildError, EffectNode, EffectProjectGeometry};

/// Rebind project defaults during an explicit owner geometry edit.
///
/// This cold authoring operation changes only concrete schema defaults and the
/// construction binding. Values, curves and every author identity are preserved,
/// even when a value equals the old default. Only otherwise identical schemas
/// can migrate. The node is replaced only after complete old/new admission.
/// Unavailable literal definitions remain editable; an unavailable contextual
/// definition cannot safely supply new defaults and refuses.
pub fn rebind_effect_project_geometry(
    effect: &mut EffectNode,
    previous: EffectProjectGeometry,
    next: EffectProjectGeometry,
) -> Result<bool, EffectGraphBuildError> {
    let invalid = |reason: String| EffectGraphBuildError::InvalidAuthorState {
        effect_key: effect.effect_type.key(),
        effect_id: effect.id,
        reason,
    };
    let Some(definition) = crate::effect_definition(&effect.effect_type) else {
        return if has_binding(&effect.params) {
            Err(EffectGraphBuildError::DefinitionUnavailable {
                effect_key: effect.effect_type.key(),
                effect_id: effect.id,
            })
        } else {
            Ok(false)
        };
    };
    if definition
        .project_geometry_for_instance(effect, Some(previous))
        .map_err(&invalid)?
        .is_none()
    {
        return Ok(false);
    }
    crate::prepared::validate_effect_schema_in_project(effect, &definition, Some(previous))?;
    if previous == next {
        return Ok(false);
    }
    let defaults =
        definition.resolved_properties(Some(next)).map_err(|e| invalid(e.to_string()))?;
    let expected = defaults
        .iter()
        .map(|(_, p)| (p.descriptor.parameter_id(), &p.descriptor.schema))
        .collect::<std::collections::HashMap<_, _>>();
    if expected.len() != effect.properties.iter().count() {
        return Err(invalid(
            "project rebind cannot change parameter inventory".into(),
        ));
    }
    let mut candidate = effect.clone();
    for (_, property) in effect.properties.iter() {
        let schema = expected
            .get(property.descriptor.parameter_id())
            .ok_or_else(|| invalid("project rebind cannot change parameter identity".into()))?;
        let mut property = property.clone();
        property.descriptor.schema.default_value = schema.default_value.clone();
        if &property.descriptor.schema != *schema {
            return Err(invalid(
                "project rebind can change defaults only; schema migration is required".into(),
            ));
        }
        candidate.properties.upsert(property);
    }
    candidate.params = encode_binding(next).map_err(&invalid)?;
    crate::prepared::validate_effect_schema_in_project(&candidate, &definition, Some(next))?;
    *effect = candidate;
    Ok(true)
}

/// Add project-dependent properties to a clone of the definition's literal base.
/// The initializer must be pure, retain no instance state and preserve that base.
pub type EffectProjectPropertyInitializer = Arc<
    dyn Fn(EffectProjectGeometry, &mut PropertyBag) -> Result<(), EffectDefinitionError>
        + Send
        + Sync,
>;

const BINDING_VERSION: u32 = 1;
const BINDING_KIND: &str = "mondrian.effect_project_defaults";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectDefaultBinding {
    kind: String,
    version: u32,
    project_geometry: EffectProjectGeometry,
}

pub(crate) fn encode_binding(geometry: EffectProjectGeometry) -> Result<serde_json::Value, String> {
    serde_json::to_value(ProjectDefaultBinding {
        kind: BINDING_KIND.into(),
        version: BINDING_VERSION,
        project_geometry: geometry,
    })
    .map_err(|error| error.to_string())
}

pub(crate) fn has_binding(value: &serde_json::Value) -> bool {
    value.get("kind").and_then(serde_json::Value::as_str) == Some(BINDING_KIND)
}

pub(crate) fn decode_binding(value: &serde_json::Value) -> Result<EffectProjectGeometry, String> {
    let binding: ProjectDefaultBinding = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid effect project-default binding: {error}"))?;
    if binding.kind != BINDING_KIND || binding.version != BINDING_VERSION {
        return Err("unsupported effect project-default binding".into());
    }
    Ok(binding.project_geometry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        effect_definition, instantiate_effect_node, instantiate_effect_node_with_project_geometry,
        register_effect_definition, EffectColorDomainContract, EffectDefinition,
        EffectFrameContext, EffectNode, EffectNodeExt, EffectType, LutPreparationCache,
        PreparedEffectProgram,
    };
    use mondrian_core::automation::{PropertyDescriptor, PropertyValue};
    use mondrian_core::{
        ParameterId, Rational, SampleAspectRatio, TimelineTime, TimelineTimeRange,
        WorkingColorSpace,
    };

    fn geometry(width: u32, height: u32) -> EffectProjectGeometry {
        EffectProjectGeometry::new(width, height, SampleAspectRatio::new(2, 1).unwrap()).unwrap()
    }
    fn contract() -> crate::EffectExecutionContract {
        crate::EffectExecutionContract {
            execution_modes: crate::EffectExecutionModes::CPU_F32,
            determinism: crate::EffectDeterminism::Deterministic,
            state_model: crate::EffectStateModel::Stateless,
            temporal_input: crate::EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: crate::EffectRoiPropagation::UnknownRequiresFullFrame,
            resource_lifetime: crate::EffectResourceLifetime::Frame,
            topology: crate::EffectGraphTopology::LinearChain,
        }
    }
    fn definition(key: &str, zero: bool) -> EffectDefinition {
        let mut base = PropertyBag::default();
        base.define(PropertyDescriptor::new(
            "literal",
            "Literal",
            PropertyValue::Double(0.25),
        ));
        EffectDefinition::new(key, key, base, EffectColorDomainContract::SCENE_LINEAR)
            .with_graph_builder(Arc::new(|_, _, _| Ok(())))
            .with_execution_contract(contract())
            .with_project_properties(
                Arc::new(move |geometry, properties| {
                    let value = if zero {
                        [0.0; 2]
                    } else {
                        geometry.canonical_size()
                    };
                    properties.define(
                        PropertyDescriptor::new(
                            "spatial",
                            "Spatial",
                            PropertyValue::DoubleVec2(value),
                        )
                        .with_parameter_id(ParameterId::new("test.context.spatial").unwrap()),
                    );
                    Ok(())
                }),
                0,
            )
    }
    fn prepare(
        effect: &EffectNode,
        geometry: EffectProjectGeometry,
    ) -> Result<PreparedEffectProgram, crate::EffectGraphBuildError> {
        let context = EffectFrameContext::new(
            Rational::FPS_24,
            geometry,
            TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
        )
        .unwrap();
        PreparedEffectProgram::prepare_hierarchical_with_frame_context(
            &[effect.clone()],
            &[],
            &[],
            &[],
            WorkingColorSpace::LinearRec709,
            context,
            &LutPreparationCache::uncached(),
        )
    }

    #[test]
    fn contextual_defaults_are_concrete_and_do_not_mutate_the_registered_literal_base() {
        let key = "test.contextual.concrete";
        let kind = EffectType::External(key.into());
        register_effect_definition(definition(key, false)).unwrap();
        assert!(matches!(
            instantiate_effect_node(kind.clone()),
            Err(crate::EffectInstantiationError::Properties(
                EffectDefinitionError::ProjectGeometryRequired { .. }
            ))
        ));
        assert!(
            std::panic::catch_unwind(|| EffectNode::with_defaults(kind.clone())).is_err(),
            "legacy infallible assembly must not publish a partial contextual bag"
        );
        let first =
            instantiate_effect_node_with_project_geometry(kind.clone(), geometry(3, 2)).unwrap();
        let second = instantiate_effect_node_with_project_geometry(kind, geometry(5, 4)).unwrap();
        assert_eq!(
            first.properties.property("spatial").unwrap().descriptor.schema.default_value,
            PropertyValue::DoubleVec2([6.0, 2.0])
        );
        assert_eq!(
            second.properties.property("spatial").unwrap().descriptor.schema.default_value,
            PropertyValue::DoubleVec2([10.0, 4.0])
        );
        assert_eq!(
            first.properties.property("literal").unwrap().static_value(),
            &PropertyValue::Double(0.25)
        );
        assert_ne!(
            first.properties.property("spatial").unwrap().track_id,
            second.properties.property("spatial").unwrap().track_id
        );
        assert!(effect_definition(&first.effect_type)
            .unwrap()
            .default_properties()
            .property("spatial")
            .is_none());
        prepare(&first, geometry(3, 2)).unwrap();
        prepare(&second, geometry(5, 4)).unwrap();
        assert!(prepare(&first, geometry(5, 4)).is_err());
        let reopened: EffectNode =
            serde_json::from_slice(&serde_json::to_vec(&first).unwrap()).unwrap();
        assert_eq!(first, reopened);
        prepare(&reopened, geometry(3, 2)).unwrap();
        let mut forged = first.clone();
        let mut property = forged.properties.property("spatial").unwrap().clone();
        property.descriptor.schema.default_value = PropertyValue::DoubleVec2([1.0, 1.0]);
        forged.properties.upsert(property);
        assert!(
            prepare(&forged, geometry(3, 2)).is_err(),
            "complete schema/default equality remains mandatory"
        );
    }

    #[test]
    fn zero_defaults_do_not_erase_owner_binding_or_accept_malformed_metadata() {
        let key = "test.contextual.zero";
        let kind = EffectType::External(key.into());
        register_effect_definition(definition(key, true)).unwrap();
        let first =
            instantiate_effect_node_with_project_geometry(kind.clone(), geometry(3, 2)).unwrap();
        let second =
            instantiate_effect_node_with_project_geometry(kind.clone(), geometry(5, 4)).unwrap();
        assert_eq!(
            first.properties.property("spatial").unwrap().descriptor.schema,
            second.properties.property("spatial").unwrap().descriptor.schema
        );
        assert!(
            prepare(&first, geometry(5, 4)).is_err(),
            "binding is not inferred from equal zero schemas"
        );
        for (field, value) in [
            ("version", serde_json::json!(0)),
            ("kind", serde_json::json!("unknown")),
            ("extra", serde_json::json!(true)),
            (
                "project_geometry",
                serde_json::json!({"width":0,"height":2,"pixel_aspect_ratio":{"numerator":2,"denominator":1}}),
            ),
        ] {
            let mut bad = first.clone();
            bad.params[field] = value;
            assert!(prepare(&bad, geometry(3, 2)).is_err());
        }
        let mut missing = first.clone();
        missing.params = serde_json::json!({});
        assert!(prepare(&missing, geometry(3, 2)).is_err());
        assert!(PreparedEffectProgram::prepare(
            &[first.clone()],
            &[],
            WorkingColorSpace::LinearRec709
        )
        .is_err());
        // Rebinding a definition cannot silently discard an existing context.
        let literal = EffectDefinition::new(
            key,
            key,
            first.properties.clone(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_graph_builder(Arc::new(|_, _, _| Ok(())))
        .with_execution_contract(contract());
        register_effect_definition(literal).unwrap();
        assert!(prepare(&first, geometry(3, 2)).is_err());
    }

    #[test]
    fn project_rebind_changes_only_defaults_and_binding_and_preserves_explicit_author_state() {
        let key = "test.contextual.rebind.preserve";
        let kind = EffectType::External(key.into());
        register_effect_definition(definition(key, false)).unwrap();
        for explicit in [false, true] {
            let mut effect =
                instantiate_effect_node_with_project_geometry(kind.clone(), geometry(3, 2))
                    .unwrap();
            if explicit {
                let mut p = effect.properties.property("spatial").unwrap().clone();
                p.set_static_value(PropertyValue::DoubleVec2([1.0, 1.0])).unwrap();
                p.set_animation_enabled(true);
                p.write_channels(
                    TimelineTime::ZERO,
                    &[(0, 1.0), (1, 1.0)],
                    mondrian_core::InterpolationType::Linear,
                )
                .unwrap();
                p.write_channels(
                    TimelineTime::ONE,
                    &[(0, 2.0), (1, 3.0)],
                    mondrian_core::InterpolationType::Linear,
                )
                .unwrap();
                effect.properties.upsert(p);
            }
            let before = effect.clone();
            assert!(
                rebind_effect_project_geometry(&mut effect, geometry(3, 2), geometry(5, 4))
                    .unwrap()
            );
            let mut expected = before.clone();
            let mut property = expected.properties.property("spatial").unwrap().clone();
            property.descriptor.schema.default_value = PropertyValue::DoubleVec2([10.0, 4.0]);
            expected.properties.upsert(property);
            expected.params = encode_binding(geometry(5, 4)).unwrap();
            assert_eq!(
                effect, expected,
                "all values, paths, identities, animation and metadata survive"
            );
            assert!(
                !rebind_effect_project_geometry(&mut effect, geometry(5, 4), geometry(5, 4))
                    .unwrap()
            );
            prepare(&effect, geometry(5, 4)).unwrap();
            assert!(prepare(&effect, geometry(3, 2)).is_err());
            let restored: EffectNode =
                serde_json::from_slice(&serde_json::to_vec(&effect).unwrap()).unwrap();
            assert_eq!(restored, effect);
            let p = effect.properties.property("spatial").unwrap();
            let reset = effect
                .properties
                .prepare_parameter_write(
                    &mondrian_core::automation::ParameterValueWrite::ResetToDefault {
                        parameter: p.address(),
                    },
                    TimelineTime::ZERO,
                )
                .unwrap()
                .unwrap();
            mondrian_core::automation::PropertyHost::apply_property_mutation(&mut effect, reset)
                .unwrap();
            assert_eq!(
                effect.properties.property("spatial").unwrap().static_value(),
                &PropertyValue::DoubleVec2([10.0, 4.0])
            );
            assert!(!effect.properties.property("spatial").unwrap().is_animated());
        }
    }

    #[test]
    fn project_rebind_refuses_unavailable_or_structural_migrations_without_mutation() {
        let key = "test.contextual.rebind.refuse";
        let kind = EffectType::External(key.into());
        let definition = definition(key, false).with_project_properties(
            Arc::new(|geometry, properties| {
                let mut descriptor = PropertyDescriptor::new(
                    "spatial",
                    "Spatial",
                    PropertyValue::DoubleVec2(geometry.canonical_size()),
                )
                .with_parameter_id(ParameterId::new("test.context.spatial").unwrap());
                if geometry.width() > 3 {
                    descriptor.schema.unit = mondrian_core::ParameterUnit::Degrees;
                }
                properties.define(descriptor);
                Ok(())
            }),
            0,
        );
        register_effect_definition(definition).unwrap();
        let mut effect =
            instantiate_effect_node_with_project_geometry(kind, geometry(3, 2)).unwrap();
        let before = effect.clone();
        assert!(
            rebind_effect_project_geometry(&mut effect, geometry(3, 2), geometry(5, 4)).is_err()
        );
        assert_eq!(effect, before);
        assert!(
            rebind_effect_project_geometry(&mut effect, geometry(5, 4), geometry(6, 4)).is_err()
        );
        assert_eq!(effect, before);
        effect.effect_type = EffectType::External("test.contextual.rebind.missing".into());
        let missing = effect.clone();
        assert!(matches!(
            rebind_effect_project_geometry(&mut effect, geometry(3, 2), geometry(5, 4)),
            Err(EffectGraphBuildError::DefinitionUnavailable { .. })
        ));
        assert_eq!(effect, missing);
        let mut unknown =
            EffectNode::new(EffectType::External("test.literal.rebind.missing".into()));
        let before = unknown.clone();
        assert!(
            !rebind_effect_project_geometry(&mut unknown, geometry(3, 2), geometry(5, 4)).unwrap()
        );
        assert_eq!(unknown, before);
    }

    #[test]
    fn contextual_initializers_cannot_replace_literal_properties_or_publish_invalid_schemas() {
        let definition = definition("test.contextual.invalid", false).with_project_properties(
            Arc::new(|_, properties| {
                let mut property = properties.property("literal").unwrap().clone();
                property.descriptor.schema.default_value = PropertyValue::Double(0.5);
                properties.upsert(property);
                Ok(())
            }),
            0,
        );
        assert!(definition.resolved_properties(Some(geometry(3, 2))).is_err());
        let invalid = EffectDefinition::new(
            "test.contextual.nonfinite",
            "Invalid",
            PropertyBag::default(),
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_project_properties(
            Arc::new(|_, properties| {
                properties.define(PropertyDescriptor::new(
                    "invalid",
                    "Invalid",
                    PropertyValue::Double(f64::NAN),
                ));
                Ok(())
            }),
            0,
        );
        assert!(invalid.resolved_properties(Some(geometry(3, 2))).is_err());
        let literal =
            instantiate_effect_node_with_project_geometry(EffectType::GaussianBlur, geometry(3, 2))
                .unwrap();
        assert_eq!(
            literal.params,
            serde_json::json!({}),
            "literal author representation stays unchanged"
        );
    }
}
