#![deny(unsafe_code)]
#![doc = include_str!("../README.md")]

use clack_extensions::audio_ports::*;
use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
use clack_extensions::log::{HostLog, LogSeverity};
use clack_extensions::render::{PluginRender, PluginRenderImpl, RenderMode};
use clack_extensions::thread_check::HostThreadCheck;
use clack_extensions::timer::{HostTimer, PluginTimer, PluginTimerImpl, TimerId};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_extensions::tail::{PluginTail, PluginTailImpl, TailLength};
use clack_plugin::prelude::*;
use clack_plugin::events::event_types::TransportFlags;
use clack_plugin::stream::{InputStream, OutputStream};
use std::cell::Cell;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

/// CLAP's audio role may coincide with main or run on another physical thread.
#[derive(Clone, Copy, Debug)]
enum RoleRequirement {
    Main,
    Audio,
    MainOrAudio,
}

impl RoleRequirement {
    fn accepts(self, main: Option<bool>, audio: Option<bool>, on_initial_main: bool) -> bool {
        // Verify the host's main identity independently of its symbolic audio
        // role; a host must not label every audio callback as main.
        if main != Some(on_initial_main) { return false; }
        match self {
            Self::Main => on_initial_main,
            Self::Audio => audio == Some(true),
            Self::MainOrAudio => on_initial_main || audio == Some(true),
        }
    }
}

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
            .register::<PluginRender>()
            .register::<PluginTimer>();
    }
}

impl DefaultPluginFactory for SidechainFixture {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("org.mondrian.fixture.sidechain", "Sidechain Signal Fixture")
    }

    fn new_shared(host: HostSharedHandle<'_>) -> Result<Shared<'_>, PluginError> {
        let shared = Shared {
            host,
            initial_main_thread: std::thread::current().id(),
            role_valid: AtomicBool::new(true),
            render_mode: AtomicU8::new(0),
            active: AtomicBool::new(false),
            restart_seen: AtomicBool::new(false),
            repeat_restart_count: AtomicU8::new(0),
            configured_latency: AtomicU32::new(3),
            mode: AtomicU8::new(0),
            gain: AtomicU32::new(1.0_f32.to_bits()),
        };
        shared.check_role(RoleRequirement::Main)?;
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
        host: HostMainThreadHandle<'a>,
        shared: &'a Shared<'a>,
    ) -> Result<Main<'a>, PluginError> {
        Ok(Main(shared, host, Cell::new(TimerFixtureState::Dormant), Cell::new(None)))
    }
}

/// State restored before native port inspection and activation.
pub struct Shared<'a> {
    host: HostSharedHandle<'a>,
    initial_main_thread: std::thread::ThreadId,
    role_valid: AtomicBool,
    render_mode: AtomicU8,
    active: AtomicBool,
    restart_seen: AtomicBool,
    repeat_restart_count: AtomicU8,
    configured_latency: AtomicU32,
    mode: AtomicU8,
    gain: AtomicU32,
}
impl<'a> PluginShared<'a> for Shared<'a> {}

impl Shared<'_> {
    fn check_role(&self, requirement: RoleRequirement) -> Result<(), PluginError> {
        let valid = self.host.get_extension::<HostThreadCheck>().is_some_and(|check| {
            requirement.accepts(
                check.is_main_thread(&self.host),
                check.is_audio_thread(&self.host),
                std::thread::current().id() == self.initial_main_thread,
            )
        });
        if !valid { self.role_valid.store(false, Ordering::Relaxed); }
        if self.role_valid.load(Ordering::Relaxed) { Ok(()) }
        else { Err(PluginError::Message("invalid host thread role")) }
    }

    fn emit_logs(&self, message: &'static std::ffi::CStr) -> Result<(), PluginError> {
        let logger=self.host.get_extension::<HostLog>()
            .ok_or(PluginError::Message("standard host log extension unavailable"))?;
        for severity in [LogSeverity::Debug,LogSeverity::Info,LogSeverity::Warning,
            LogSeverity::Error,LogSeverity::Fatal,LogSeverity::HostMisbehaving,LogSeverity::PluginMisbehaving] {
            logger.log(&self.host,severity,message);
        }
        Ok(())
    }

    fn delay_frames(&self) -> usize {
        self.configured_latency.load(Ordering::Relaxed) as usize
    }

    fn next_activation_delay_frames(&self) -> u32 {
        match self.mode.load(Ordering::Relaxed) {
            12 => 0,
            13 => 3 + 4 * u32::from(self.repeat_restart_count.load(Ordering::Relaxed)),
            3 if self.render_mode.load(Ordering::Relaxed) == 1 => 5,
            7..=9 if self.restart_seen.load(Ordering::Relaxed) => 7,
            _ => 3,
        }
    }
}

