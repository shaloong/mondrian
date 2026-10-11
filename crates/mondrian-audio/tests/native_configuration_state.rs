//! Independent native configuration-state oracle: SDK state need not store DSP history.

use clack_extensions::latency::PluginLatency;
use clack_extensions::render::{PluginRender, RenderMode};
use clack_extensions::state::PluginState;
use clack_extensions::thread_check::{HostThreadCheck, HostThreadCheckImpl};
use clack_host::events::event_types::{TransportEvent, TransportFlags};
use clack_host::events::{EventFlags, EventHeader};
use clack_host::prelude::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, EventBuffer, HostExtensions, HostHandlers,
    HostInfo, InputChannel, OutputEvents, PluginAudioConfiguration, PluginAudioProcessor,
    PluginEntry, PluginInstance, SharedHandler,
};
use clack_host::utils::{BeatTime, SecondsTime};
use std::marker::PhantomData;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, ThreadId};
use vst3_host::audio::{BusDirection, MediaType};
use vst3_host::plugin::ProcessMode;
use vst3_host::Vst3Host;

const FRAMES: usize = 16;
const SIGNAL: f32 = 2.625;
const DELAY: usize = 3;

#[test]
#[ignore = "owned VST3 sidechain fixture; direct SDK state/history oracle, no production reprepare API"]
fn installed_vst3_configuration_state_is_not_dsp_checkpoint_reference() {
    let binary = fixture_path("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN");
    let mut host = Vst3Host::builder()
        .sample_rate(48_000.0)
        .block_size(FRAMES)
        .input_channels(2)
        .output_channels(2)
        .build()
        .expect("independent reference host");
    let mut old = host.load_plugin(&binary).expect("owned reference component");
    old.load_state(&vst3_configuration()).expect("owned configuration");
    old.set_process_mode(ProcessMode::Offline).expect("reference offline mode");
    activate_inputs(&mut old);
    old.start_processing().expect("old reference start");
    assert_eq!(old.latency_samples() as usize, DELAY);
    let mut old_buffers = old.create_bus_audio_buffers(FRAMES).expect("old reference buffers");
    fill_vst3(&mut old_buffers, 0);
    old.process_bus_audio(&mut old_buffers).expect("old reference history");
    assert_reference(&old_buffers.outputs[0].channels, 0, true);

    // Capture actual IComponent/IEditController getState while no DSP call is
    // in flight. This is configuration, with no reset or post-DSP window retry.
    let captured = old.save_state().expect("actual native configuration capture");
    assert!(captured.len() <= 128 * 1024);
    fill_vst3(&mut old_buffers, FRAMES);
    old.process_bus_audio(&mut old_buffers).expect("old exact continuation");
    assert_reference(&old_buffers.outputs[0].channels, FRAMES, false);

    let mut restored = host.load_plugin(&binary).expect("independent fresh component");
    restored.load_state(&captured).expect("actual saved configuration restore");
    restored.set_process_mode(ProcessMode::Offline).expect("restored offline mode");
    activate_inputs(&mut restored);
    restored.start_processing().expect("fresh reference start");
    assert_eq!(restored.latency_samples() as usize, DELAY);
    let mut restored_buffers = restored.create_bus_audio_buffers(FRAMES).expect("fresh buffers");
    fill_vst3(&mut restored_buffers, FRAMES);
    restored
        .process_bus_audio(&mut restored_buffers)
        .expect("fresh native configuration DSP");
    assert_reference(&restored_buffers.outputs[0].channels, FRAMES, true);
    assert_ne!(
        restored_buffers.outputs[0].channels,
        old_buffers.outputs[0].channels
    );
    old.stop_processing().expect("old teardown");
    restored.stop_processing().expect("fresh teardown");
}

#[test]
#[ignore = "owned CLAP sidechain fixture; direct SDK state/history oracle, no production reprepare API"]
fn installed_clap_configuration_state_is_not_dsp_checkpoint_reference() {
    let binary = fixture_path("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN");
    let mut configuration = vec![0];
    configuration.extend_from_slice(&0.5_f32.to_le_bytes());
    let mut old = ClapReference::new(&binary, &configuration);
    assert_reference(&old.process(0), 0, true);
    let mut captured = Vec::new();
    let handle = old.instance.plugin_handle();
    handle
        .get_extension::<PluginState>()
        .expect("fixture state extension")
        .save(&handle, &mut captured)
        .expect("actual CLAP configuration save");
    assert_eq!(
        captured, configuration,
        "native state stores configuration, not delay history"
    );
    let continued = old.process(FRAMES);
    assert!(
        !old.shared.restart.load(Ordering::Acquire),
        "mode0 must not request restart"
    );
    assert_reference(&continued, FRAMES, false);
    let mut restored = ClapReference::new(&binary, &captured);
    let fresh = restored.process(FRAMES);
    assert_reference(&fresh, FRAMES, true);
    assert_ne!(fresh, continued);
}

