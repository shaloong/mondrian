//! Bind a described, installed OpenFX Filter to the shared visual effect graph.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use mondrian_core::automation::{
    ParameterInvalidValuePolicy, ParameterNumericContract, ParameterNumericRange, ParameterUnit,
    PropertyBag, PropertyDescriptor, PropertyValue,
};
use mondrian_core::{MondrianError, ParameterId};
use mondrian_effects::{
    register_effect_definition, EffectCachePolicy, EffectColorDomainContract, EffectDefinition,
    EffectDeterminism, EffectExecutionContract, EffectExecutionModes, EffectGraphBuildError,
    EffectGraphTopology, EffectPluginContract, EffectResourceLifetime, EffectRoiPropagation,
    EffectStateModel, EffectTemporalInputExtent, EffectType,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::openfx_adapter::OpenFxBinaryInspection;
use crate::openfx_host::{
    describe_openfx_filter, render_openfx_filter_frame, OpenFxDoubleType, OpenFxFilterDescription,
    OpenFxFloatFrame, OpenFxHostError, OpenFxRenderTiming, OpenFxScalarValue,
};

/// A selected Filter cannot be represented by the admitted visual effect contract.
#[derive(Debug, thiserror::Error)]
pub enum OpenFxEffectRegistrationError {
    /// The supervised description child failed or rejected the Filter.
    #[error(transparent)]
    Host(#[from] OpenFxHostError),
    /// The Filter describes a schema that cannot be authored exactly.
    #[error("OpenFX filter schema is unsupported: {0}")]
    Schema(String),
    /// The definition registry rejected the admitted schema.
    #[error("OpenFX effect registration failed: {0}")]
    Registry(String),
}

#[derive(Clone)]
struct BoundParameter {
    name: String,
    parameter_id: Option<ParameterId>,
    default_value: OpenFxScalarValue,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenFxGraphParameters {
    timing: OpenFxRenderTiming,
    values: BTreeMap<String, OpenFxScalarValue>,
}

/// Describe and register one installed OpenFX Filter for Clip insertion.
///
/// The selected binary remains external to the project and is verified by its
/// full SHA-256 on each supervised render. A changed binary must be explicitly
/// selected and described again before it can replace this definition.
pub fn register_selected_openfx_filter(
    helper_executable: &Path,
    inspection: &OpenFxBinaryInspection,
    plugin_identifier: &str,
) -> Result<EffectType, OpenFxEffectRegistrationError> {
    let description = describe_openfx_filter(helper_executable, inspection, plugin_identifier)?;
    let effect_type = EffectType::Plugin(stable_key("openfx.", plugin_identifier.as_bytes()));
    let definition = build_definition(helper_executable, inspection, description, &effect_type)?;
    register_effect_definition(definition)
        .map_err(|error| OpenFxEffectRegistrationError::Registry(error.to_string()))?;
    Ok(effect_type)
}

fn build_definition(
    helper_executable: &Path,
    inspection: &OpenFxBinaryInspection,
    description: OpenFxFilterDescription,
    effect_type: &EffectType,
) -> Result<EffectDefinition, OpenFxEffectRegistrationError> {
    let plugin = inspection
        .plugins
        .iter()
        .find(|plugin| plugin.identifier == description.identifier)
        .ok_or_else(|| {
            OpenFxEffectRegistrationError::Schema("selected plugin is missing".into())
        })?;
    let mut properties = PropertyBag::default();
    let mut bound = Vec::with_capacity(description.parameters.len());
    for (index, parameter) in description.parameters.iter().enumerate() {
        let editable = !parameter.secret && parameter.enabled;
        let parameter_id = if editable {
            let mut identity = description.identifier.as_bytes().to_vec();
            identity.push(0);
            identity.extend_from_slice(parameter.name.as_bytes());
            let parameter_id = ParameterId::new(stable_key("openfx.param.", &identity))
                .map_err(|error| OpenFxEffectRegistrationError::Schema(error.to_string()))?;
            let path = stable_key("openfx.control.", parameter.name.as_bytes());
            let default_value = match parameter.default_value {
                OpenFxScalarValue::Double(value) => PropertyValue::Double(value),
                OpenFxScalarValue::Boolean(value) => PropertyValue::Bool(value),
            };
            let mut descriptor = PropertyDescriptor::try_new(path, &parameter.label, default_value)
                .map_err(|error| OpenFxEffectRegistrationError::Schema(error.to_string()))?
                .with_parameter_id(parameter_id.clone())
                .with_animatable(parameter.can_animate);
            descriptor.ui_metadata.display_order = Some(index as u32);
            descriptor.ui_metadata.group_name = Some("OpenFX".to_owned());
            if let OpenFxScalarValue::Double(_) = parameter.default_value {
                let (Some(min), Some(max), Some(display_min), Some(display_max)) = (
                    parameter.minimum,
                    parameter.maximum,
                    parameter.display_minimum,
                    parameter.display_maximum,
                ) else {
                    return Err(OpenFxEffectRegistrationError::Schema(format!(
                        "parameter `{}` has incomplete numeric bounds",
                        parameter.name
                    )));
                };
                let hard_range = ParameterNumericRange::new(min, max)
                    .map_err(|error| OpenFxEffectRegistrationError::Schema(error.to_string()))?;
                let soft_range =
                    ParameterNumericRange::new(display_min.max(min), display_max.min(max))
                        .map_err(|error| {
                            OpenFxEffectRegistrationError::Schema(error.to_string())
                        })?;
                let numeric = ParameterNumericContract::new(
                    hard_range,
                    soft_range,
                    None,
                    ParameterInvalidValuePolicy::Reject,
                )
                .map_err(|error| OpenFxEffectRegistrationError::Schema(error.to_string()))?;
                let unit = match parameter.double_type {
                    Some(OpenFxDoubleType::Plain | OpenFxDoubleType::Scale) => {
                        ParameterUnit::Unitless
                    }
                    None => {
                        return Err(OpenFxEffectRegistrationError::Schema(format!(
                            "parameter `{}` has no Double interpretation",
                            parameter.name
                        )));
                    }
                };
                descriptor = descriptor.with_numeric_contract(unit, numeric);
            }
            descriptor.validate().map_err(|error| {
                OpenFxEffectRegistrationError::Schema(format!(
                    "parameter `{}`: {error}",
                    parameter.name
                ))
            })?;
            properties.define(descriptor);
            Some(parameter_id)
        } else {
            None
        };
        bound.push(BoundParameter {
            name: parameter.name.clone(),
            parameter_id,
            default_value: parameter.default_value.clone(),
        });
    }

    let params_key = effect_type.key();
    let parameters = Arc::new(bound);
    let params_builder = Arc::new(
        move |effect: &mondrian_effects::EffectNode,
              context: mondrian_effects::EffectEvalContext| {
            let invalid = |reason: String| EffectGraphBuildError::InvalidAuthorState {
                effect_key: params_key.clone(),
                effect_id: effect.id,
                reason,
            };
            let frame_context = context
                .frame_context
                .ok_or_else(|| invalid("OpenFX requires a sequence frame context".to_owned()))?;
            let (first, last) =
                frame_context.available_frames().map_err(|error| invalid(error.to_string()))?;
            let timing = OpenFxRenderTiming {
                frame: frame_context.frame_coordinate(context.time),
                frame_rate: frame_context.frame_rate().to_f64(),
                first_frame: first as f64,
                last_frame: last as f64,
                pixel_aspect_ratio: frame_context.pixel_aspect_ratio().to_f64(),
            };
            let mut values = BTreeMap::new();
            for parameter in parameters.iter() {
                let value = match &parameter.parameter_id {
                    Some(parameter_id) => {
                        match effect.evaluate_parameter(parameter_id, context.time) {
                            Some(PropertyValue::Double(value)) if value.is_finite() => {
                                OpenFxScalarValue::Double(value)
                            }
                            Some(PropertyValue::Bool(value)) => OpenFxScalarValue::Boolean(value),
                            _ => {
                                return Err(invalid(format!(
                                    "parameter `{}` has no valid value",
                                    parameter.name
                                )))
                            }
                        }
                    }
                    None => parameter.default_value.clone(),
                };
                values.insert(parameter.name.clone(), value);
            }
            let params = serde_json::to_value(OpenFxGraphParameters { timing, values })
                .map_err(|error| invalid(error.to_string()))?;
            Ok(Some(params))
        },
    );

    let version = format!(
        "{}.{}:{}",
        plugin.version_major, plugin.version_minor, inspection.binary_sha256
    );
    let helper = helper_executable.to_path_buf();
    let inspection = inspection.clone();
    let identifier = description.identifier.clone();
    let processor = Arc::new(
        move |pixels: &mut Vec<[f32; 4]>, width, height, params: &serde_json::Value, _seed| {
            let parameters: OpenFxGraphParameters = serde_json::from_value(params.clone())
                .map_err(|error| MondrianError::WorkflowStepFailed {
                    step_id: "openfx_graph_parameters".to_owned(),
                    reason: error.to_string(),
                })?;
            let frame = OpenFxFloatFrame { width, height, pixels: std::mem::take(pixels) };
            let output = render_openfx_filter_frame(
                &helper,
                &inspection,
                &identifier,
                &parameters.timing,
                &parameters.values,
                &frame,
            )
            .map_err(|error| MondrianError::WorkflowStepFailed {
                step_id: "openfx_filter_render".to_owned(),
                reason: error.to_string(),
            })?;
            *pixels = output.pixels;
            Ok(())
        },
    );
    Ok(EffectDefinition::new(
        effect_type.key(),
        description.identifier,
        properties,
        EffectColorDomainContract::SCENE_LINEAR,
    )
    .with_category(vec!["OpenFX".to_owned()])
    .with_execution_contract(EffectExecutionContract {
        execution_modes: EffectExecutionModes::CPU_F32,
        determinism: EffectDeterminism::Nondeterministic,
        state_model: EffectStateModel::Stateless,
        temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
        roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
        resource_lifetime: EffectResourceLifetime::Frame,
        topology: EffectGraphTopology::LinearChain,
    })
    .with_custom_float_render_backend(
        params_builder,
        None,
        EffectCachePolicy::Uncacheable,
        processor,
    )
    .with_plugin_contract(EffectPluginContract::new(version)))
}

fn stable_key(prefix: &str, bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{prefix}{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openfx_adapter::OpenFxPluginDescriptor;
    use mondrian_core::{
        Rational, SampleAspectRatio, TimelineTime, TimelineTimeRange, WorkingColorSpace,
    };
    use mondrian_effects::{
        instantiate_effect_node, EffectFrameContext, LutPreparationCache, PreparedEffectProgram,
    };

    #[test]
    fn described_filter_binds_author_values_and_exact_frame_time_into_shared_graph() {
        let identifier = "test.mondrian.openfx.bridge";
        let inspection = OpenFxBinaryInspection {
            binary_path: std::path::PathBuf::from("C:/selected/Basic.ofx"),
            binary_sha256: "a".repeat(64),
            plugins: vec![OpenFxPluginDescriptor {
                identifier: identifier.to_owned(),
                api_version: 1,
                version_major: 1,
                version_minor: 0,
            }],
        };
        let description = OpenFxFilterDescription {
            identifier: identifier.to_owned(),
            parameters: vec![
                crate::openfx_host::OpenFxParameterDescription {
                    name: "gain".to_owned(),
                    label: "Gain".to_owned(),
                    hint: "Multiply every channel".to_owned(),
                    default_value: OpenFxScalarValue::Double(1.0),
                    double_type: Some(OpenFxDoubleType::Scale),
                    minimum: Some(0.0),
                    maximum: Some(2.0),
                    display_minimum: Some(0.0),
                    display_maximum: Some(2.0),
                    can_animate: true,
                    secret: false,
                    enabled: true,
                },
                crate::openfx_host::OpenFxParameterDescription {
                    name: "internalSwitch".to_owned(),
                    label: "Internal Switch".to_owned(),
                    hint: String::new(),
                    default_value: OpenFxScalarValue::Boolean(false),
                    double_type: None,
                    minimum: None,
                    maximum: None,
                    display_minimum: None,
                    display_maximum: None,
                    can_animate: false,
                    secret: true,
                    enabled: true,
                },
            ],
        };
        let effect_type = EffectType::Plugin(stable_key("openfx.", identifier.as_bytes()));
        let definition = build_definition(
            Path::new("C:/selected/mondrian.exe"),
            &inspection,
            description,
            &effect_type,
        )
        .expect("build selected filter definition");
        register_effect_definition(definition).expect("register filter definition");
        let mut effect = instantiate_effect_node(effect_type).expect("insert filter instance");
        assert_eq!(
            effect.properties.iter().count(),
            1,
            "hidden control stays at its default"
        );
        let gain_id = effect
            .properties
            .iter()
            .next()
            .expect("gain property")
            .1
            .descriptor
            .parameter_id()
            .clone();
        effect
            .set_static_value_by_parameter(&gain_id, PropertyValue::Double(1.5))
            .expect("author gain");
        let range = TimelineTimeRange::new(
            TimelineTime::ZERO,
            TimelineTime::new(10, 24).expect("ten frames"),
        )
        .expect("clip range");
        let frame_context =
            EffectFrameContext::new(Rational::FPS_24, SampleAspectRatio::SQUARE, range)
                .expect("frame context");
        let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
            &[effect.clone()],
            &[],
            &[],
            &[],
            WorkingColorSpace::LinearRec709,
            frame_context,
            &LutPreparationCache::uncached(),
        )
        .expect("compile selected filter");
        let first = program.evaluate(TimelineTime::ZERO).expect("first frame graph");
        let second = program
            .evaluate(TimelineTime::new(1, 24).expect("second frame"))
            .expect("second frame graph");
        assert_ne!(first.semantic_fingerprint(), second.semantic_fingerprint());
        assert!(matches!(
            PreparedEffectProgram::prepare(&[effect], &[], WorkingColorSpace::LinearRec709,),
            Err(EffectGraphBuildError::InvalidAuthorState { .. })
        ));
    }
}