impl Drop for Shared<'_> {
    fn drop(&mut self) { let _ = self.check_role(RoleRequirement::Main); }
}

#[derive(Clone, Copy)]
enum TimerFixtureState {
    Dormant,
    Registered { a: TimerId, b: TimerId },
    Finished,
}

/// Main-thread port metadata, state and owned timer adapter.
pub struct Main<'a>(
    &'a Shared<'a>,
    HostMainThreadHandle<'a>,
    Cell<TimerFixtureState>,
    Cell<Option<BeatReference>>,
);

/// Mode11's independent expected reference, configured by the test state rather
/// than copied from the transport being checked. No other mode consumes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BeatReference {
    zero_numerator: i64,
    zero_denominator: i64,
    duration_numerator: i64,
    duration_denominator: i64,
}

impl BeatReference {
    const DEFAULT: Self = Self {
        zero_numerator: -19,
        zero_denominator: 7,
        duration_numerator: 3,
        duration_denominator: 4,
    };

    fn fields(self) -> [i64; 4] {
        [
            self.zero_numerator,
            self.zero_denominator,
            self.duration_numerator,
            self.duration_denominator,
        ]
    }

    fn read_optional(input: &mut impl Read) -> Result<Option<Self>, PluginError> {
        let mut bytes = [0_u8; 32];
        if input.read(&mut bytes[..1])? == 0 {
            return Ok(None);
        }
        input.read_exact(&mut bytes[1..])?;
        let mut extra = [0_u8; 1];
        if input.read(&mut extra)? != 0 {
            return Err(PluginError::Message("extra mode11 reference bytes"));
        }
        let mut fields = [0_i64; 4];
        for (field, bytes) in fields.iter_mut().zip(bytes.chunks_exact(8)) {
            *field = i64::from_le_bytes(bytes.try_into().expect("fixed eight-byte field"));
        }
        let [zero_numerator, zero_denominator, duration_numerator, duration_denominator] = fields;
        if zero_denominator <= 0 || duration_numerator <= 0 || duration_denominator <= 0 {
            return Err(PluginError::Message("invalid mode11 reference"));
        }
        Ok(Some(Self {
            zero_numerator,
            zero_denominator,
            duration_numerator,
            duration_denominator,
        }))
    }

    fn tempo(self) -> f64 {
        60.0 * self.duration_denominator as f64 / self.duration_numerator as f64
    }

    fn ticks(self, sample: i64, rate: u32) -> Result<i64, PluginError> {
        // This independent fixture oracle admits only the finite test vectors
        // whose complete ratio fits i128. It never calls the host's lowering.
        let overflow = || PluginError::Message("mode11 oracle ratio overflow");
        let difference = i128::from(sample)
            .checked_mul(i128::from(self.zero_denominator))
            .and_then(|value| value.checked_sub(i128::from(self.zero_numerator) * i128::from(rate)))
            .ok_or_else(overflow)?;
        let numerator = difference
            .checked_mul(i128::from(self.duration_denominator))
            .and_then(|value| value.checked_mul(1_i128 << 31))
            .ok_or_else(overflow)?;
        let denominator = i128::from(rate)
            .checked_mul(i128::from(self.zero_denominator))
            .and_then(|value| value.checked_mul(i128::from(self.duration_numerator)))
            .filter(|value| *value > 0)
            .ok_or_else(overflow)? as u128;
        let magnitude = numerator.unsigned_abs();
        let quotient = magnitude / denominator;
        let remainder = magnitude % denominator;
        let rounded = quotient
            .checked_add(u128::from(remainder >= denominator - remainder))
            .ok_or_else(overflow)?;
        let signed = i128::try_from(rounded).map_err(|_| overflow())?;
        let signed = if numerator < 0 { -signed } else { signed };
        i64::try_from(signed).map_err(|_| overflow())
    }
}

