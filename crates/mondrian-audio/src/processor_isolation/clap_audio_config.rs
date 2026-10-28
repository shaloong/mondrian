//! Cold render-context negotiation, before native state restoration.
//!
//! No config ID is persisted or inferred from an error. A known Mono/Stereo
//! Main-output mismatch may select one unique matching preset. The full port
//! scanner remains authoritative, both immediately after select and after load.
use super::clap_discovery::{MAX_INPUT_PORTS, MAX_OUTPUT_PORTS};
use crate::AudioProcessorHostError;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfoBuffer, AudioPortType, PluginAudioPorts,
};
use clack_extensions::audio_ports_config::{
    AudioPortsConfigBuffer, MainPortInfo, PluginAudioPortsConfig, PluginAudioPortsConfigInfo,
};
use clack_host::plugin::InactivePluginMainThreadHandle;
use clack_host::prelude::ClapId;
use mondrian_core::AudioChannelLayout;

// Cold-query resource bound, unrelated to project or plugin defaults.
const MAX_CONFIGURATIONS: usize = 128;

// Copy only bounded facts; no native string borrow survives another call.
// None is no Main input, Some(false) is a present ordinal Main input.
struct SelectedSummary {
    id: ClapId,
    inputs: u32,
    outputs: u32,
    main_input_named: Option<bool>,
    main_output_named: bool,
}

fn invalid(detail: impl Into<String>) -> AudioProcessorHostError {
    AudioProcessorHostError::InvalidContract(detail.into())
}

/// Requires an inactive main-thread borrow. Never call after native load or on
/// a stopped retained instance: those already have an admitted native topology.
pub(super) fn negotiate_initial(
    handle: &InactivePluginMainThreadHandle<'_>,
    layout: AudioChannelLayout,
) -> Result<bool, AudioProcessorHostError> {
    let expected_type = match layout {
        AudioChannelLayout::Mono => AudioPortType::MONO,
        AudioChannelLayout::Stereo => AudioPortType::STEREO,
        _ => return Ok(false), // Existing complete admission reports unsupported layouts.
    };
    let Some(configs) = handle.get_extension::<PluginAudioPortsConfig>() else {
        return Ok(false);
    };
    let Some(ports) = handle.get_extension::<PluginAudioPorts>() else {
        return Ok(false);
    };
    if !(1..=MAX_OUTPUT_PORTS).contains(&ports.count(handle, false)) {
        return Ok(false);
    }
    // Preserve every already-compatible preset. In particular, do not select
    // between different sidechains/effect modes with identical Main layouts.
    let mut buffer = AudioPortInfoBuffer::new();
    let Some(output) = ports.get(handle, 0, false, &mut buffer) else {
        return Ok(false);
    };
    if !output.flags.contains(AudioPortFlags::IS_MAIN) {
        return Ok(false);
    }
    let current_layout = match output.channel_count {
        1 if output.port_type.is_none() || output.port_type == Some(AudioPortType::MONO) => {
            AudioChannelLayout::Mono
        }
        2 if output.port_type.is_none() || output.port_type == Some(AudioPortType::STEREO) => {
            AudioChannelLayout::Stereo
        }
        _ => return Ok(false),
    };
    if current_layout == layout {
        return Ok(false);
    }

    let count = configs.count(handle) as usize;
    if count > MAX_CONFIGURATIONS {
        return Err(invalid(
            "CLAP audio configuration list exceeds cold-query capacity",
        ));
    }
    let mut ids = [0; MAX_CONFIGURATIONS];
    let mut candidate = None;
    let mut ambiguous = false;
    for index in 0..count {
        let mut buffer = AudioPortsConfigBuffer::new();
        let config = configs
            .get(handle, index as u32, &mut buffer)
            .ok_or_else(|| invalid("CLAP audio configuration metadata is unavailable"))?;
        if ids[..index].contains(&config.id.get()) {
            return Err(invalid("CLAP audio configuration IDs are duplicated"));
        }
        ids[index] = config.id.get();
        if (config.main_input.is_some() && config.input_port_count == 0)
            || (config.main_output.is_some() && config.output_port_count == 0)
        {
            return Err(invalid(
                "CLAP audio configuration Main counts are contradictory",
            ));
        }
        let matches = |main: MainPortInfo<'_>| {
            main.channel_count as usize == layout.channel_count()
                && (main.port_type.is_none() || main.port_type == Some(expected_type))
        };
        if config.input_port_count <= MAX_INPUT_PORTS
            && (1..=MAX_OUTPUT_PORTS).contains(&config.output_port_count)
            && config.main_input.is_none_or(matches)
            && config.main_output.is_some_and(matches)
        {
            ambiguous |= candidate.is_some();
            candidate = Some(SelectedSummary {
                id: config.id,
                inputs: config.input_port_count,
                outputs: config.output_port_count,
                main_input_named: config.main_input.map(|main| main.port_type.is_some()),
                main_output_named: config.main_output.is_some_and(|main| main.port_type.is_some()),
            });
        }
    }
    if ambiguous {
        return Err(invalid(
            "CLAP audio configuration requires an explicit choice: multiple matching Main layouts",
        ));
    }
    let Some(selected) = candidate else {
        return Ok(false); // The original port scanner reports the actual mismatch.
    };
    configs.select(handle, selected.id).map_err(|error| {
        AudioProcessorHostError::Unavailable(format!(
            "CLAP audio configuration select failed: {error}"
        ))
    })?;
    if let Some(info) = handle.get_extension::<PluginAudioPortsConfigInfo>()
        && info.current_config(handle) != Some(selected.id)
    {
        return Err(invalid(
            "CLAP selected audio configuration ID was not applied",
        ));
    }
    // A summary cannot establish Aux/output layouts, IDs or roles. Always use
    // the existing full scanner; do not activate based on the summary alone.
    let mut scratch = super::clap_metadata::MetadataScanScratch::new();
    let mut main_input_named = None;
    let mut main_output_named = None;
    let (main, _, auxiliary_inputs, auxiliary_outputs) = super::clap_metadata::scan_audio_ports(
        handle,
        layout,
        &mut scratch,
        |is_input, index, port, _| {
            if is_input && port.flags.contains(AudioPortFlags::IS_MAIN) {
                main_input_named = Some(port.port_type.is_some());
            } else if !is_input && index == 0 {
                main_output_named = Some(port.port_type.is_some());
            }
        },
    )?;
    let actual_inputs = auxiliary_inputs + usize::from(main == super::ClapMainAudioInput::Present);
    if actual_inputs != selected.inputs as usize
        || auxiliary_outputs + 1 != selected.outputs as usize
    {
        return Err(invalid(
            "CLAP selected audio configuration port counts disagree with its summary",
        ));
    }
    if main_input_named != selected.main_input_named
        || main_output_named != Some(selected.main_output_named)
    {
        return Err(invalid(
            "CLAP selected audio configuration Main roles or types disagree with its summary",
        ));
    }
    Ok(true)
}
