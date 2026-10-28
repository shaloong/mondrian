//! Owned CLAP ABI controls, not third-party compatibility qualification.
use super::*;
use clack_extensions::audio_ports_config::{
    AudioPortConfigWriter, AudioPortsConfiguration, MainPortInfo, PluginAudioPortsConfig,
    PluginAudioPortsConfigImpl, PluginAudioPortsConfigInfo, PluginAudioPortsConfigInfoImpl,
};
use std::cell::Cell;

const ID: &str = "org.mondrian.test.audio-config";
const INITIAL_GAIN: u32 = 3;
const SELECTED_GAIN: u32 = 1;
struct ConfigPlugin<const CASE: u8, const INITIAL: u32>;
struct Shared {
    channels: AtomicU32,
    gain: AtomicU32,
    active: AtomicBool,
}
struct Main<'a, const CASE: u8, const INITIAL: u32> {
    shared: &'a Shared,
    host: HostMainThreadHandle<'a>,
    selected: Cell<Option<ClapId>>,
}
struct Processor<'a>(&'a Shared);
impl<'a> PluginShared<'a> for Shared {}
impl<const C: u8, const I: u32> Plugin for ConfigPlugin<C, I> {
    type Shared<'a> = Shared;
    type MainThread<'a> = Main<'a, C, I>;
    type AudioProcessor<'a> = Processor<'a>;
    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder.register::<PluginAudioPorts>().register::<PluginState>();
        builder.register::<PluginAudioPortsConfig>();
        if C != 10 {
            builder.register::<PluginAudioPortsConfigInfo>();
        }
    }
}
impl<const C: u8, const I: u32> DefaultPluginFactory for ConfigPlugin<C, I> {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(ID, "Audio Config Fixture")
    }
    fn new_shared(_: HostSharedHandle<'_>) -> Result<Shared, PluginError> {
        Ok(Shared {
            channels: AtomicU32::new(I),
            gain: AtomicU32::new(INITIAL_GAIN),
            active: AtomicBool::new(false),
        })
    }
    fn new_main_thread<'a>(
        host: HostMainThreadHandle<'a>,
        shared: &'a Shared,
    ) -> Result<Main<'a, C, I>, PluginError> {
        Ok(Main { shared, host, selected: Cell::new(None) })
    }
}
impl<'a, const C: u8, const I: u32> PluginMainThread<'a, Shared> for Main<'a, C, I> {}
impl<const C: u8, const I: u32> Main<'_, C, I> {
    fn inactive(&self) {
        assert!(!self.shared.active.load(Ordering::Relaxed));
        let check = self.host.get_extension::<HostThreadCheck>().expect("actual host thread check");
        assert_eq!(check.is_main_thread(&self.host), Some(true));
        assert_eq!(check.is_audio_thread(&self.host), Some(false));
    }
    fn channels(&self) -> u32 {
        self.shared.channels.load(Ordering::Relaxed)
    }
}
impl<const C: u8, const I: u32> PluginAudioPortsConfigImpl for Main<'_, C, I> {
    fn count(&self) -> u32 {
        self.inactive();
        if C == 8 {
            129
        } else {
            3
        }
    }
    fn get(&self, index: u32, writer: &mut AudioPortConfigWriter) {
        self.inactive();
        if C == 7 && index == 2 {
            return;
        }
        let channels = if index == 0 { 1 } else { 2 };
        // Index2 normally has unsupported width, giving precisely one candidate.
        let channels = if index == 2 && C != 2 { 4 } else { channels };
        let main = MainPortInfo {
            channel_count: channels,
            port_type: if C == 14 {
                None
            } else {
                AudioPortType::from_channel_count(channels)
            },
        };
        writer.write(&AudioPortsConfiguration {
            id: ClapId::new(if C == 1 && index == 2 { 1 } else { index }),
            name: b"layout",
            input_port_count: 1,
            output_port_count: if C == 5 { 2 } else { 1 },
            main_input: if C == 13 { None } else { Some(main) },
            main_output: Some(main),
        });
    }
    fn select(&self, id: ClapId) -> Result<(), PluginError> {
        self.inactive();
        if C == 3 {
            return Err(PluginError::Message("select declined"));
        }
        if C != 4 {
            self.shared.channels.store(id.get() + 1, Ordering::Relaxed);
        }
        self.selected.set(Some(id));
        // Deliberately reset gain: native load must happen after select.
        self.shared.gain.store(SELECTED_GAIN, Ordering::Relaxed);
        Ok(())
    }
}
impl<const C: u8, const I: u32> PluginAudioPortsConfigInfoImpl for Main<'_, C, I> {
    fn current_config(&self) -> Option<ClapId> {
        self.inactive();
        Some(ClapId::new(self.channels() - 1))
    }
    fn get(&self, _: ClapId, _: u32, _: bool, _: &mut AudioPortInfoWriter) {}
}
impl<const C: u8, const I: u32> PluginAudioPortsImpl for Main<'_, C, I> {
    fn count(&self, is_input: bool) -> u32 {
        self.inactive();
        if C == 11 && !is_input {
            0
        } else if C == 6 && self.selected.get().is_some() && is_input {
            2
        } else {
            1
        }
    }
    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        self.inactive();
        assert!(
            index < PluginAudioPortsImpl::count(self, is_input),
            "no out-of-range port queries"
        );
        let channels = if index == 1 { 3 } else { self.channels() };
        writer.set(&AudioPortInfo {
            id: ClapId::new(index),
            name: b"Main",
            channel_count: channels,
            flags: if index == 0
                && C != 9
                && !(C == 12 && is_input && self.selected.get().is_some())
            {
                AudioPortFlags::IS_MAIN
            } else {
                AudioPortFlags::empty()
            },
            port_type: if C == 15 && self.selected.get().is_some() {
                None
            } else {
                AudioPortType::from_channel_count(channels)
            },
            in_place_pair: None,
        });
    }
}
impl<const C: u8, const I: u32> PluginStateImpl for Main<'_, C, I> {
    fn save(&self, out: &mut OutputStream) -> Result<(), PluginError> {
        self.inactive();
        out.write_all(&[
            self.channels() as u8,
            self.shared.gain.load(Ordering::Relaxed) as u8,
        ])?;
        Ok(())
    }
    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        self.inactive();
        let mut state = [0; 2];
        input.read_exact(&mut state)?;
        // Prove pre-load negotiation, not a select which overwrites restored gain.
        assert_eq!(self.channels(), if I == 1 { 2 } else { 1 });
        self.shared.channels.store(u32::from(state[0]), Ordering::Relaxed);
        self.shared.gain.store(u32::from(state[1]), Ordering::Relaxed);
        Ok(())
    }
}
impl<'a, const C: u8, const I: u32>
    clack_plugin::plugin::PluginAudioProcessor<'a, Shared, Main<'a, C, I>> for Processor<'a>
{
    fn activate(
        _: HostAudioProcessorHandle<'a>,
        main: &Main<'a, C, I>,
        shared: &'a Shared,
        _: PluginConfig,
    ) -> Result<Self, PluginError> {
        main.inactive();
        shared.active.store(true, Ordering::Relaxed);
        Ok(Self(shared))
    }
    fn deactivate(self, _: &Main<'a, C, I>) {
        self.0.active.store(false, Ordering::Relaxed);
    }
    fn process(
        &mut self,
        _: Process,
        mut audio: Audio,
        _: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let mut port = audio.port_pair(0).unwrap();
        let mut channels = port.channels()?.into_f32().unwrap();
        assert_eq!(
            channels.iter_mut().count(),
            self.0.channels.load(Ordering::Relaxed) as usize
        );
        for channel in channels.iter_mut() {
            match channel {
                ChannelPair::InputOutput(input, output) => {
                    for (x, y) in input.iter().zip(output.iter_mut()) {
                        *y = *x * self.0.gain.load(Ordering::Relaxed) as f32;
                    }
                }
                _ => return Err(PluginError::Message("separate buffers expected")),
            }
        }
        Ok(ProcessStatus::Continue)
    }
}
fn entry<const C: u8, const I: u32>() -> PluginEntry {
    PluginEntry::load_from_clack::<SinglePluginEntry<ConfigPlugin<C, I>>>(c"/test/audio-config")
        .unwrap()
}
fn render(layout: AudioChannelLayout, mode: AudioProcessingMode) -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 44_100,
        channel_layout: layout,
        max_block_frames: 7,
        processing_mode: mode,
        processor_session_scratch_budget_bytes: 1024 * 1024,
        public_output_lookahead_budget_frames: 1024,
        compensation_delay_scratch_budget_bytes: 1024 * 1024,
    }
}
fn request(render: AudioRenderContract) -> IsolatedAudioProcessorWorkerPrepareRequest {
    IsolatedAudioProcessorWorkerPrepareRequest {
        musical_time_reference: None,
        auxiliary_outputs: Default::default(),
        note_input_contract: Default::default(),
        native_lane: None,
        payload: Vec::new(),
        plugin_execution_contract: AudioProcessorExecutionContract::new(
            0,
            AudioProcessorTail::None,
            true,
            true,
            true,
            0,
        )
        .unwrap(),
        auxiliary_inputs: Default::default(),
        parameter_ids: Vec::new(),
        render_contract: render,
    }
}