impl Main<'_> {
    fn timer_log(&self, message: &'static std::ffi::CStr) -> Result<(), PluginError> {
        let logger = self
            .0
            .host
            .get_extension::<HostLog>()
            .ok_or(PluginError::Message("timer fixture requires host log"))?;
        logger.log(&self.0.host, LogSeverity::Info, message);
        Ok(())
    }

    fn timer_main_role(&self) -> Result<(), PluginError> {
        self.0.check_role(RoleRequirement::Main)?;
        let check = self
            .0
            .host
            .get_extension::<HostThreadCheck>()
            .ok_or(PluginError::Message("timer fixture requires thread check"))?;
        if check.is_main_thread(&self.0.host) != Some(true)
            || check.is_audio_thread(&self.0.host) != Some(false)
        {
            return Err(PluginError::Message(
                "timer callback must be main, not audio",
            ));
        }
        Ok(())
    }

    fn register_fixture_timers(&self) -> Result<(), PluginError> {
        self.timer_main_role()?;
        // Never register again after a failure or after A consumes this pair.
        self.2.set(TimerFixtureState::Finished);
        let timer = self
            .1
            .get_extension::<HostTimer>()
            .ok_or(PluginError::Message("standard host timer unavailable"))?;
        let a = timer.register_timer(&self.1, 1)?;
        self.timer_log(c"timer:registered:A")?;
        let b = timer.register_timer(&self.1, 1)?;
        if a == b {
            return Err(PluginError::Message("live timer IDs must be distinct"));
        }
        let registered_b = std::time::Instant::now();
        self.2.set(TimerFixtureState::Registered { a, b });
        self.timer_log(c"timer:registered:B-distinct")?;
        // This owned mode creates a due pair, not a cadence benchmark. The
        // current host assigns the first two empty slots to A then B. Its
        // service_main_tick samples due time AFTER this main callback returns,
        // so both registered one-millisecond deadlines must be in that snapshot.
        // No DSP callback sleeps, and no independent idle scheduling is claimed.
        std::thread::sleep(std::time::Duration::from_millis(2));
        if registered_b.elapsed() < std::time::Duration::from_millis(1) {
            return Err(PluginError::Message("timer due-pair precondition failed"));
        }
        self.timer_log(c"timer:both-due-before-main-return")
    }

    fn consume_fixture_timer(&self, id: TimerId) -> Result<(), PluginError> {
        self.timer_main_role()?;
        let TimerFixtureState::Registered { a, b } = self.2.get() else {
            return Err(PluginError::Message("unexpected repeated timer callback"));
        };
        if id != a {
            return Err(PluginError::Message(
                "B must be skipped after A unregisters it",
            ));
        }
        self.2.set(TimerFixtureState::Finished);
        self.timer_log(c"timer:A:main=true:audio=false")?;
        let timer = self
            .1
            .get_extension::<HostTimer>()
            .ok_or(PluginError::Message("standard host timer disappeared"))?;
        timer.unregister_timer(&self.1, b)?;
        self.timer_log(c"timer:unregistered:B")?;
        timer.unregister_timer(&self.1, a)?;
        self.timer_log(c"timer:unregistered:A")?;
        if timer.unregister_timer(&self.1, b).is_ok() {
            return Err(PluginError::Message("duplicate B unregister must fail"));
        }
        self.timer_log(c"timer:duplicate-unregister:B:refused")?;
        if timer.unregister_timer(&self.1, a).is_ok() {
            return Err(PluginError::Message("duplicate A unregister must fail"));
        }
        self.timer_log(c"timer:duplicate-unregister:A:refused")
    }
}

impl<'a> PluginMainThread<'a, Shared<'a>> for Main<'a> {
    fn on_main_thread(&self) {
        let _ = self.0.check_role(RoleRequirement::Main);
        if self.0.mode.load(Ordering::Relaxed) == 14
            && matches!(self.2.get(), TimerFixtureState::Dormant)
            && self.register_fixture_timers().is_err()
        {
            let _ = self.timer_log(c"timer:registration-failed");
        }
    }
}

impl PluginTimerImpl for Main<'_> {
    fn on_timer(&self, id: TimerId) {
        if self.consume_fixture_timer(id).is_err() {
            let _ = self.timer_log(c"timer:unexpected-callback-or-unregister-failure");
        }
    }
}