fn fixture_path(name: &str) -> PathBuf {
    std::env::var_os(name).map(PathBuf::from).expect("owned native fixture path")
}

fn assert_reference(channels: &[Vec<f32>], origin: usize, fresh: bool) {
    assert_eq!(channels.len(), 2);
    assert_eq!(channels[0].len(), FRAMES);
    assert_eq!(channels[1].len(), FRAMES);
    for (offset, (&left, &right)) in channels[0].iter().zip(&channels[1]).enumerate() {
        // Fixture equation is (2*main + 3*aux1 + 5*aux2)*gain.
        // Inputs are 1, .25, .5 times the ramp, gain .5, delay 3.
        let expected = if fresh && offset < DELAY {
            0.0
        } else {
            SIGNAL * (origin + offset + 1 - DELAY) as f32
        };
        assert_eq!(
            [left, right],
            [expected, -expected * 0.5],
            "independent origin {origin}, offset {offset}, fresh {fresh}"
        );
    }
}

fn vst3_configuration() -> Vec<u8> {
    vst3_configuration_for_mode(0)
}

fn vst3_configuration_for_mode(mode: u32) -> Vec<u8> {
    let mut component = b"MSS1".to_vec();
    component.extend_from_slice(&1_u32.to_le_bytes());
    component.extend_from_slice(&0.5_f64.to_le_bytes());
    component.extend_from_slice(&mode.to_le_bytes());
    let mut state = b"VST3HOST_STATE\0\0".to_vec();
    state.extend_from_slice(&1_u32.to_le_bytes());
    state.extend_from_slice(&(component.len() as u32).to_le_bytes());
    state.extend_from_slice(&(component.len() as u32).to_le_bytes());
    state.extend_from_slice(&component);
    state.extend_from_slice(&component);
    state
}

fn activate_inputs(plugin: &mut vst3_host::Plugin) {
    for index in 0..3 {
        plugin
            .set_bus_active(MediaType::Audio, BusDirection::Input, index, true)
            .expect("owned native input");
    }
    plugin
        .set_bus_active(MediaType::Audio, BusDirection::Output, 0, true)
        .expect("owned native output");
}

fn fill_vst3(buffers: &mut vst3_host::BusAudioBuffers, origin: usize) {
    for (bus, weight) in buffers.inputs.iter_mut().zip([1.0_f32, 0.25, 0.5]) {
        for offset in 0..FRAMES {
            let input = (origin + offset + 1) as f32 * weight;
            bus.channels[0][offset] = input;
            bus.channels[1][offset] = -input * 0.5;
        }
    }
}

struct ReferenceHost;

#[derive(Clone)]
struct ReferenceShared {
    main: ThreadId,
    audio: Arc<AtomicBool>,
    callback: Arc<AtomicBool>,
    restart: Arc<AtomicBool>,
}

impl ReferenceShared {
    fn audio_role(&self) -> ReferenceAudioRole<'_> {
        assert_eq!(thread::current().id(), self.main);
        let previous = self.audio.swap(true, Ordering::AcqRel);
        ReferenceAudioRole { shared: self, previous, owner: PhantomData }
    }
}

struct ReferenceAudioRole<'a> {
    shared: &'a ReferenceShared,
    previous: bool,
    owner: PhantomData<Rc<()>>,
}

impl Drop for ReferenceAudioRole<'_> {
    fn drop(&mut self) {
        self.shared.audio.store(self.previous, Ordering::Release);
    }
}

impl SharedHandler<'_> for ReferenceShared {
    fn request_restart(&self) {
        self.restart.store(true, Ordering::Release);
    }
    fn request_process(&self) {}
    fn request_callback(&self) {
        self.callback.store(true, Ordering::Release);
    }
}

impl HostThreadCheckImpl for ReferenceShared {
    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.main
    }
    fn is_audio_thread(&self) -> bool {
        self.is_main_thread() && self.audio.load(Ordering::Acquire)
    }
}

