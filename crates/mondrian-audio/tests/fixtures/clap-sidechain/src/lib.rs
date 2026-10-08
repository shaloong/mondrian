#![deny(unsafe_code)]
#![doc = include_str!("../README.md")]

use clack_extensions::audio_ports::*;
use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
use clack_extensions::render::{PluginRender, PluginRenderImpl, RenderMode};
use clack_extensions::thread_check::HostThreadCheck;
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_extensions::tail::{PluginTail, PluginTailImpl, TailLength};
use clack_plugin::prelude::*;
use clack_plugin::events::event_types::TransportFlags;
use clack_plugin::stream::{InputStream, OutputStream};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

/// Independent native sidechain signal oracle.
pub struct SidechainFixture;

impl Plugin for SidechainFixture {
    type AudioProcessor<'a> = Processor<'a>;
    type Shared<'a> = Shared<'a>;
    type MainThread<'a> = Main<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginState>()
            .register::<PluginLatency>()
            .register::<PluginTail>()
            .register::<PluginRender>();
    }
}

impl DefaultPluginFactory for SidechainFixture {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("org.mondrian.fixture.sidechain", "Sidechain Signal Fixture")
    }

    fn new_shared(host: HostSharedHandle<'_>) -> Result<Shared<'_>, PluginError> {
        let shared = Shared {
            host,
            role_valid: AtomicBool::new(true),
            render_mode: AtomicU8::new(0),
            active: AtomicBool::new(false),
            mode: AtomicU8::new(0),
            gain: AtomicU32::new(1.0_f32.to_bits()),
        };
        shared.check_role(false)?;
        if let Some(check) = host.get_extension::<HostThreadCheck>() {
            let background_ok = std::thread::scope(|scope| {
                scope.spawn(move || check.is_main_thread(&host) == Some(false)
                    && check.is_audio_thread(&host) == Some(false))
                    .join().map_err(|_| PluginError::Message("background thread check failed"))
            })?;
            if !background_ok { return Err(PluginError::Message("background thread was misidentified")); }
        }
        host.request_callback();
        Ok(shared)
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        shared: &'a Shared<'a>,
    ) -> Result<Main<'a>, PluginError> {
        Ok(Main(shared))
    }
}

/// State restored before native port inspection and activation.
pub struct Shared<'a> {
    host: HostSharedHandle<'a>,
    role_valid: AtomicBool,
    render_mode: AtomicU8,
    active: AtomicBool,
    mode: AtomicU8,
    gain: AtomicU32,
}
impl<'a> PluginShared<'a> for Shared<'a> {}

impl Shared<'_> {
    fn check_role(&self, audio: bool) -> Result<(), PluginError> {
        let valid = self.host.get_extension::<HostThreadCheck>().is_some_and(|check| {
            check.is_main_thread(&self.host) == Some(true)
                && check.is_audio_thread(&self.host) == Some(audio)
        });
        if !valid { self.role_valid.store(false, Ordering::Relaxed); }
        if self.role_valid.load(Ordering::Relaxed) { Ok(()) }
        else { Err(PluginError::Message("invalid host thread role")) }
    }

    fn delay_frames(&self) -> usize {
        if self.mode.load(Ordering::Relaxed) == 3 && self.render_mode.load(Ordering::Relaxed) == 1 { 5 } else { 3 }
    }
}

impl Drop for Shared<'_> {
    fn drop(&mut self) { let _ = self.check_role(false); }
}

/// Main-thread port metadata and state adapter.
pub struct Main<'a>(&'a Shared<'a>);
impl<'a> PluginMainThread<'a, Shared<'a>> for Main<'a> {
    fn on_main_thread(&self) { let _ = self.0.check_role(false); }
}

impl PluginStateImpl for Main<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        self.0.check_role(false)?;
        output.write_all(&[self.0.mode.load(Ordering::Relaxed)])?;
        output.write_all(&self.0.gain.load(Ordering::Relaxed).to_le_bytes())?;
        Ok(())
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        self.0.check_role(false)?;
        let mut bytes = [0; 5];
        input.read_exact(&mut bytes)?;
        let gain = f32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
        if bytes[0] > 6 || !gain.is_finite() || !(0.0..=1.0).contains(&gain) {
            return Err(PluginError::Message("invalid fixture state"));
        }
        self.0.mode.store(bytes[0], Ordering::Relaxed);
        self.0.gain.store(gain.to_bits(), Ordering::Relaxed);
        Ok(())
    }
}

impl PluginRenderImpl for Main<'_> {
    fn has_hard_realtime_requirement(&self) -> bool {
        let _ = self.0.check_role(false);
        self.0.mode.load(Ordering::Relaxed) == 4
    }

    fn set(&self, mode: RenderMode) -> Result<(), PluginError> {
        self.0.check_role(false)?;
        if self.0.mode.load(Ordering::Relaxed) == 5 && mode == RenderMode::Offline {
            return Err(PluginError::Message("fixture declined offline render"));
        }
        self.0.render_mode.store((mode == RenderMode::Offline) as u8, Ordering::Relaxed);
        Ok(())
    }
}

impl PluginLatencyImpl for Main<'_> {
    fn get(&self) -> u32 {
        if self.0.check_role(false).is_ok() && self.0.active.load(Ordering::Relaxed) {
            self.0.delay_frames() as u32
        } else {
            u32::MAX
        }
    }
}