impl PluginStateImpl for Main<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        self.0.check_role(RoleRequirement::Main)?;
        output.write_all(&[self.0.mode.load(Ordering::Relaxed)])?;
        output.write_all(&self.0.gain.load(Ordering::Relaxed).to_le_bytes())?;
        if (7..=9).contains(&self.0.mode.load(Ordering::Relaxed)) {
            output.write_all(&[u8::from(self.0.restart_seen.load(Ordering::Relaxed))])?;
        }
        if self.0.mode.load(Ordering::Relaxed) == 13 {
            output.write_all(&[self.0.repeat_restart_count.load(Ordering::Relaxed)])?;
        }
        if self.0.mode.load(Ordering::Relaxed) == 11 {
            if let Some(reference) = self.3.get() {
                for field in reference.fields() {
                    output.write_all(&field.to_le_bytes())?;
                }
            }
        }
        if self.0.restart_seen.load(Ordering::Relaxed)
            && self.0.configured_latency.load(Ordering::Relaxed) == 7 {
            match self.0.mode.load(Ordering::Relaxed) {
                // A second same-bit callback arrives inside actual state capture.
                8 => self.0.host.request_restart(),
                // Deliberately ignore the failed native write. The host's own
                // writer must prevent publication of truncated configuration.
                9 => { let oversized = [0_u8; 70 * 1024]; let _ = output.write(&oversized); }
                _ => {}
            }
        }
        Ok(())
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        self.0.check_role(RoleRequirement::Main)?;
        let mut bytes = [0; 5];
        input.read_exact(&mut bytes)?;
        let gain = configuration_gain(bytes)?;
        let restart_seen = if (7..=9).contains(&bytes[0]) {
            let mut flag = [0];
            input.read_exact(&mut flag)?;
            if flag[0] > 1 { return Err(PluginError::Message("invalid fixture restart flag")); }
            flag[0] != 0
        } else { false };
        let repeat_restart_count = if bytes[0] == 13 {
            read_repeat_restart_count(input)?
        } else { 0 };
        let reference = if bytes[0] == 11 { BeatReference::read_optional(input)? } else { None };
        self.3.set(reference);
        self.0.repeat_restart_count.store(repeat_restart_count, Ordering::Relaxed);
        self.0.restart_seen.store(restart_seen, Ordering::Relaxed);
        self.0.mode.store(bytes[0], Ordering::Relaxed);
        self.0.gain.store(gain.to_bits(), Ordering::Relaxed);
        Ok(())
    }
}

impl PluginRenderImpl for Main<'_> {
    fn has_hard_realtime_requirement(&self) -> bool {
        let _ = self.0.check_role(RoleRequirement::Main);
        self.0.mode.load(Ordering::Relaxed) == 4
    }

    fn set(&self, mode: RenderMode) -> Result<(), PluginError> {
        self.0.check_role(RoleRequirement::Main)?;
        if self.0.mode.load(Ordering::Relaxed) == 5 && mode == RenderMode::Offline {
            return Err(PluginError::Message("fixture declined offline render"));
        }
        self.0.render_mode.store((mode == RenderMode::Offline) as u8, Ordering::Relaxed);
        Ok(())
    }
}

impl PluginLatencyImpl for Main<'_> {
    fn get(&self) -> u32 {
        if self.0.check_role(RoleRequirement::Main).is_ok() && self.0.active.load(Ordering::Relaxed) {
            self.0.delay_frames() as u32
        } else {
            u32::MAX
        }
    }
}

