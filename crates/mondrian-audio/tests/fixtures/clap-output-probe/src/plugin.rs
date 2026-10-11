//! Owned independent callback-acceptance oracle, shared by the native DLL and SDK test entry.
//! Its PCM is the native producer's observed callback result, never a host-generated echo.

use clack_extensions::audio_ports::*;
use clack_extensions::note_ports::*;
use clack_plugin::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent};
use clack_plugin::prelude::*;

/// Declared stereo audio effect with one real CLAP/MIDI note output.
pub struct OutputProbePlugin;
/// Main-thread metadata owner; no shared controller/DSP state is synthesized.
pub struct OutputProbeMain;
/// Actual processor retains only a non-wrapping note ID for each native call.
pub struct OutputProbeProcessor {
    next_note_id: u32,
}

impl Plugin for OutputProbePlugin {
    type Shared<'a> = ();
    type MainThread<'a> = OutputProbeMain;
    type AudioProcessor<'a> = OutputProbeProcessor;
    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&()>) {
        builder.register::<PluginAudioPorts>().register::<PluginNotePorts>();
    }
}

impl DefaultPluginFactory for OutputProbePlugin {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "org.mondrian.fixture.output-probe",
            "Mondrian CLAP Output Probe",
        )
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<(), PluginError> {
        Ok(())
    }
    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        _shared: &'a (),
    ) -> Result<OutputProbeMain, PluginError> {
        Ok(OutputProbeMain)
    }
}

impl PluginMainThread<'_, ()> for OutputProbeMain {}
impl PluginAudioPortsImpl for OutputProbeMain {
    fn count(&self, _is_input: bool) -> u32 {
        1
    }
    fn get(&self, index: u32, _is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: b"main",
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            });
        }
    }
}
impl PluginNotePortsImpl for OutputProbeMain {
    fn count(&self, is_input: bool) -> u32 {
        u32::from(!is_input)
    }
    fn get(&self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if !is_input && index == 0 {
            writer.set(&NotePortInfo {
                id: ClapId::new(17),
                name: b"notes",
                supported_dialects: NoteDialects::CLAP | NoteDialects::MIDI,
                preferred_dialect: Some(NoteDialect::Clap),
            });
        }
    }
}

impl<'a> PluginAudioProcessor<'a, (), OutputProbeMain> for OutputProbeProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main: &OutputProbeMain,
        _shared: &'a (),
        _config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self { next_note_id: 0 })
    }
    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        if audio.frames_count() < 2 {
            return Err(PluginError::Message("output oracle requires two frames"));
        }
        if self.next_note_id > i32::MAX as u32 {
            return Err(PluginError::Message("owned note serial exhausted"));
        }
        let note = Pckn::new(0u16, 0u16, 60u16, self.next_note_id);
        self.next_note_id += 1;
        // Different keys denote different notes: never encode the same note twice.
        let note_on = events.output.try_push(NoteOnEvent::new(0, note, 0.5)).is_ok();
        let midi_on = events.output.try_push(MidiEvent::new(0, 0, [0x90, 61, 100])).is_ok();
        let note_off = events.output.try_push(NoteOffEvent::new(1, note, 0.5)).is_ok();
        let midi_off = events.output.try_push(MidiEvent::new(1, 0, [0x80, 61, 64])).is_ok();
        let acceptance = f32::from(
            u8::from(note_on)
                | u8::from(midi_on) << 1
                | u8::from(note_off) << 2
                | u8::from(midi_off) << 3,
        );
        let mut output =
            audio.output_ports().next().ok_or(PluginError::Message("missing output"))?;
        let mut channels = output
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("Float32 output required"))?;
        for channel in channels.iter_mut() {
            channel.fill(acceptance);
        }
        Ok(ProcessStatus::Continue)
    }
}
