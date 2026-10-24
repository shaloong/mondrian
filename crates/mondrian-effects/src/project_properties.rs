//! Definition-owned project defaults over the existing persisted instance metadata.
//! Parameter values and concrete schemas remain solely in the author PropertyBag.
use std::sync::Arc;

use mondrian_core::automation::PropertyBag;
use serde::{Deserialize, Serialize};

use crate::{EffectDefinitionError, EffectProjectGeometry};

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
    fn definition(key: &str, zero: bool) -> EffectDefinition {
        let mut base = PropertyBag::default();
        base.define(PropertyDescriptor::new(
            "literal",
            "Literal",
            PropertyValue::Double(0.25),
        ));
        EffectDefinition::new(key, key, base, EffectColorDomainContract::SCENE_LINEAR)
            .with_graph_builder(Arc::new(|_, _, _| Ok(())))
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
        .with_graph_builder(Arc::new(|_, _, _| Ok(())));
        register_effect_definition(literal).unwrap();
        assert!(prepare(&first, geometry(3, 2)).is_err());
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