impl PluginAudioPortsImpl for Main<'_> {
    fn count(&self, is_input: bool) -> u32 {
        let _ = self.0.check_role(RoleRequirement::Main);
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

const ECHO_FRAMES: usize = 256;

// Only the existing five-byte mode/gain header is configuration. Modes 7–9
// retain their separate restart flag; mode12 never serializes its live ring.
// Isolated mode13 appends only a bounded configuration generation 0..=2.
fn configuration_gain(bytes: [u8; 5]) -> Result<f32, PluginError> {
    let gain = f32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
    if bytes[0] > 14 || !gain.is_finite() || !(0.0..=1.0).contains(&gain) {
        return Err(PluginError::Message("invalid fixture state"));
    }
    Ok(gain)
}

fn read_repeat_restart_count(input: &mut impl Read) -> Result<u8, PluginError> {
    let mut count = [0];
    input.read_exact(&mut count)?;
    if count[0] > 2 {
        return Err(PluginError::Message(
            "invalid repeated fixture restart count",
        ));
    }
    Ok(count[0])
}

// This real process-boundary decision emits exactly two actual host callbacks;
// the host cannot obtain either request by inventing a parent-side ticket.
fn repeat_restart_boundary(count: &AtomicU8, end: i64) -> bool {
    let previous = count.load(Ordering::Relaxed);
    if !matches!((previous, end), (0, 10..) | (1, 30..)) {
        return false;
    }
    count
        .compare_exchange(previous, previous + 1, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
}

// The 2048-byte stereo ring belongs to this native DSP instance. Latency is
// zero: a tap is audible history, not a delay for the host to compensate.
struct FiniteEcho {
    ring: [[f32; 2]; ECHO_FRAMES],
    cursor: usize,
}

impl FiniteEcho {
    fn new() -> Self {
        Self { ring: [[0.0; 2]; ECHO_FRAMES], cursor: 0 }
    }

    fn process_frame(&mut self, current: [f32; 2]) -> [f32; 2] {
        let tap = self.ring[self.cursor];
        self.ring[self.cursor] = current;
        self.cursor = (self.cursor + 1) % ECHO_FRAMES;
        [current[0] + 0.5 * tap[0], current[1] + 0.5 * tap[1]]
    }

    fn reset(&mut self) {
        self.ring.fill([0.0; 2]);
        self.cursor = 0;
    }
}

/// Prepared PCM, mode-specific delay and independent transport qualification.
pub struct Processor<'a> {
    shared: &'a Shared<'a>,
    mix: Vec<[f32; 2]>,
    delay: Vec<[f32; 2]>,
    echo: Option<Box<FiniteEcho>>,
    cursor: usize,
    mode: u8,
    gain: f32,
    sample_rate: u32,
    beat_reference: BeatReference,
}

impl PluginTailImpl for Processor<'_> {
    fn get(&self) -> TailLength {
        if self.shared.check_role(RoleRequirement::MainOrAudio).is_ok() {
            TailLength::Finite(if self.mode == 12 { ECHO_FRAMES as u32 } else { self.delay.len() as u32 })
        }
        else { TailLength::Infinite }
    }
}