impl PluginAudioPortsImpl for Main<'_> {
    fn count(&self, is_input: bool) -> u32 {
        let _ = self.0.check_role(false);
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
            name: match (index, self.0.mode.load(Ordering::Relaxed), id) {
                (0, _, _) => "Main",
                (_, 2, _) => "   ",
                (_, 1, _) | (_, _, 11) => "压缩探测 🙂",
                _ => "备用探测",
            }
            .as_bytes(),
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

/// Prepared PCM, mode-specific delay and independent transport qualification.
pub struct Processor<'a> {
    shared: &'a Shared<'a>,
    mix: Vec<[f32; 2]>,
    delay: Vec<[f32; 2]>,
    cursor: usize,
    mode: u8,
    gain: f32,
    sample_rate: u32,
}

impl PluginTailImpl for Processor<'_> {
    fn get(&self) -> TailLength {
        if self.shared.check_role(true).is_ok() { TailLength::Finite(self.delay.len() as u32) }
        else { TailLength::Infinite }
    }
}

impl<'a> PluginAudioProcessor<'a, Shared<'a>, Main<'a>> for Processor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main: &Main<'a>,
        shared: &'a Shared<'a>,
        config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        shared.check_role(false)?;
        if config.max_frames_count == 0 || config.max_frames_count > 65_536 {
            return Err(PluginError::Message("fixture block extent"));
        }
        shared.active.store(true, Ordering::Relaxed);
        Ok(Self {
            shared,
            sample_rate: config.sample_rate as u32,
            mix: vec![[0.0; 2]; config.max_frames_count as usize],
            delay: vec![[0.0; 2]; shared.delay_frames()],
            cursor: 0,
            mode: shared.mode.load(Ordering::Relaxed),
            gain: f32::from_bits(shared.gain.load(Ordering::Relaxed))
                * if shared.mode.load(Ordering::Relaxed) == 3 && shared.render_mode.load(Ordering::Relaxed) == 1 { 1.5 } else { 1.0 },
        })
    }

    fn deactivate(self, _main: &Main<'a>) {
        let _ = self.shared.check_role(false);
        self.shared.active.store(false, Ordering::Relaxed);
    }

    fn reset(&mut self) {
        let _ = self.shared.check_role(true);
        self.delay.fill([0.0; 2]);
        self.cursor = 0;
    }

    fn start_processing(&mut self) -> Result<(), PluginError> {
        self.shared.check_role(true)
    }

    fn stop_processing(&mut self) {
        let _ = self.shared.check_role(true);
    }

    fn process(
        &mut self,
        process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        self.shared.check_role(true)?;
        self.shared.host.request_callback();
        let frames = audio.frames_count() as usize;
        // New state 6 is an independent ABI oracle. Other states retain their
        // original PCM, ports and mode-specific lifecycle requirements.
        let timeline_start = if self.mode == 6 {
            let transport = process.transport.ok_or(PluginError::Message("missing timeline transport"))?;
            if transport.flags != TransportFlags::HAS_SECONDS_TIMELINE
                || transport.header.time() != 0 || process.steady_time.is_some()
                || transport.song_pos_beats.to_bits() != 0
                || transport.tempo != 0.0 || transport.tempo_inc != 0.0
                || transport.time_signature_numerator != 0 || transport.time_signature_denominator != 0
                || transport.loop_start_beats.to_bits() != 0 || transport.loop_end_beats.to_bits() != 0
                || transport.loop_start_seconds.to_bits() != 0 || transport.loop_end_seconds.to_bits() != 0
                || transport.bar_start.to_bits() != 0 || transport.bar_number != 0
            { return Err(PluginError::Message("unavailable transport facts advertised")); }
            // Decode the ABI fixed-point seconds independently. 44.1/48 kHz
            // samples are recoverable exactly to the nearest integer sample.
            let scaled = i128::from(transport.song_pos_seconds.to_bits()) * i128::from(self.sample_rate);
            let factor = 1_i128 << 31;
            let samples = scaled / factor + if (scaled % factor).abs() * 2 >= factor { scaled.signum() } else { 0 };
            Some(i64::try_from(samples).map_err(|_| PluginError::Message("transport sample overflow"))?)
        } else { None };
        let mix = self
            .mix
            .get_mut(..frames)
            .ok_or(PluginError::Message("block exceeds prepared capacity"))?;
        mix.fill([0.0; 2]);
        for (port_index, port) in audio.input_ports().enumerate() {
            let weight = match (self.mode, port_index) {
                (_, 0) => 2.0,
                (1, 1) => 5.0,
                (_, 2) if self.mode != 1 => 5.0,
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
        for (index, frame) in mix.iter_mut().enumerate() {
            let tag = if let Some(start) = timeline_start {
                let sample = start.checked_add(index as i64).ok_or(PluginError::Message("transport sample overflow"))?;
                (sample % 251) as f32 / 1024.0
            } else { 0.0 };
            let gain = if timeline_start.is_some() { self.gain + tag } else { self.gain };
            let delayed = self.delay[self.cursor];
            self.delay[self.cursor] = [frame[0] * gain, frame[1] * gain];
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
