//! Borrowed native metadata observations and the existing scalar/structure rules.
//! Views and refusal facts grant no author, processing or Source authority.
use super::clap_discovery::{
    parameter_error_with_capture, ClapParameterDescriptor, ParameterQueryCapture, MAX_INPUT_PORTS,
    MAX_OUTPUT_PORTS, MAX_PARAMETERS,
};
use super::ClapMainAudioInput;
use crate::{
    AudioProcessorAuxiliaryInputContract, AudioProcessorAuxiliaryOutputContract,
    AudioProcessorHostError,
};
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoBuffer, AudioPortType, PluginAudioPorts,
};
use clack_extensions::params::{ParamInfo, ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_host::prelude::{ClapId, PluginMainThreadHandle};
use mondrian_core::AudioChannelLayout;
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
    fn from_info(info: &ParamInfo<'a>, name: &'a str, current_value: f64) -> Self {
        Self {
            id: info.id.get(),
            name,
            min_value: info.min_value,
            max_value: info.max_value,
            default_value: info.default_value,
            current_value,
            automatable: info.flags.contains(ParamInfoFlags::IS_AUTOMATABLE),
            stepped: info.flags.contains(ParamInfoFlags::IS_STEPPED),
            enumeration: info.flags.contains(ParamInfoFlags::IS_ENUM),
            read_only: info.flags.contains(ParamInfoFlags::IS_READONLY),
        }
    }

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

/// One control-thread scan workspace, reused by both parameter scans and the
/// inactive port scan. Its real layout is included in the worker's main-inline
/// stack obligation. Cold error Strings remain outside this allocation claim.
/// Sorted IDs require at most 523,776 u32 moves (<2 MiB) for 1,024 reverse IDs.
pub(super) struct MetadataScanScratch {
    ids: [u32; MAX_PARAMETERS as usize],
    len: usize,
    first_invalid: Option<ParameterFailure>,
    schema_changed: bool,
}

impl MetadataScanScratch {
    pub(super) const fn new() -> Self {
        Self {
            ids: [0; MAX_PARAMETERS as usize],
            len: 0,
            first_invalid: None,
            schema_changed: false,
        }
    }

    fn reset(&mut self) {
        self.len = 0;
        self.first_invalid = None;
        self.schema_changed = false;
    }

    fn insert_id(&mut self, id: u32) -> bool {
        match self.ids[..self.len].binary_search(&id) {
            Ok(_) => false,
            Err(position) => {
                // All callers reject counts above this fixed storage first.
                self.ids.copy_within(position..self.len, position + 1);
                self.ids[position] = id;
                self.len += 1;
                true
            }
        }
    }
}

/// A full single native snapshot, without owned descriptors/names or a tree.
/// Query failures still outrank earlier row/shape errors, exactly as the owned
/// collector which queries every row before validating its captured vector.
pub(super) fn revalidate_parameters(
    handle: &PluginMainThreadHandle<'_>,
    expected: &[ClapParameterDescriptor],
    scratch: &mut MetadataScanScratch,
) -> Result<bool, AudioProcessorHostError> {
    scratch.reset();
    let Some(extension) = handle.get_extension::<PluginParams>() else {
        return Ok(expected.is_empty());
    };
    let count = extension.count(handle);
    if count > MAX_PARAMETERS {
        return Err(invalid("CLAP parameter count exceeds the supported bound"));
    }
    scratch.schema_changed = count as usize != expected.len();
    for index in 0..count {
        // Preserve the original fresh, zero-initialized native query buffer.
        let mut buffer = ParamInfoBuffer::new();
        let (info, name) = parameter_info_at(handle, &extension, index, &mut buffer, None)?;
        let current = parameter_value_at(handle, &extension, info.id, index, None)?;
        let row = ParameterRow::from_info(&info, name, current);
        let duplicate = ClapId::from_raw(row.id).is_some() && !scratch.insert_id(row.id);
        if scratch.first_invalid.is_none()
            && let Some(reason) = parameter_invalid_reason(row, duplicate)
        {
            scratch.first_invalid = Some(ParameterFailure::new(index as usize, row, reason));
        }
        if expected.get(index as usize).is_none_or(|expected| {
            !same_parameter_structure(row, ParameterRow::from_descriptor(expected))
        }) {
            scratch.schema_changed = true;
        }
    }
    if let Some(failure) = scratch.first_invalid {
        return Err(failure.error());
    }
    Ok(!scratch.schema_changed)
}

/// Borrowed ABI traversal shared by discovery's owned collector and retained
/// revalidation. The plugin must be inactive. Invalid native rows fail in the
/// original order; the visitor cannot shortcut a valid shape mismatch.
pub(super) fn scan_audio_ports(
    handle: &PluginMainThreadHandle<'_>,
    layout: AudioChannelLayout,
    scratch: &mut MetadataScanScratch,
    mut visit: impl FnMut(bool, u32, &AudioPortInfo<'_>, AudioChannelLayout),
) -> Result<(ClapMainAudioInput, u32, usize, usize), AudioProcessorHostError> {
    let expected_type = match layout {
        AudioChannelLayout::Mono => AudioPortType::MONO,
        AudioChannelLayout::Stereo => AudioPortType::STEREO,
        _ => return Err(invalid("CLAP audio port layout is unsupported")),
    };
    let ports = handle
        .get_extension::<PluginAudioPorts>()
        .ok_or_else(|| invalid("CLAP plugin does not expose audio ports"))?;
    let mut main_input = ClapMainAudioInput::Absent;
    let mut main_output = None;
    let mut auxiliary_inputs = 0;
    let mut auxiliary_outputs = 0;
    for is_input in [true, false] {
        let count = ports.count(handle, is_input);
        if (is_input && count > MAX_INPUT_PORTS)
            || (!is_input && !(1..=MAX_OUTPUT_PORTS).contains(&count))
        {
            return Err(invalid(
                "CLAP requires bounded inputs and one Main with at most 31 auxiliary outputs",
            ));
        }
        scratch.reset();
        for index in 0..count {
            let mut buffer = AudioPortInfoBuffer::new();
            let port = ports
                .get(handle, index, is_input, &mut buffer)
                .ok_or_else(|| invalid("CLAP audio port metadata is unavailable"))?;
            // Port type supplies channel meaning; count alone never selects a layout.
            // Main and outputs keep the negotiated render layout. Auxiliary inputs
            // may independently declare either of the supported semantic layouts.
            let auxiliary_input = is_input && !port.flags.contains(AudioPortFlags::IS_MAIN);
            let (port_layout, port_type) =
                if auxiliary_input && port.port_type == Some(AudioPortType::MONO) {
                    (AudioChannelLayout::Mono, AudioPortType::MONO)
                } else if auxiliary_input && port.port_type == Some(AudioPortType::STEREO) {
                    (AudioChannelLayout::Stereo, AudioPortType::STEREO)
                } else {
                    (layout, expected_type)
                };
            let refusal = if !scratch.insert_id(port.id.get()) {
                Some("duplicate_id")
            } else if (port.flags.contains(AudioPortFlags::IS_MAIN) && index != 0)
                || (!is_input && index == 0 && !port.flags.contains(AudioPortFlags::IS_MAIN))
            {
                Some("main_role")
            } else if port.port_type != Some(port_type) {
                Some("port_type")
            } else if usize::try_from(port.channel_count).ok() != Some(port_layout.channel_count())
            {
                Some("channel_count")
            } else {
                None
            };
            if let Some(reason) = refusal {
                return Err(super::clap_discovery::audio_port_refusal(
                    reason,
                    is_input,
                    index,
                    count,
                    &port,
                    port_layout.channel_count(),
                ));
            }
            if !is_input && index == 0 {
                main_output = Some(port.id.get());
            } else if !is_input {
                auxiliary_outputs += 1;
            } else if port.flags.contains(AudioPortFlags::IS_MAIN) {
                main_input = ClapMainAudioInput::Present;
            } else {
                auxiliary_inputs += 1;
            }
            visit(is_input, index, &port, port_layout);
            if is_input && !port.flags.contains(AudioPortFlags::IS_MAIN) {
                main_input.validate_auxiliary_count(auxiliary_inputs)?;
            }
        }
    }
    main_input.validate_auxiliary_count(auxiliary_inputs)?;
    Ok((
        main_input,
        main_output.ok_or_else(|| invalid("CLAP Main output is missing"))?,
        auxiliary_inputs,
        auxiliary_outputs,
    ))
}

// Equivalent to the original format!("{prefix}{id}") equality: no signs,
// whitespace or leading zeros, and no reserved native identity.
fn canonical_port_key(key: &str, prefix: &str, id: u32) -> bool {
    if id == u32::MAX {
        return false;
    }
    let Some(digits) = key.strip_prefix(prefix) else {
        return false;
    };
    if digits.is_empty()
        || (digits.len() > 1 && digits.starts_with('0'))
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    digits.parse::<u32>() == Ok(id)
}

pub(super) fn revalidate_audio_ports(
    handle: &PluginMainThreadHandle<'_>,
    layout: AudioChannelLayout,
    main_input: ClapMainAudioInput,
    inputs: &AudioProcessorAuxiliaryInputContract,
    main_output: u32,
    outputs: &AudioProcessorAuxiliaryOutputContract,
    scratch: &mut MetadataScanScratch,
) -> Result<bool, AudioProcessorHostError> {
    let (inputs_match, main_and_outputs_match) = inspect_audio_port_configuration(
        handle,
        layout,
        main_input,
        inputs,
        main_output,
        outputs,
        scratch,
        |_| {},
    )?;
    Ok(inputs_match && main_and_outputs_match)
}

/// One inactive traversal. An input-only proposal cannot relax Main/output
/// compatibility. Both paths retain the original bounded native validation.
pub(super) fn inspect_audio_port_configuration(
    handle: &PluginMainThreadHandle<'_>,
    layout: AudioChannelLayout,
    main_input: ClapMainAudioInput,
    inputs: &AudioProcessorAuxiliaryInputContract,
    main_output: u32,
    outputs: &AudioProcessorAuxiliaryOutputContract,
    scratch: &mut MetadataScanScratch,
    mut visit_input: impl FnMut(super::ClapAudioInputPort),
) -> Result<(bool, bool), AudioProcessorHostError> {
    let mut input_index = 0;
    let mut output_index = 0;
    let mut inputs_match = true;
    let mut outputs_match = true;
    let (actual_input, actual_output, input_count, output_count) = scan_audio_ports(
        handle,
        layout,
        scratch,
        |is_input, index, port, port_layout| {
            if is_input {
                visit_input(super::ClapAudioInputPort {
                    native_id: port.id.get(),
                    channel_layout: port_layout,
                    is_main: port.flags.contains(AudioPortFlags::IS_MAIN),
                });
            }
            if is_input && !port.flags.contains(AudioPortFlags::IS_MAIN) {
                inputs_match &= inputs.buses.get(input_index).is_some_and(|bus| {
                    bus.channel_layout == port_layout
                        && canonical_port_key(&bus.bus_key, "clap.audio-input.", port.id.get())
                });
                input_index += 1;
            } else if !is_input && index != 0 {
                outputs_match &= outputs.buses.get(output_index).is_some_and(|bus| {
                    bus.channel_layout == layout
                        && canonical_port_key(&bus.bus_key, "clap.audio-output.", port.id.get())
                });
                output_index += 1;
            }
        },
    )?;
    Ok((
        inputs_match && input_count == inputs.buses.len(),
        outputs_match
            && actual_input == main_input
            && actual_output == main_output
            && output_count == outputs.buses.len(),
    ))
}