impl<'a> PluginAudioProcessor<'a, Shared<'a>, Main<'a>> for Processor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        main: &Main<'a>,
        shared: &'a Shared<'a>,
        config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        shared.check_role(RoleRequirement::Main)?;
        if shared.mode.load(Ordering::Relaxed) == 14 {
            // Registration runs through the actual plugin main callback, after
            // the normal restored-state/render-mode/activation path.
            shared.host.request_callback();
        }
        if config.max_frames_count == 0 || config.max_frames_count > 65_536 {
            return Err(PluginError::Message("fixture block extent"));
        }
        if shared.mode.load(Ordering::Relaxed)==10 {
            shared.emit_logs(c"log:activate")?;
            // Legal arbitrary-thread callbacks, joined before activation ends.
            // No process callback creates, joins or waits for this thread.
            std::thread::scope(|scope| {
                // PluginError may contain a non-Send erased error. Carry only
                // its owned message across the thread, then restore the error
                // on the activation thread without discarding the failure.
                scope.spawn(|| shared.emit_logs(c"log:background").map_err(|error| error.to_string()))
                    .join().map_err(|_|PluginError::Message("background logger panicked"))?
                    .map_err(|message|PluginError::Error(Box::new(std::io::Error::other(message))))
            })?;
        }
        shared.configured_latency.store(shared.next_activation_delay_frames(), Ordering::Relaxed);
        shared.active.store(true, Ordering::Relaxed);
        Ok(Self {
            shared,
            sample_rate: config.sample_rate as u32,
            beat_reference: main.3.get().unwrap_or(BeatReference::DEFAULT),
            mix: vec![[0.0; 2]; config.max_frames_count as usize],
            delay: vec![[0.0; 2]; shared.delay_frames()],
            echo: (shared.mode.load(Ordering::Relaxed) == 12).then(|| Box::new(FiniteEcho::new())),
            cursor: 0,
            mode: shared.mode.load(Ordering::Relaxed),
            gain: f32::from_bits(shared.gain.load(Ordering::Relaxed))
                * if shared.mode.load(Ordering::Relaxed) == 3 && shared.render_mode.load(Ordering::Relaxed) == 1 { 1.5 } else { 1.0 },
        })
    }

    fn deactivate(self, _main: &Main<'a>) {
        let _ = self.shared.check_role(RoleRequirement::Main);
        self.shared.active.store(false, Ordering::Relaxed);
    }

    fn reset(&mut self) {
        let _ = self.shared.check_role(RoleRequirement::Audio);
        self.delay.fill([0.0; 2]);
        self.cursor = 0;
        if let Some(echo) = self.echo.as_mut() { echo.reset(); }
    }

    fn start_processing(&mut self) -> Result<(), PluginError> {
        self.shared.check_role(RoleRequirement::Audio)
    }

    fn stop_processing(&mut self) {
        let _ = self.shared.check_role(RoleRequirement::Audio);
    }

    fn process(
        &mut self,
        process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        self.shared.check_role(RoleRequirement::Audio)?;
        if self.mode==10 { self.shared.emit_logs(c"log:process")?; }
        self.shared.host.request_callback();
        let frames = audio.frames_count() as usize;
        // New state 6 is an independent ABI oracle. Other states retain their
        // original PCM, ports and mode-specific lifecycle requirements.
        let timeline_start = if (6..=9).contains(&self.mode) || self.mode == 13 {
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
        } else if self.mode == 11 {
            // Some carries exact beats and its explicit tempo; None remains
            // seconds-only. Bar/meter/loop/playing facts are never inferred.
            let transport =
                process.transport.ok_or(PluginError::Message("missing musical transport"))?;
            let scaled =
                i128::from(transport.song_pos_seconds.to_bits()) * i128::from(self.sample_rate);
            let factor = 1_i128 << 31;
            let samples = scaled / factor
                + if (scaled % factor).abs() * 2 >= factor {
                    scaled.signum()
                } else {
                    0
                };
            let sample = i64::try_from(samples)
                .map_err(|_| PluginError::Message("musical transport sample overflow"))?;
            let has_music = transport.flags.contains(TransportFlags::HAS_TEMPO);
            let expected_flags = TransportFlags::HAS_SECONDS_TIMELINE
                | if has_music {
                    TransportFlags::HAS_TEMPO | TransportFlags::HAS_BEATS_TIMELINE
                } else {
                    TransportFlags::empty()
                };
            let expected_beats = if has_music {
                self.beat_reference.ticks(sample, self.sample_rate)?
            } else {
                0
            };
            if transport.flags != expected_flags
                || transport.header.time() != 0
                || process.steady_time.is_some()
                || transport.song_pos_beats.to_bits() != expected_beats
                || transport.tempo
                    != if has_music {
                        self.beat_reference.tempo()
                    } else {
                        0.0
                    }
                || transport.tempo_inc != 0.0
                || transport.bar_start.to_bits() != 0
                || transport.bar_number != 0
                || transport.time_signature_numerator != 0
                || transport.time_signature_denominator != 0
                || transport.loop_start_beats.to_bits() != 0
                || transport.loop_end_beats.to_bits() != 0
                || transport.loop_start_seconds.to_bits() != 0
                || transport.loop_end_seconds.to_bits() != 0
            {
                return Err(PluginError::Message("invalid musical transport facts"));
            }
            Some(sample)
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
            let tag = if let Some(start) = timeline_start.filter(|_| self.mode == 6) {
                let sample = start.checked_add(index as i64).ok_or(PluginError::Message("transport sample overflow"))?;
                (sample % 251) as f32 / 1024.0
            } else { 0.0 };
            let gain = if self.mode == 11 {
                let start = timeline_start.ok_or(PluginError::Message("missing tempo timeline"))?;
                let sample = start.checked_add(index as i64).ok_or(PluginError::Message("tempo sample overflow"))?;
                let tempo = process.transport.ok_or(PluginError::Message("missing tempo"))?.tempo as f32;
                self.gain + (sample % 251) as f32 / 1024.0 + tempo / 1024.0
            } else if timeline_start.is_some() { self.gain + tag } else { self.gain };
            if let Some(echo) = self.echo.as_mut() {
                *frame = echo.process_frame([frame[0] * gain, frame[1] * gain]);
            } else {
                // Original modes retain their exact delay path. Mode12's
                // latency0 never indexes or takes modulo an empty delay Vec.
                let delayed = self.delay[self.cursor];
                self.delay[self.cursor] = [frame[0] * gain, frame[1] * gain];
                *frame = delayed;
                self.cursor = (self.cursor + 1) % self.delay.len();
            }
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
        if (7..=9).contains(&self.mode) && timeline_start.is_some_and(|start| start.saturating_add(frames as i64) > 9)
            && !self.shared.restart_seen.swap(true, Ordering::Relaxed)
        {
            // Core request_restart is thread-safe. This completed process block
            // remains valid under the current active delay until the host stops,
            // deactivates and activates at a later legal boundary.
            self.shared.host.request_restart();
        }
        if self.mode == 13 && timeline_start.is_some_and(|start| {
            repeat_restart_boundary(&self.shared.repeat_restart_count, start.saturating_add(frames as i64))
        }) {
            self.shared.host.request_restart();
        }
        Ok(ProcessStatus::Continue)
    }
}

