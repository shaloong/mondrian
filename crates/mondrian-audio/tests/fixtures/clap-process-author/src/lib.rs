//! Owned CLAP effect producing one recordable parameter change from actual
//! Process at sample offset three. No GUI or State extension is advertised.
#![deny(unsafe_code)]

use clack_extensions::{audio_ports::*, params::*};
use clack_plugin::events::event_types::{ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

/// Stable native parameter used by the independent fixture.
const PARAMETER: ClapId = ClapId::new(17);
/// Independent new plain value; not copied from any host author proposal.
const OBSERVED: f64 = 0.625;

/// The only plugin class in this owned fixture.
pub struct ProcessAuthor;
/// Native shared plain value queried from the main role.
pub struct Shared {
    value: AtomicU64,
}
/// Same-instance main metadata owner.
pub struct Main<'a> {
    shared: &'a Shared,
}
/// Same-instance DSP role, with one bounded producer publication per activation.
pub struct Dsp<'a> {
    shared: &'a Shared,
    emitted: bool,
}

impl Plugin for ProcessAuthor {
    type Shared<'a> = Shared;
    type MainThread<'a> = Main<'a>;
    type AudioProcessor<'a> = Dsp<'a>;
    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder.register::<PluginAudioPorts>().register::<PluginParams>();
    }
}
impl DefaultPluginFactory for ProcessAuthor {
    fn get_descriptor() -> PluginDescriptor {
        use clack_plugin::plugin::features::*;
        PluginDescriptor::new("org.mondrian.fixture.process-author", "Mondrian Process Author")
            .with_features([AUDIO_EFFECT, STEREO])
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<Shared, PluginError> {
        Ok(Shared { value: AtomicU64::new(1.0_f64.to_bits()) })
    }
    fn new_main_thread<'a>(_host: HostMainThreadHandle<'a>, shared: &'a Shared)
        -> Result<Main<'a>, PluginError> {
        Ok(Main { shared })
    }
}
impl PluginShared<'_> for Shared {}
impl<'a> PluginMainThread<'a, Shared> for Main<'a> {}

impl Shared {
    fn value(&self) -> f64 { f64::from_bits(self.value.load(Ordering::Acquire)) }
    fn apply(&self, input: &InputEvents) {
        for event in input {
            if let Some(CoreEventSpace::ParamValue(value)) = event.as_core_event()
                && value.param_id() == PARAMETER {
                self.value.store(value.value().to_bits(), Ordering::Release);
            }
        }
    }
}
impl PluginAudioPortsImpl for Main<'_> {
    fn count(&self, _input: bool) -> u32 { 1 }
    fn get(&self, index: u32, _input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(0), name: b"main", channel_count: 2,
                flags: AudioPortFlags::IS_MAIN, port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            });
        }
    }
}
impl PluginMainThreadParams for Main<'_> {
    fn count(&self) -> u32 { 1 }
    fn get_info(&self, index: u32, writer: &mut ParamInfoWriter) {
        if index == 0 {
            writer.set(&ParamInfo {
                id: PARAMETER, flags: ParamInfoFlags::IS_AUTOMATABLE,
                cookie: Default::default(), name: b"Gain", module: b"",
                min_value: 0.0, max_value: 1.0, default_value: 1.0,
            });
        }
    }
    fn get_value(&self, id: ClapId) -> Option<f64> {
        (id == PARAMETER).then(|| self.shared.value())
    }
    fn value_to_text(&self, id: ClapId, value: f64, writer: &mut ParamDisplayWriter)
        -> std::fmt::Result {
        if id == PARAMETER { write!(writer, "{value}") } else { Err(std::fmt::Error) }
    }
    fn text_to_value(&self, id: ClapId, text: &CStr) -> Option<f64> {
        if id != PARAMETER { return None; }
        text.to_str().ok()?.parse::<f64>().ok().filter(|value| value.is_finite())
    }
    fn flush(&self, input: &InputEvents, _output: &mut OutputEvents) {
        self.shared.apply(input);
    }
}
impl PluginAudioProcessorParams for Dsp<'_> {
    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        self.shared.apply(input);
    }
}
impl<'a> PluginAudioProcessor<'a, Shared, Main<'a>> for Dsp<'a> {
    fn activate(_host: HostAudioProcessorHandle<'a>, _main: &Main<'a>, shared: &'a Shared,
        _configuration: PluginAudioConfiguration) -> Result<Self, PluginError> {
        Ok(Self { shared, emitted: false })
    }
    fn process(&mut self, _process: Process, mut audio: Audio, events: Events)
        -> Result<ProcessStatus, PluginError> {
        if audio.frames_count() < 5 {
            return Err(PluginError::Message("owned producer requires five frames"));
        }
        let first = !self.emitted;
        if first {
            // Native acceptance is a real callback result, not a host fixture
            // echo. False retains a bounded fixture failure, never fake success.
            if events.output.try_push(ParamGestureBeginEvent::new(2, PARAMETER)).is_err()
                || events.output.try_push(ParamValueEvent::new(3, PARAMETER, Pckn::match_all(), OBSERVED)).is_err()
                || events.output.try_push(ParamGestureEndEvent::new(4, PARAMETER)).is_err() {
                return Err(PluginError::Message("native parameter output refused"));
            }
            self.emitted = true;
        }
        let mut pair = audio.port_pair(0).ok_or(PluginError::Message("missing port pair"))?;
        let mut channels = pair.channels()?.into_f32().ok_or(PluginError::Message("Float32 required"))?;
        for channel in channels.iter_mut() {
            match channel {
                ChannelPair::InputOutput(input, output) => output.copy_from_slice(input),
                ChannelPair::InPlace(_) => {},
                _ => return Err(PluginError::Message("stereo paired input/output required")),
            }
        }
        for batch in events.input.batch() {
            for event in batch.events() {
                if let Some(CoreEventSpace::ParamValue(value)) = event.as_core_event()
                    && value.param_id() == PARAMETER {
                    self.shared.value.store(value.value().to_bits(), Ordering::Release);
                }
            }
            let bounds = batch.sample_bounds();
            let first_sample = batch.first_sample();
            let incoming = self.shared.value() as f32;
            for channel in channels.iter_mut() {
                let output = match channel {
                    ChannelPair::InputOutput(_, output) => output,
                    ChannelPair::InPlace(output) => output,
                    _ => return Err(PluginError::Message("paired channel changed")),
                };
                for (offset, sample) in output[bounds].iter_mut().enumerate() {
                    let frame = first_sample + offset;
                    *sample *= if first && frame >= 3 { OBSERVED as f32 } else { incoming };
                }
            }
        }
        if first { self.shared.value.store(OBSERVED.to_bits(), Ordering::Release); }
        Ok(ProcessStatus::Continue)
    }
}

clack_export_entry!(SinglePluginEntry<ProcessAuthor>);