impl HostHandlers for ReferenceHost {
    type Shared<'a> = ReferenceShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
    fn declare_extensions(builder: &mut HostExtensions<Self>, _: &ReferenceShared) {
        builder.register::<HostThreadCheck>();
    }
}

struct ClapReference {
    processor: Option<PluginAudioProcessor<ReferenceHost>>,
    instance: PluginInstance<ReferenceHost>,
    shared: ReferenceShared,
    mode: u8,
    render_mode: RenderMode,
}

impl ClapReference {
    fn new(path: &std::path::Path, state: &[u8]) -> Self {
        Self::with_mode(path, state, RenderMode::Offline)
    }

    fn with_mode(path: &std::path::Path, state: &[u8], render_mode: RenderMode) -> Self {
        // SAFETY: this ignored reference test explicitly loads the owned native
        // fixture. PluginEntry owns the library for the instance lifetime.
        let entry = unsafe { PluginEntry::load(path.as_os_str()) }.expect("owned CLAP entry");
        let shared = ReferenceShared {
            main: thread::current().id(),
            audio: Arc::new(AtomicBool::new(false)),
            callback: Arc::new(AtomicBool::new(false)),
            restart: Arc::new(AtomicBool::new(false)),
        };
        let info = HostInfo::new(
            "Mondrian native state reference",
            "Mondrian",
            "https://github.com/shaloong/mondrian",
            env!("CARGO_PKG_VERSION"),
        )
        .expect("reference host info");
        let mut instance = PluginInstance::<ReferenceHost>::new(
            |_| shared.clone(),
            |_| (),
            &entry,
            c"org.mondrian.fixture.sidechain",
            &info,
        )
        .expect("reference native instance");
        let handle = instance.plugin_handle();
        handle
            .get_extension::<PluginState>()
            .expect("fixture state")
            .load(&handle, &mut &state[..])
            .expect("native configuration restore");
        handle
            .get_extension::<PluginRender>()
            .expect("fixture render")
            .set(&handle, render_mode)
            .expect("reference render mode");
        service_reference_callbacks(&shared, &mut instance);
        let processor = instance
            .activate(
                |_, _| (),
                PluginAudioConfiguration {
                    sample_rate: 48_000.0,
                    min_frames_count: 1,
                    max_frames_count: FRAMES as u32,
                },
            )
            .expect("reference activation")
            .into();
        Self {
            processor: Some(processor),
            instance,
            shared,
            mode: state[0],
            render_mode,
        }
    }

    fn process(&mut self, origin: usize) -> Vec<Vec<f32>> {
        let mut inputs: Vec<Vec<Vec<f32>>> = [1.0_f32, 0.25, 0.5]
            .into_iter()
            .map(|weight| {
                let left: Vec<_> =
                    (0..FRAMES).map(|offset| (origin + offset + 1) as f32 * weight).collect();
                let right = left.iter().map(|value| -value * 0.5).collect();
                vec![left, right]
            })
            .collect();
        let mut outputs = vec![vec![0.0; FRAMES]; 2];
        let mut input_ports = AudioPorts::with_capacity(6, 3);
        let mut output_ports = AudioPorts::with_capacity(2, 1);
        let input = input_ports.with_input_buffers(inputs.iter_mut().map(|port| AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_input_only(
                port.iter_mut().map(|channel| InputChannel::variable(&mut channel[..])),
            ),
        }));
        let mut output = output_ports.with_output_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_output_only(
                outputs.iter_mut().map(|channel| &mut channel[..]),
            ),
        }]);
        let events = EventBuffer::new();
        let transport = (self.mode == 7).then(|| reference_transport(origin));
        {
            let _role = self.shared.audio_role();
            self.processor
                .as_mut()
                .expect("reference processor")
                .ensure_processing_started()
                .expect("reference processing start")
                .process(
                    &input,
                    &mut output,
                    &events.as_input(),
                    &mut OutputEvents::void(),
                    None,
                    transport.as_ref(),
                )
                .expect("reference native DSP");
        }
        service_reference_callbacks(&self.shared, &mut self.instance);
        outputs
    }
}

impl Drop for ClapReference {
    fn drop(&mut self) {
        if let Some(mut processor) = self.processor.take() {
            {
                let _role = self.shared.audio_role();
                processor.ensure_processing_stopped();
            }
            drop(processor);
        }
        assert!(
            self.instance.try_deactivate().is_ok(),
            "reference native deactivate"
        );
    }
}