#[test]
fn cold_config_selection_precedes_state_and_processes_mono_stereo_pcm() {
    fn run<const INITIAL: u32>(layout: AudioChannelLayout) {
        for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
            for gain in [None, Some(9_u8)] {
                let render = render(layout, mode);
                let channels = render.channel_count();
                let supplied = gain.map(|gain| [channels as u8, gain]);
                let entry = entry::<0, INITIAL>();
                let probe = crate::processor_isolation::clap_discovery::probe_loaded(
                    &entry,
                    ID,
                    render,
                    supplied.as_ref().map(|s| s.as_slice()),
                    true,
                )
                .unwrap();
                let state = supplied
                    .as_ref()
                    .map(|s| s.as_slice())
                    .or(probe.captured_state.as_deref())
                    .unwrap();
                // Selecting a layout resets the fixture gain to1 before capture.
                let expected_gain = gain.unwrap_or(SELECTED_GAIN as u8);
                assert_eq!(state, &[channels as u8, expected_gain]);
                let mut worker = prepare_loaded(
                    request(render),
                    entry,
                    c"org.mondrian.test.audio-config",
                    Some(state),
                    &[],
                    &[],
                )
                .unwrap();
                worker.enter_state(0).unwrap();
                let auxiliary = AudioProcessorAuxiliaryInputContract::default();
                for block in 0..3 {
                    let frames = render.max_block_frames;
                    let offset = i64::try_from(block * frames).unwrap();
                    let mut pcm: Vec<f32> = (0..frames * channels)
                        .map(|i| (offset as f32 + i as f32 - 5.0) / 32.0)
                        .collect();
                    let expected: Vec<_> =
                        pcm.iter().map(|x| *x * f32::from(expected_gain)).collect();
                    worker
                        .process(IsolatedAudioProcessorWorkerBlock::new(
                            offset,
                            frames,
                            render,
                            &mut pcm,
                            &auxiliary,
                            &[],
                            &[],
                            &[],
                        ))
                        .unwrap();
                    assert_eq!(pcm, expected);
                }
            }
        }
    }
    run::<1>(AudioChannelLayout::Stereo);
    run::<2>(AudioChannelLayout::Mono);
}

