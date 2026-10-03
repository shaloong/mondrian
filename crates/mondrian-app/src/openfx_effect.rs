//! Bind a described, installed OpenFX Filter to the shared visual effect graph.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mondrian_core::automation::{
    ParameterInvalidValuePolicy, ParameterNumericContract, ParameterNumericRange,
    ParameterResourceContract, ParameterResourceReference, ParameterUnit, PropertyBag,
    PropertyDescriptor, PropertyValue, ResourcePathKind, ResourcePathSelection,
};
use mondrian_core::{MondrianError, ParameterId};
use mondrian_effects::{
    register_effect_definition, EffectCachePolicy, EffectColorDomainContract, EffectDefinition,
    EffectDefinitionError, EffectDeterminism, EffectExecutionContract, EffectExecutionModes,
    EffectFloatProcessorFactory, EffectFloatProcessorInstance, EffectFloatProcessorPreparation,
    EffectGraphBuildError, EffectGraphTopology, EffectPluginContract, EffectProcessorControl,
    EffectProcessorStopReason, EffectResourceLifetime, EffectRoiPropagation, EffectStateModel,
    EffectTemporalInputExtent, EffectType,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::openfx_adapter::OpenFxBinaryInspection;
use crate::openfx_host::{
    describe_openfx_filter, OpenFxDoubleType, OpenFxFilterDescription, OpenFxFilterSession,
    OpenFxFloatFrame, OpenFxHostError, OpenFxParameterValue, OpenFxRenderTiming, OpenFxStringMode,
    OPENFX_MAX_STRING_BYTES,
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
    #[error(transparent)]
    Registry(#[from] EffectDefinitionError),
}

#[derive(Clone)]
struct BoundParameter {
    name: String,
    parameter_id: Option<ParameterId>,
    default_value: OpenFxParameterValue,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenFxGraphParameters {
    timing: OpenFxRenderTiming,
    values: BTreeMap<String, OpenFxParameterValue>,
}

/// A definition retains only an immutable selection, never a native instance.
#[derive(Debug)]
struct OpenFxProcessorFactory {
    helper: PathBuf,
    inspection: OpenFxBinaryInspection,
    identifier: String,
}

#[derive(Debug)]
struct OpenFxProcessorInstance {
    session: OpenFxFilterSession,
    width: u32,
    height: u32,
}

fn processor_checkpoint(control: &mut EffectProcessorControl<'_>) -> Result<(), OpenFxHostError> {
    control.check().map_err(|_| match control.stop_reason() {
        Some(EffectProcessorStopReason::DeadlineExpired) => OpenFxHostError::DeadlineExceeded,
        _ => OpenFxHostError::Canceled,
    })
}

fn processor_host_error(
    error: OpenFxHostError,
    control: &EffectProcessorControl<'_>,
) -> MondrianError {
    if control.stop_reason().is_some() {
        MondrianError::Cancelled
    } else {
        // An adapter transport timeout is a worker fault. Only an observed
        // scheduler stop may report cancellation without runtime quarantine.
        MondrianError::WorkflowStepFailed {
            step_id: "openfx_filter_execution".to_owned(),
            reason: error.to_string(),
        }
    }
}

impl EffectFloatProcessorFactory for OpenFxProcessorFactory {
    fn resident_bytes(
        &self,
        request: EffectFloatProcessorPreparation,
    ) -> mondrian_core::Result<usize> {
        OpenFxFilterSession::required_frame_storage(request.width, request.height).map_err(
            |error| MondrianError::WorkflowStepFailed {
                step_id: "openfx_filter_frame_contract".to_owned(),
                reason: error.to_string(),
            },
        )
    }

    fn prepare(
        &self,
        request: EffectFloatProcessorPreparation,
        control: &mut EffectProcessorControl<'_>,
    ) -> mondrian_core::Result<Box<dyn EffectFloatProcessorInstance>> {
        control.check()?;
        let result = {
            let mut checkpoint = || processor_checkpoint(control);
            OpenFxFilterSession::prepare(
                &self.helper,
                &self.inspection,
                &self.identifier,
                request.width,
                request.height,
                &mut checkpoint,
            )
        };
        let session = result.map_err(|error| processor_host_error(error, control))?;
        Ok(Box::new(OpenFxProcessorInstance {
            session,
            width: request.width,
            height: request.height,
        }))
    }
}

impl EffectFloatProcessorInstance for OpenFxProcessorInstance {
    fn process(
        &mut self,
        pixels: &mut Vec<[f32; 4]>,
        params: &serde_json::Value,
        _frame_seed: i64,
        control: &mut EffectProcessorControl<'_>,
    ) -> mondrian_core::Result<()> {
        control.check()?;
        let parameters: OpenFxGraphParameters =
            serde_json::from_value(params.clone()).map_err(|error| {
                MondrianError::WorkflowStepFailed {
                    step_id: "openfx_graph_parameters".to_owned(),
                    reason: error.to_string(),
                }
            })?;
        // This vector is the executor's atomic stage. Moving it into the host
        // envelope introduces no extra input raster or definition-global state.
        let frame = OpenFxFloatFrame {
            width: self.width,
            height: self.height,
            pixels: std::mem::take(pixels),
        };
        let result = {
            let mut checkpoint = || processor_checkpoint(control);
            self.session.render(
                &parameters.timing,
                &parameters.values,
                &frame,
                &mut checkpoint,
            )
        };
        *pixels = frame.pixels;
        let output = result.map_err(|error| processor_host_error(error, control))?;
        control.check()?;
        *pixels = output.pixels;
        Ok(())
    }
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
    let effect_type = effect_type_for_filter(inspection, plugin_identifier)?;
    let definition = build_definition(helper_executable, inspection, description, &effect_type)?;
    register_effect_definition(definition)?;
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
        if matches!(parameter.default_value, OpenFxParameterValue::String(_))
            && !matches!(
                parameter.string_mode,
                Some(
                    OpenFxStringMode::SingleLine
                        | OpenFxStringMode::MultiLine
                        | OpenFxStringMode::FilePath { .. }
                        | OpenFxStringMode::DirectoryPath { .. }
                )
            )
        {
            return Err(OpenFxEffectRegistrationError::Schema(format!(
                "parameter `{}` requires a read-only label or rich-text authoring contract",
                parameter.name,
            )));
        }
        let editable = !parameter.secret && parameter.enabled;
        let parameter_id = if editable {
            let mut identity = description.identifier.as_bytes().to_vec();
            identity.push(0);
            identity.extend_from_slice(parameter.name.as_bytes());
            let parameter_id = ParameterId::new(stable_key("openfx.param.", &identity))
                .map_err(|error| OpenFxEffectRegistrationError::Schema(error.to_string()))?;
            let path = stable_key("openfx.control.", parameter.name.as_bytes());
            let default_value = match parameter.default_value.clone() {
                OpenFxParameterValue::Double(value) => PropertyValue::Double(value),
                OpenFxParameterValue::Boolean(value) => PropertyValue::Bool(value),
                OpenFxParameterValue::Integer(value) => PropertyValue::Int(i64::from(value)),
                OpenFxParameterValue::Choice(value) => PropertyValue::Enum(value.to_string()),
                OpenFxParameterValue::StrChoice(key) => PropertyValue::Enum(key),
                OpenFxParameterValue::String(text) => match parameter.string_mode {
                    Some(
                        OpenFxStringMode::FilePath { .. } | OpenFxStringMode::DirectoryPath { .. },
                    ) => PropertyValue::Resource(if text.is_empty() {
                        ParameterResourceReference::Unbound
                    } else {
                        ParameterResourceReference::ExternalFile {
                            path: PathBuf::from(text).into(),
                        }
                    }),
                    _ => PropertyValue::Text(text),
                },
            };
            let mut descriptor = PropertyDescriptor::try_new(path, &parameter.label, default_value)
                .map_err(|error| OpenFxEffectRegistrationError::Schema(error.to_string()))?
                .with_parameter_id(parameter_id.clone())
                .with_animatable(parameter.can_animate);
            if let Some(mode) = parameter.string_mode {
                descriptor = match mode {
                    OpenFxStringMode::SingleLine => {
                        descriptor.with_text_mode(mondrian_core::ParameterTextMode::SingleLine)
                    }
                    OpenFxStringMode::MultiLine => {
                        descriptor.with_text_mode(mondrian_core::ParameterTextMode::MultiLine)
                    }
                    OpenFxStringMode::FilePath { must_exist }
                    | OpenFxStringMode::DirectoryPath { must_exist } => {
                        let kind = if matches!(mode, OpenFxStringMode::FilePath { .. }) {
                            ResourcePathKind::File
                        } else {
                            ResourcePathKind::Directory
                        };
                        let selection = if must_exist {
                            ResourcePathSelection::Existing
                        } else {
                            ResourcePathSelection::ExistingOrNew
                        };
                        descriptor.with_resource_contract(
                            ParameterResourceContract::external_utf8_path(
                                kind,
                                selection,
                                OPENFX_MAX_STRING_BYTES,
                            ),
                        )
                    }
                    _ => {
                        return Err(OpenFxEffectRegistrationError::Schema(
                            "unsupported String authoring contract".to_owned(),
                        ))
                    }
                };
            }
            if matches!(
                parameter.default_value,
                OpenFxParameterValue::Choice(_) | OpenFxParameterValue::StrChoice(_)
            ) {
                descriptor = descriptor.with_enum_options(
                    parameter
                        .choices
                        .iter()
                        .enumerate()
                        .map(|(index, option)| {
                            let mut choice = mondrian_core::automation::ParameterEnumOption::new(
                                &option.key,
                                &option.label,
                                format!("{parameter_id}.choice.{index}"),
                            );
                            if let Some(rank) = option.display_rank {
                                choice = choice.with_display_rank(rank);
                            }
                            choice
                        })
                        .collect(),
                );
            }
            if matches!(parameter.default_value, OpenFxParameterValue::Integer(_)) {
                descriptor.schema.allowed_interpolations = vec![
                    mondrian_core::automation::ParameterInterpolation::Hold,
                    mondrian_core::automation::ParameterInterpolation::Linear,
                    mondrian_core::automation::ParameterInterpolation::Bezier,
                ];
            }
            let preferred = parameter.preferred_interpolation;
            if !descriptor.schema.allowed_interpolations.contains(&preferred) {
                return Err(OpenFxEffectRegistrationError::Schema(format!(
                    "parameter `{}` declares unsupported default interpolation",
                    parameter.name
                )));
            }
            descriptor.schema.allowed_interpolations.retain(|mode| *mode != preferred);
            descriptor.schema.allowed_interpolations.insert(0, preferred);
            descriptor.ui_metadata.display_order = Some(index as u32);
            descriptor.ui_metadata.group_name = Some("OpenFX".to_owned());
            if matches!(
                parameter.default_value,
                OpenFxParameterValue::Double(_) | OpenFxParameterValue::Integer(_)
            ) {
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
                    matches!(parameter.default_value, OpenFxParameterValue::Integer(_))
                        .then_some(1.0),
                    ParameterInvalidValuePolicy::Reject,
                )
                .map_err(|error| OpenFxEffectRegistrationError::Schema(error.to_string()))?;
                let unit = match parameter.double_type {
                    Some(OpenFxDoubleType::Plain | OpenFxDoubleType::Scale) => {
                        ParameterUnit::Unitless
                    }
                    None if matches!(parameter.default_value, OpenFxParameterValue::Integer(_)) => {
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
                                OpenFxParameterValue::Double(value)
                            }
                            Some(PropertyValue::Enum(key))
                                if matches!(
                                    parameter.default_value,
                                    OpenFxParameterValue::Choice(_)
                                ) =>
                            {
                                OpenFxParameterValue::Choice(key.parse::<i32>().map_err(|_| {
                                    invalid(format!(
                                        "parameter `{}` has invalid Choice key",
                                        parameter.name
                                    ))
                                })?)
                            }
                            Some(PropertyValue::Enum(key))
                                if matches!(
                                    parameter.default_value,
                                    OpenFxParameterValue::StrChoice(_)
                                ) =>
                            {
                                OpenFxParameterValue::StrChoice(key)
                            }
                            Some(PropertyValue::Text(text))
                                if matches!(
                                    parameter.default_value,
                                    OpenFxParameterValue::String(_)
                                ) =>
                            {
                                OpenFxParameterValue::String(text)
                            }
                            Some(PropertyValue::Resource(reference))
                                if matches!(
                                    parameter.default_value,
                                    OpenFxParameterValue::String(_)
                                ) =>
                            {
                                let text = match reference {
                                    ParameterResourceReference::Unbound => String::new(),
                                    ParameterResourceReference::ExternalFile { path }
                                        if path.is_absolute() =>
                                    {
                                        path.into_native_path()
                                            .map_err(|error| invalid(error.to_string()))?
                                            .to_str()
                                            .ok_or_else(|| {
                                                invalid(format!(
                                                    "parameter `{}` requires a UTF-8 path",
                                                    parameter.name
                                                ))
                                            })?
                                            .to_owned()
                                    }
                                    _ => {
                                        return Err(invalid(format!(
                                            "parameter `{}` requires an external path",
                                            parameter.name
                                        )))
                                    }
                                };
                                OpenFxParameterValue::String(text)
                            }
                            Some(PropertyValue::Bool(value)) => {
                                OpenFxParameterValue::Boolean(value)
                            }
                            Some(PropertyValue::Int(value)) => OpenFxParameterValue::Integer(
                                i32::try_from(value).map_err(|_| {
                                    invalid(format!(
                                        "parameter `{}` exceeds the OFX Integer range",
                                        parameter.name
                                    ))
                                })?,
                            ),
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
    let processor = Arc::new(OpenFxProcessorFactory {
        helper: helper_executable.to_path_buf(),
        inspection: inspection.clone(),
        identifier: description.identifier.clone(),
    });
    Ok(EffectDefinition::new(
        effect_type.key(),
        description.label,
        properties,
        EffectColorDomainContract::SCENE_LINEAR,
    )
    .with_category(vec!["插件".to_owned(), "OpenFX".to_owned()])
    .with_execution_contract(EffectExecutionContract {
        execution_modes: EffectExecutionModes::CPU_F32,
        determinism: EffectDeterminism::Nondeterministic,
        state_model: EffectStateModel::Stateless,
        temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
        roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
        resource_lifetime: EffectResourceLifetime::Frame,
        topology: EffectGraphTopology::LinearChain,
    })
    .with_custom_float_processor_factory(
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

fn effect_type_for_filter(
    inspection: &OpenFxBinaryInspection,
    plugin_identifier: &str,
) -> Result<EffectType, OpenFxEffectRegistrationError> {
    let plugin = inspection
        .plugins
        .iter()
        .find(|plugin| plugin.identifier == plugin_identifier)
        .ok_or_else(|| {
            OpenFxEffectRegistrationError::Schema("selected plugin is missing".into())
        })?;
    let identity = format!(
        "{}\0{}.{}\0{}",
        plugin_identifier, plugin.version_major, plugin.version_minor, inspection.binary_sha256
    );
    Ok(EffectType::Plugin(stable_key(
        "openfx.",
        identity.as_bytes(),
    )))
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
    fn native_factory_sizes_controlled_frames_before_loading_a_binary() {
        let request = EffectFloatProcessorPreparation {
            effect_id: mondrian_core::EffectId::new(),
            width: 2,
            height: 3,
        };
        assert_eq!(
            OpenFxFilterSession::required_frame_storage(request.width, request.height)
                .expect("frame charge"),
            3 * 2 * 3 * 16
        );
        assert!(OpenFxFilterSession::required_frame_storage(0, 1).is_err());
        assert!(OpenFxFilterSession::required_frame_storage(u32::MAX, u32::MAX).is_err());
    }

    #[test]
    fn native_transport_faults_do_not_invent_scheduler_cancellation() {
        let control = EffectProcessorControl::uncancelled();
        assert!(matches!(
            processor_host_error(OpenFxHostError::DeadlineExceeded, &control),
            MondrianError::WorkflowStepFailed { .. }
        ));
        let mut deadline = || Some(EffectProcessorStopReason::DeadlineExpired);
        let mut control = EffectProcessorControl::new(&mut deadline);
        assert!(matches!(
            processor_checkpoint(&mut control),
            Err(OpenFxHostError::DeadlineExceeded)
        ));
        assert!(matches!(
            processor_host_error(OpenFxHostError::DeadlineExceeded, &control),
            MondrianError::Cancelled
        ));
    }

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
            label: "Example Gain".to_owned(),
            parameters: vec![
                crate::openfx_host::OpenFxParameterDescription {
                    name: "gain".to_owned(),
                    label: "Gain".to_owned(),
                    hint: "Multiply every channel".to_owned(),
                    default_value: OpenFxParameterValue::Double(1.0),
                    choices: Vec::new(),
                    preferred_interpolation:
                        mondrian_core::automation::ParameterInterpolation::Linear,
                    double_type: Some(OpenFxDoubleType::Scale),
                    string_mode: None,
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
                    default_value: OpenFxParameterValue::Boolean(false),
                    choices: Vec::new(),
                    preferred_interpolation:
                        mondrian_core::automation::ParameterInterpolation::Hold,
                    double_type: None,
                    string_mode: None,
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
        let effect_type = effect_type_for_filter(&inspection, identifier).expect("effect identity");
        let definition = build_definition(
            Path::new("C:/selected/mondrian.exe"),
            &inspection,
            description,
            &effect_type,
        )
        .expect("build selected filter definition");
        assert_eq!(definition.display_name(), "Example Gain");
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

    #[test]
    fn binary_revision_changes_effect_identity_without_rebinding_old_instances() {
        let inspection = OpenFxBinaryInspection {
            binary_path: std::path::PathBuf::from("C:/selected/Basic.ofx"),
            binary_sha256: "a".repeat(64),
            plugins: vec![OpenFxPluginDescriptor {
                identifier: "test.mondrian.openfx.revision".to_owned(),
                api_version: 1,
                version_major: 1,
                version_minor: 0,
            }],
        };
        let original = effect_type_for_filter(&inspection, &inspection.plugins[0].identifier)
            .expect("original identity");
        let mut changed = inspection.clone();
        changed.binary_sha256 = "b".repeat(64);
        let updated = effect_type_for_filter(&changed, &changed.plugins[0].identifier)
            .expect("updated identity");
        assert_ne!(original, updated);
        changed.binary_sha256 = inspection.binary_sha256.clone();
        changed.plugins[0].version_minor = 1;
        let versioned = effect_type_for_filter(&changed, &changed.plugins[0].identifier)
            .expect("versioned identity");
        assert_ne!(original, versioned);
        assert_eq!(
            original,
            effect_type_for_filter(&inspection, &inspection.plugins[0].identifier)
                .expect("stable identity")
        );
    }
}