clack_export_entry!(SinglePluginEntry<SidechainFixture>);

#[cfg(test)]
mod role_contract_tests {
    use super::RoleRequirement;

    #[test]
    fn role_requirements_verify_physical_main_and_symbolic_audio() {
        // Columns are Main, Audio, MainOrAudio. Both audio identities are
        // legal, but a mismatching main claim and neither role are rejected.
        let cases = [
            (false, Some(false), Some(false), [false, false, false]),
            (false, Some(false), Some(true),  [false, true, true]),
            (false, Some(false), None,        [false, false, false]),
            (false, Some(true),  Some(false), [false, false, false]),
            (false, Some(true),  Some(true),  [false, false, false]),
            (false, Some(true),  None,        [false, false, false]),
            (false, None,        Some(false), [false, false, false]),
            (false, None,        Some(true),  [false, false, false]),
            (false, None,        None,        [false, false, false]),
            (true,  Some(false), Some(false), [false, false, false]),
            (true,  Some(false), Some(true),  [false, false, false]),
            (true,  Some(false), None,        [false, false, false]),
            (true,  Some(true),  Some(false), [true, false, true]),
            (true,  Some(true),  Some(true),  [true, true, true]),
            (true,  Some(true),  None,        [true, false, true]),
            (true,  None,        Some(false), [false, false, false]),
            (true,  None,        Some(true),  [false, false, false]),
            (true,  None,        None,        [false, false, false]),
        ];
        let requirements = [RoleRequirement::Main, RoleRequirement::Audio, RoleRequirement::MainOrAudio];
        for (on_initial_main, main, audio, expected) in cases {
            for (requirement, expected) in requirements.into_iter().zip(expected) {
                assert_eq!(requirement.accepts(main, audio, on_initial_main), expected,
                    "{requirement:?}: main={main:?}, audio={audio:?}, actual_initial_main={on_initial_main}");
            }
        }
    }
}


#[cfg(test)]
mod finite_echo_tests {
    use super::{configuration_gain, FiniteEcho, ECHO_FRAMES};

    #[test]
    fn impulse_has_immediate_dry_signal_and_exact_finite_tap_across_blocks() {
        let mut echo = FiniteEcho::new();
        let mut frame = 0;
        // Unequal blocks cross both the tap and ring wrap boundaries. The
        // expected impulse response is independent of the ring implementation.
        for block in [127, 130, 19, 257] {
            for _ in 0..block {
                let input = if frame == 0 { [1.0, -0.5] } else { [0.0; 2] };
                let expected = match frame {
                    0 => [1.0, -0.5],
                    ECHO_FRAMES => [0.5, -0.25],
                    _ => [0.0; 2],
                };
                assert_eq!(echo.process_frame(input), expected, "public frame {frame}");
                frame += 1;
            }
        }
    }