#[test]
fn cold_config_selection_refuses_ambiguous_malformed_declined_and_false_summaries() {
    fn reject<const CASE: u8>(reason: &str) {
        // The zero-output case needs a valid matching input to reach that
        // refusal, rather than failing first on an unrelated Mono input.
        let entry = if CASE == 11 {
            entry::<CASE, 2>()
        } else {
            entry::<CASE, 1>()
        };
        let result = crate::processor_isolation::clap_discovery::probe_loaded(
            &entry,
            ID,
            render(AudioChannelLayout::Stereo, AudioProcessingMode::Offline),
            None,
            true,
        );
        let refusal = result.err().map(|error| error.to_string());
        assert!(
            refusal.as_ref().is_some_and(|error| error.contains(reason)),
            "case {CASE}: expected {reason}, got {refusal:?}"
        );
    }
    reject::<1>("duplicated");
    reject::<2>("explicit choice");
    reject::<3>("select failed");
    reject::<4>("ID was not applied");
    reject::<5>("port counts disagree");
    reject::<6>("channel_count");
    reject::<7>("metadata is unavailable");
    reject::<8>("capacity");
    reject::<9>("main_role");
    reject::<11>("bounded inputs");
    reject::<12>("Main roles or types disagree");
    reject::<13>("Main roles or types disagree");
    reject::<14>("Main roles or types disagree");
    reject::<15>("Main roles or types disagree");
}

#[test]
fn cold_config_preserves_compatible_default_and_never_reselects_restored_state() {
    let render = render(AudioChannelLayout::Stereo, AudioProcessingMode::Offline);
    // Same-layout defaults are preserved even if the offered list is ambiguous.
    let probe = crate::processor_isolation::clap_discovery::probe_loaded(
        &entry::<2, 2>(),
        ID,
        render,
        None,
        true,
    )
    .unwrap();
    assert_eq!(
        probe.captured_state.as_deref(),
        Some([2, INITIAL_GAIN as u8].as_slice())
    );
    // An incompatible restored topology is refused; load is never followed by select.
    let result = crate::processor_isolation::clap_discovery::probe_loaded(
        &entry::<0, 1>(),
        ID,
        render,
        Some(&[1, 9]),
        false,
    );
    assert!(result.is_err_and(|error| error.to_string().contains("reason=port_type")));
    let worker_result = prepare_loaded(
        request(render),
        entry::<0, 1>(),
        c"org.mondrian.test.audio-config",
        Some(&[1, 9]),
        &[],
        &[],
    );
    assert!(worker_result.is_err_and(|error| error.to_string().contains("reason=port_type")));
    // Config-info is optional; full actual ports still have to pass.
    let probe = crate::processor_isolation::clap_discovery::probe_loaded(
        &entry::<10, 1>(),
        ID,
        render,
        None,
        true,
    )
    .unwrap();
    assert_eq!(
        probe.captured_state.as_deref(),
        Some([2, SELECTED_GAIN as u8].as_slice())
    );
}
