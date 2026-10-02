//! Bind author resource input to its explicit Project directory before commit.

use super::AppState;
use mondrian_core::automation::ParameterValueWrite;
use mondrian_core::{MondrianError, ParameterResourceReference, PropertyValue, Result};
use std::borrow::Cow;

impl AppState {
    /// Bind incoming graph values without changing definition defaults or author identities.
    /// Resource parameters cannot carry animation, as enforced by graph validation.
    pub(crate) fn resolve_grade_graph_resources(
        &self,
        graph: &mut mondrian_core::GradeGraph,
    ) -> Result<()> {
        graph.validate_author_state()?;
        for node in &mut graph.nodes {
            let mondrian_core::GradeGraphNodeKind::Effect { effect, .. } = &mut node.kind else {
                continue;
            };
            let mut writes = Vec::new();
            for (path, property) in effect.properties.iter() {
                if !matches!(property.static_value(), PropertyValue::Resource(
                    ParameterResourceReference::ExternalFile { path }) if !path.is_absolute())
                {
                    continue;
                }
                let write = ParameterValueWrite::Value {
                    parameter: property.address(),
                    value: property.static_value().clone(),
                };
                if let Cow::Owned(ParameterValueWrite::Value { value, .. }) =
                    self.resolve_resource_parameter_write(&write)?
                {
                    writes.push((path.to_owned(), value));
                }
            }
            for (path, value) in writes {
                effect.properties.set_static_value(&path, value)?;
            }
        }
        graph.validate_author_state()
    }

    /// Resolve complete external resource writes before schema admission.
    /// Already-bound resources and non-resource/channel edits allocate nothing.
    pub(crate) fn resolve_resource_parameter_write<'a>(
        &self,
        write: &'a ParameterValueWrite,
    ) -> Result<Cow<'a, ParameterValueWrite>> {
        let ParameterValueWrite::Value {
            parameter,
            value:
                PropertyValue::Resource(reference @ ParameterResourceReference::ExternalFile { path }),
        } = write
        else {
            return Ok(Cow::Borrowed(write));
        };
        if path.is_absolute() {
            return Ok(Cow::Borrowed(write));
        }
        let directory =
            self.current_project_path().and_then(std::path::Path::parent).ok_or_else(|| {
                MondrianError::WorkflowStepFailed {
                    step_id: "resource_path_owner".into(),
                    reason: "relative resources require an open Project directory".into(),
                }
            })?;
        let mut reference = reference.clone();
        reference.resolve_external_path(directory).map_err(|error| {
            MondrianError::WorkflowStepFailed {
                step_id: "resource_path_owner".into(),
                reason: error.to_string(),
            }
        })?;
        Ok(Cow::Owned(ParameterValueWrite::Value {
            parameter: parameter.clone(),
            value: PropertyValue::Resource(reference),
        }))
    }
}