fn service_reference_callbacks(
    shared: &ReferenceShared,
    instance: &mut PluginInstance<ReferenceHost>,
) {
    for _ in 0..16 {
        if !shared.callback.swap(false, Ordering::AcqRel) {
            return;
        }
        instance.call_on_main_thread_callback();
    }
    panic!("owned mode0 reference requested unbounded callbacks");
}

#[test]
#[ignore = "owned VST3 fixture modes5/6; direct SDK stable component-profile lifecycle and seed control"]
fn installed_vst3_constant_profile_reprepare_reference() {
    let binary = fixture_path("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN");
    for mode in [ProcessMode::Realtime, ProcessMode::Offline] {
        let mut host = Vst3Host::builder()
            .sample_rate(48_000.0)
            .block_size(FRAMES)
            .input_channels(2)
            .output_channels(2)
            .build()
            .expect("profile reference host");
        let mut plugin = host.load_plugin(&binary).expect("profile reference component");
        plugin
            .load_state(&vst3_configuration_for_mode(5))
            .expect("component initial profile");
        plugin.set_process_mode(mode).expect("reference mode");
        activate_inputs(&mut plugin);
        plugin.start_processing().expect("reference start");
        assert_eq!(plugin.latency_samples(), 3);
        let mut buffers =
            plugin.create_bus_audio_buffers(FRAMES).expect("reference profile buffers");
        fill_vst3(&mut buffers, 0);
        plugin
            .process_bus_audio(&mut buffers)
            .expect("real DSP records component desired profile");
        assert_reference(&buffers.outputs[0].channels, 0, true);
        assert_eq!(
            plugin.latency_samples(),
            3,
            "active contract remains legal after completed DSP"
        );

        // The unchanged .5 value is not what changes component configuration.
        // A real owning ConnectionPoint message queries the component's desired
        // non-parameter profile recorded above; only then the controller requests.
        plugin
            .set_parameter(0, 0.5)
            .expect("control-thread component profile synchronization");
        fill_vst3(&mut buffers, FRAMES);
        plugin
            .process_bus_audio(&mut buffers)
            .expect("old active contract still completes once");
        assert_reference(&buffers.outputs[0].channels, FRAMES, false);
        let flags = plugin.service_host_requests().expect("legal main-thread lifecycle");
        assert_eq!(
            flags.bits() as u32,
            1 << 3,
            "actual kLatencyChanged request is acknowledged"
        );
        assert_eq!(
            plugin.latency_samples(),
            7,
            "facts queried after final activation"
        );
        assert_eq!(plugin.tail_samples(), 7);
        assert!(
            plugin.take_restart_flags().is_empty(),
            "no repeated control request"
        );
        fill_vst3(&mut buffers, FRAMES * 2);
        plugin.process_bus_audio(&mut buffers).expect("new profile native DSP");
        assert_profile_reference(&buffers.outputs[0].channels, FRAMES * 2, 7);

        let seed = plugin.save_state().expect("actual component/controller configuration seed");
        // This is not a one-shot live instance disguised as a factory. Each
        // round independently restores configuration and queries final facts.
        for _ in 0..2 {
            let mut fresh = host.load_plugin(&binary).expect("independent seed component");
            fresh.load_state(&seed).expect("actual component/controller seed restore");
            fresh.set_process_mode(mode).expect("seed mode");
            activate_inputs(&mut fresh);
            fresh.start_processing().expect("final seed activation");
            assert_eq!(fresh.latency_samples(), 7);
            assert_eq!(fresh.tail_samples(), 7);
            let mut pcm = fresh.create_bus_audio_buffers(FRAMES).expect("seed native buffers");
            fill_vst3(&mut pcm, FRAMES * 2);
            fresh.process_bus_audio(&mut pcm).expect("fresh seed native PCM");
            assert_profile_reference(&pcm.outputs[0].channels, FRAMES * 2, 7);
            assert!(
                fresh.take_restart_flags().is_empty(),
                "stable restored profile cannot oscillate"
            );
            fresh.stop_processing().expect("seed teardown");
        }
        plugin.stop_processing().expect("profile teardown");
    }
}

