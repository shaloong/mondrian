//! Borrowed native metadata observations and the existing scalar/structure rules.
//! Views and refusal facts grant no author, processing or Source authority.
use super::clap_discovery::{
    parameter_error_with_capture, ClapParameterDescriptor, ParameterQueryCapture,
};
use crate::AudioProcessorHostError;
use clack_extensions::params::{ParamInfo, ParamInfoBuffer, PluginParams};
use clack_host::prelude::{ClapId, PluginMainThreadHandle};
fn invalid(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.into())
}
#[derive(Clone, Copy)]
pub(super) struct ParameterRow<'a> {
    id: u32,
    name: &'a str,
    min_value: f64,
    max_value: f64,
    default_value: f64,
    current_value: f64,
    automatable: bool,
    stepped: bool,
    enumeration: bool,
    read_only: bool,
}
impl<'a> ParameterRow<'a> {
    pub(super) fn from_descriptor(value: &'a ClapParameterDescriptor) -> Self {
        Self {
            id: value.id,
            name: &value.name,
            min_value: value.min_value,
            max_value: value.max_value,
            default_value: value.default_value,
            current_value: value.current_value,
            automatable: value.automatable,
            stepped: value.stepped,
            enumeration: value.enumeration,
            read_only: value.read_only,
        }
    }
}
pub(super) fn parameter_info_at<'b>(
    handle: &PluginMainThreadHandle<'_>,
    extension: &PluginParams,
    index: u32,
    buffer: &'b mut ParamInfoBuffer,
    mut capture: Option<&mut ParameterQueryCapture>,
) -> Result<(ParamInfo<'b>, &'b str), AudioProcessorHostError> {
    let info = match capture.as_deref_mut() {
        Some(capture) => {
            capture.begin_row(index);
            extension.get_info_observed(handle, index, buffer, |info| {
                capture.record_info(index, info)
            })
        }
        None => extension.get_info(handle, index, buffer),
    }
    .ok_or_else(|| {
        parameter_error_with_capture(
            invalid("CLAP parameter metadata is missing"),
            capture.as_deref(),
            Some(index as usize),
            None,
        )
    })?;
    let name = std::str::from_utf8(info.name).map_err(|_| {
        parameter_error_with_capture(
            invalid("CLAP parameter name is not UTF-8"),
            capture.as_deref(),
            Some(index as usize),
            None,
        )
    })?;
    Ok((info, name))
}
pub(super) fn parameter_value_at(
    handle: &PluginMainThreadHandle<'_>,
    extension: &PluginParams,
    id: ClapId,
    index: u32,
    mut capture: Option<&mut ParameterQueryCapture>,
) -> Result<f64, AudioProcessorHostError> {
    match capture.as_deref_mut() {
        Some(capture) => {
            extension.get_value_observed(handle, id, |value| capture.record_value(index, value))
        }
        None => extension.get_value(handle, id),
    }
    .ok_or_else(|| {
        parameter_error_with_capture(
            invalid("CLAP parameter current value is unavailable"),
            capture.as_deref(),
            Some(index as usize),
            None,
        )
    })
}
pub(super) fn parameter_invalid_reason(
    parameter: ParameterRow<'_>,
    duplicate: bool,
) -> Option<&'static str> {
    if ClapId::from_raw(parameter.id).is_none() {
        Some("reserved_id")
    } else if duplicate {
        Some("duplicate_id")
    } else if parameter.name.is_empty() {
        Some("empty_name")
    } else if parameter.name.len() > 256 {
        Some("name_bytes_exceed_256")
    } else if parameter.name.chars().any(char::is_control) {
        Some("name_contains_control")
    } else if !parameter.min_value.is_finite() {
        Some("min_nonfinite")
    } else if !parameter.max_value.is_finite() {
        Some("max_nonfinite")
    } else if !parameter.default_value.is_finite() {
        Some("default_nonfinite")
    } else if !parameter.current_value.is_finite() {
        Some("current_nonfinite")
    } else if parameter.min_value > parameter.max_value {
        Some("reversed_range")
    } else if !(parameter.min_value..=parameter.max_value).contains(&parameter.default_value) {
        Some("default_out_of_range")
    } else if !(parameter.min_value..=parameter.max_value).contains(&parameter.current_value) {
        Some("current_out_of_range")
    } else if parameter.enumeration && !parameter.stepped {
        Some("enum_not_stepped")
    } else if parameter.stepped {
        [
            parameter.min_value,
            parameter.max_value,
            parameter.default_value,
            parameter.current_value,
        ]
        .into_iter()
        .enumerate()
        .find_map(|(field, value)| {
            // Preserve field order and fractional-before-precision checks.
            if value.fract() != 0.0 {
                Some(
                    [
                        "stepped_min_fractional",
                        "stepped_max_fractional",
                        "stepped_default_fractional",
                        "stepped_current_fractional",
                    ][field],
                )
            } else if value.abs() > 9_007_199_254_740_992.0 {
                Some(
                    [
                        "stepped_min_precision_limit",
                        "stepped_max_precision_limit",
                        "stepped_default_precision_limit",
                        "stepped_current_precision_limit",
                    ][field],
                )
            } else {
                None
            }
        })
    } else {
        None
    }
}
/// The first invalid row's fixed facts; no borrowed native buffer survives a query.
#[derive(Clone, Copy)]
pub(super) struct ParameterFailure {
    index: usize,
    id: u32,
    reason: &'static str,
    bits: [u64; 4],
}
impl ParameterFailure {
    pub(super) fn new(index: usize, row: ParameterRow<'_>, reason: &'static str) -> Self {
        Self {
            index,
            id: row.id,
            reason,
            bits: [
                row.min_value.to_bits(),
                row.max_value.to_bits(),
                row.default_value.to_bits(),
                row.current_value.to_bits(),
            ],
        }
    }
    pub(super) fn error(self) -> AudioProcessorHostError {
        invalid(format!("CLAP parameter metadata is invalid or ambiguous: index={} native_id={} reason={} min_bits={:016X} max_bits={:016X} default_bits={:016X} current_bits={:016X}",self.index,self.id,self.reason,self.bits[0],self.bits[1],self.bits[2],self.bits[3]))
    }
}
/// Definition facts only: current values, display name and hidden stay excluded.
pub(super) fn same_parameter_structure(a: ParameterRow<'_>, e: ParameterRow<'_>) -> bool {
    a.id == e.id
        && a.min_value == e.min_value
        && a.max_value == e.max_value
        && a.default_value == e.default_value
        && a.automatable == e.automatable
        && a.stepped == e.stepped
        && a.enumeration == e.enumeration
        && a.read_only == e.read_only
}
