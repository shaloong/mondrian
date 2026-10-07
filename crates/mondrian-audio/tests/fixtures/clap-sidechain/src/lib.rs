#![deny(unsafe_code)]
#![doc = include_str!("../README.md")]

use clack_extensions::audio_ports::*;
use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_extensions::tail::{PluginTail, PluginTailImpl, TailLength};
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

/// Independent native sidechain signal oracle.
pub struct SidechainFixture;

impl Plugin for SidechainFixture {
    type AudioProcessor<'a> = Processor<'a>;
    type Shared<'a> = Shared;
    type MainThread<'a> = Main<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginState>()
            .register::<PluginLatency>()
            .register::<PluginTail>();
    }
}

impl DefaultPluginFactory for SidechainFixture {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("org.mondrian.fixture.sidechain", "Sidechain Signal Fixture")
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<Shared, PluginError> {
        Ok(Shared {
            active: AtomicBool::new(false),
            mode: AtomicU8::new(0),
            gain: AtomicU32::new(1.0_f32.to_bits()),
        })
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        shared: &'a Shared,
    ) -> Result<Main<'a>, PluginError> {
        Ok(Main(shared))
    }
}

/// State restored before native port inspection and activation.
pub struct Shared {
    active: AtomicBool,
    mode: AtomicU8,
    gain: AtomicU32,
}
impl PluginShared<'_> for Shared {}

/// Main-thread port metadata and state adapter.
pub struct Main<'a>(&'a Shared);
impl<'a> PluginMainThread<'a, Shared> for Main<'a> {}

impl PluginStateImpl for Main<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        output.write_all(&[self.0.mode.load(Ordering::Relaxed)])?;
        output.write_all(&self.0.gain.load(Ordering::Relaxed).to_le_bytes())?;
        Ok(())
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut bytes = [0; 5];
        input.read_exact(&mut bytes)?;
        let gain = f32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
        if bytes[0] > 2 || !gain.is_finite() || !(0.0..=1.0).contains(&gain) {
            return Err(PluginError::Message("invalid fixture state"));
        }
        self.0.mode.store(bytes[0], Ordering::Relaxed);
        self.0.gain.store(gain.to_bits(), Ordering::Relaxed);
        Ok(())
    }
}

impl PluginLatencyImpl for Main<'_> {
    fn get(&self) -> u32 {
        if self.0.active.load(Ordering::Relaxed) {
            3
        } else {
            u32::MAX
        }
    }
}

impl PluginAudioPortsImpl for Main<'_> {
    fn count(&self, is_input: bool) -> u32 {
        if is_input {
            if self.0.mode.load(Ordering::Relaxed) == 2 {
                2
            } else {
                3
            }
        } else {
            1
        }
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index >= self.count(is_input) {
            return;
        }
        let reversed = self.0.mode.load(Ordering::Relaxed) == 1;
        let id = match index {
            0 => 0,
            1 if !reversed => 11,
            2 if reversed => 11,
            _ => 7,
        };
        writer.set(&AudioPortInfo {
            id: ClapId::new(id),
            name: b"input",
            channel_count: 2,
            flags: if index == 0 {
                AudioPortFlags::IS_MAIN
            } else {
                AudioPortFlags::empty()
            },
            port_type: Some(AudioPortType::STEREO),
            in_place_pair: None,
        });
    }
}

/// Prepared native PCM buffers and three-frame delay.
pub struct Processor<'a> {
    active: &'a AtomicBool,
    mix: Vec<[f32; 2]>,
    delay: [[f32; 2]; 3],
    cursor: usize,
    mode: u8,
    gain: f32,
}

impl PluginTailImpl for Processor<'_> {
    fn get(&self) -> TailLength {
        TailLength::Finite(3)
    }
}

impl<'a> PluginAudioProcessor<'a, Shared, Main<'a>> for Processor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main: &Main<'a>,
        shared: &'a Shared,
        config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        if config.max_frames_count == 0 || config.max_frames_count > 65_536 {
            return Err(PluginError::Message("fixture block extent"));
        }
        shared.active.store(true, Ordering::Relaxed);
        Ok(Self {
            active: &shared.active,
            mix: vec![[0.0; 2]; config.max_frames_count as usize],
            delay: [[0.0; 2]; 3],
            cursor: 0,
            mode: shared.mode.load(Ordering::Relaxed),
            gain: f32::from_bits(shared.gain.load(Ordering::Relaxed)),
        })
    }

    fn deactivate(self, _main: &Main<'a>) {
        self.active.store(false, Ordering::Relaxed);
    }

    fn reset(&mut self) {
        self.delay.fill([0.0; 2]);
        self.cursor = 0;
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let frames = audio.frames_count() as usize;
        let mix = self
            .mix
            .get_mut(..frames)
            .ok_or(PluginError::Message("block exceeds prepared capacity"))?;
        mix.fill([0.0; 2]);
        for (port_index, port) in audio.input_ports().enumerate() {
            let weight = match (self.mode, port_index) {
                (_, 0) => 2.0,
                (1, 1) | (0, 2) => 5.0,
                _ => 3.0,
            };
            let channels =
                port.channels()?.into_f32().ok_or(PluginError::Message("f32 input required"))?;
            for (channel, samples) in channels.iter().enumerate() {
                for (target, source) in mix.iter_mut().zip(samples.iter()) {
                    target[channel] += source * weight;
                }
            }
        }
        for frame in mix.iter_mut() {
            let delayed = self.delay[self.cursor];
            self.delay[self.cursor] = [frame[0] * self.gain, frame[1] * self.gain];
            *frame = delayed;
            self.cursor = (self.cursor + 1) % self.delay.len();
        }
        let mut output =
            audio.output_ports().next().ok_or(PluginError::Message("missing output"))?;
        let mut channels = output
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("f32 output required"))?;
        for (channel, samples) in channels.iter_mut().enumerate() {
            for (target, frame) in samples.iter_mut().zip(mix.iter()) {
                *target = frame[channel];
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

clack_export_entry!(SinglePluginEntry<SidechainFixture>);