#[test]
#[ignore = "owned CLAP fixture mode7; direct SDK retained instance lifecycle and stable seed control"]
fn installed_clap_constant_profile_reprepare_reference() {
    let binary = fixture_path("MONDRIAN_CLAP_SIDECHAIN_TEST_PLUGIN");
    let mut state = vec![7];
    state.extend_from_slice(&0.5_f32.to_le_bytes());
    state.push(0);
    for mode in [RenderMode::Realtime, RenderMode::Offline] {
        let mut reference = ClapReference::with_mode(&binary, &state, mode);
        assert_eq!(reference.latency(), 3);
        let old = reference.process(0);
        assert_reference(&old, 0, true);
        assert_eq!(
            reference.latency(),
            3,
            "completed triggering block retains active contract"
        );
        assert!(
            reference.shared.restart.swap(false, Ordering::AcqRel),
            "actual core restart"
        );
        reference.reactivate();
        assert_eq!(
            reference.latency(),
            7,
            "new latency follows deactivate/reactivate"
        );
        assert!(
            !reference.shared.restart.load(Ordering::Acquire),
            "activation has no new request"
        );
        let new = reference.process(FRAMES);
        assert_profile_reference(&new, FRAMES, 7);
        let handle = reference.instance.plugin_handle();
        let mut seed = Vec::new();
        handle
            .get_extension::<PluginState>()
            .expect("profile state")
            .save(&handle, &mut seed)
            .expect("actual stable native configuration");
        let mut expected_seed = state.clone();
        expected_seed[5] = 1;
        assert_eq!(
            seed, expected_seed,
            "configuration flag persists separately from DSP history"
        );
        for _ in 0..2 {
            let mut fresh = ClapReference::with_mode(&binary, &seed, mode);
            assert_eq!(fresh.latency(), 7);
            assert_profile_reference(&fresh.process(FRAMES), FRAMES, 7);
            assert!(
                !fresh.shared.restart.load(Ordering::Acquire),
                "stable fresh profile has no restart loop"
            );
        }
    }
}

fn assert_profile_reference(channels: &[Vec<f32>], origin: usize, delay: usize) {
    assert_eq!(channels.len(), 2);
    assert_eq!(channels[0].len(), FRAMES);
    assert_eq!(channels[1].len(), FRAMES);
    for (offset, (&left, &right)) in channels[0].iter().zip(&channels[1]).enumerate() {
        let expected = if offset < delay {
            0.0
        } else {
            SIGNAL * (origin + offset + 1 - delay) as f32
        };
        assert_eq!(
            [left, right],
            [expected, -expected * 0.5],
            "independent activated profile origin {origin}, frame {offset}, lag {delay}"
        );
    }
}

impl ClapReference {
    fn latency(&mut self) -> usize {
        let handle = self.instance.plugin_handle();
        handle.get_extension::<PluginLatency>().expect("reference latency").get(&handle) as usize
    }

    fn reactivate(&mut self) {
        let mut previous = self.processor.take().expect("old reference audio handle");
        {
            let _role = self.shared.audio_role();
            previous.ensure_processing_stopped();
        }
        drop(previous);
        self.instance
            .try_deactivate()
            .expect("retained reference instance deactivation");
        service_reference_callbacks(&self.shared, &mut self.instance);
        let handle = self.instance.plugin_handle();
        handle
            .get_extension::<PluginRender>()
            .expect("reference render extension")
            .set(&handle, self.render_mode)
            .expect("inactive native render mode");
        self.processor = Some(
            self.instance
                .activate(
                    |_, _| (),
                    PluginAudioConfiguration {
                        sample_rate: 48_000.0,
                        min_frames_count: 1,
                        max_frames_count: FRAMES as u32,
                    },
                )
                .expect("retained native profile activation")
                .into(),
        );
    }
}

fn reference_transport(origin: usize) -> TransportEvent {
    // Owned inputs use small nonnegative integral grid positions. This is the
    // SDK seconds representation only; no musical facts are fabricated.
    let numerator = origin as i128 * (1_i128 << 31);
    let ticks = numerator / 48_000 + i128::from(numerator % 48_000 * 2 >= 48_000);
    TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags: TransportFlags::HAS_SECONDS_TIMELINE,
        song_pos_seconds: SecondsTime::from_bits(ticks as i64),
        song_pos_beats: BeatTime::from_bits(0),
        tempo: 0.0,
        tempo_inc: 0.0,
        loop_start_beats: BeatTime::from_bits(0),
        loop_end_beats: BeatTime::from_bits(0),
        loop_start_seconds: SecondsTime::from_bits(0),
        loop_end_seconds: SecondsTime::from_bits(0),
        bar_start: BeatTime::from_bits(0),
        bar_number: 0,
        time_signature_numerator: 0,
        time_signature_denominator: 0,
    }
}