    #[test]
    fn configuration_restore_is_cold_and_reset_discards_live_history() {
        let mut bytes = [12, 0, 0, 0, 0];
        bytes[1..].copy_from_slice(&0.5_f32.to_le_bytes());
        let gain = configuration_gain(bytes).expect("actual load header accepts mode12");
        assert_eq!(gain, 0.5);
        let mut live = FiniteEcho::new();
        for _ in 0..ECHO_FRAMES {
            assert_eq!(live.process_frame([2.0 * gain, -gain]), [1.0, -0.5]);
        }
        // Configuration contains only these same five bytes; restoring it
        // constructs a fresh ring, not an exact-history checkpoint.
        assert_eq!(configuration_gain(bytes).unwrap(), gain);
        let mut restored = FiniteEcho::new();
        assert_eq!(live.process_frame([0.0; 2]), [0.5, -0.25]);
        assert_eq!(restored.process_frame([0.0; 2]), [0.0; 2]);
        live.reset();
        for _ in 0..=ECHO_FRAMES {
            assert_eq!(live.process_frame([0.0; 2]), [0.0; 2]);
        }
        bytes[0] = 15;
        assert!(configuration_gain(bytes).is_err());
    }
}


#[cfg(test)]
mod repeat_restart_tests {
    use super::*;

    #[test]
    fn repeated_restart_configuration_is_bounded_and_requires_its_sixth_byte() {
        let mut header = [13, 0, 0, 0, 0];
        header[1..].copy_from_slice(&0.5_f32.to_le_bytes());
        assert_eq!(configuration_gain(header).unwrap(), 0.5);
        for expected in 0..=2 {
            assert_eq!(
                read_repeat_restart_count(&mut &[expected][..]).unwrap(),
                expected
            );
        }
        assert!(read_repeat_restart_count(&mut &[][..]).is_err());
        assert!(read_repeat_restart_count(&mut &[3][..]).is_err());
        header[0] = 15;
        assert!(configuration_gain(header).is_err());
    }

    #[test]
    fn real_process_boundary_decision_requests_only_two_distinct_generations() {
        let count = AtomicU8::new(0);
        assert!(!repeat_restart_boundary(&count, 9));
        assert!(repeat_restart_boundary(&count, 10));
        assert_eq!(count.load(Ordering::Relaxed), 1);
        for end in [10, 11, 29] {
            assert!(!repeat_restart_boundary(&count, end));
        }
        assert!(repeat_restart_boundary(&count, 30));
        assert_eq!(count.load(Ordering::Relaxed), 2);
        for end in [30, 31, i64::MAX] {
            assert!(!repeat_restart_boundary(&count, end));
        }
    }
}

#[cfg(test)]
mod beat_reference_tests {
    use super::*;

    fn bytes(reference: BeatReference) -> Vec<u8> {
        reference.fields().into_iter().flat_map(i64::to_le_bytes).collect()
    }

    #[test]
    fn mode11_reference_accepts_only_empty_or_exact_valid_payload() {
        assert_eq!(BeatReference::read_optional(&mut &[][..]).unwrap(), None);
        let valid = bytes(BeatReference::DEFAULT);
        assert_eq!(
            BeatReference::read_optional(&mut valid.as_slice()).unwrap(),
            Some(BeatReference::DEFAULT)
        );
        for length in 1..32 {
            assert!(BeatReference::read_optional(&mut &valid[..length]).is_err());
        }
        let mut extra = valid.clone();
        extra.push(0);
        assert!(BeatReference::read_optional(&mut extra.as_slice()).is_err());
        for index in 1..4 {
            for invalid in [0_i64, -1] {
                let mut invalid_bytes = valid.clone();
                invalid_bytes[index * 8..index * 8 + 8].copy_from_slice(&invalid.to_le_bytes());
                assert!(BeatReference::read_optional(&mut invalid_bytes.as_slice()).is_err());
            }
        }
    }

    #[test]
    fn mode11_reference_oracle_preserves_signed_large_origin_cancellation() {
        assert_eq!(BeatReference::DEFAULT.tempo(), 80.0);
        for rate in [44_100, 48_000] {
            for sample in [-(1_i64 << 47), 1_i64 << 47] {
                for offset in [-1, 1] {
                    let expected = BeatReference {
                        zero_numerator: sample - offset,
                        zero_denominator: i64::from(rate),
                        duration_numerator: 1,
                        duration_denominator: i64::from(rate),
                    };
                    assert_eq!(
                        expected.ticks(sample, rate).unwrap(),
                        offset * (1_i64 << 31)
                    );
                    assert!(expected.ticks(sample, 0).is_err());
                }
            }
        }
    }
}
