//! Allocation and signal oracles over the actual child-side CLAP worker.
//!
//! The allocator records successful requested bytes on the current test thread.
//! Its const TLS bookkeeping allocates no memory and is disabled outside the
//! measured operation. This is not RSS or a third-party plugin heap measurement.

use super::super::{ClapParameterDescriptor, SharedParameterEvent};
use super::*;
use crate::{AudioProcessingMode, AudioProcessorFactory, AudioProcessorTail, AudioRenderContract};
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams, PluginParams,
};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::OutputEvents;
use clack_plugin::prelude::{
    Audio, ChannelPair, DefaultPluginFactory, Events, HostAudioProcessorHandle,
    HostMainThreadHandle, HostSharedHandle, Plugin, PluginAudioConfiguration as PluginConfig,
    PluginDescriptor, PluginError, PluginExtensions, PluginMainThread, Process, ProcessStatus,
    SinglePluginEntry,
};
use mondrian_core::{AudioChannelLayout, ParameterId};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AllocationCounts {
    allocations: usize,
    reallocations: usize,
    requested_bytes: usize,
    largest_request: usize,
}

thread_local! {
    static COUNTS: Cell<Option<AllocationCounts>> = const { Cell::new(None) };
}

struct MeasuringAllocator;

fn record_allocation(bytes: usize, reallocation: bool) {
    let _ = COUNTS.try_with(|cell| {
        if let Some(mut counts) = cell.get() {
            counts.allocations += usize::from(!reallocation);
            counts.reallocations += usize::from(reallocation);
            counts.requested_bytes += bytes;
            counts.largest_request = counts.largest_request.max(bytes);
            cell.set(Some(counts));
        }
    });
}

// SAFETY: every memory operation forwards the original arguments to System.
// Recording uses only const-initialized, non-allocating thread-local integer cells.
unsafe impl GlobalAlloc for MeasuringAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc(layout) };
        if !result.is_null() {
            record_allocation(layout.size(), false);
        }
        result
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc_zeroed(layout) };
        if !result.is_null() {
            record_allocation(layout.size(), false);
        }
        result
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, layout, bytes) };
        if !result.is_null() {
            record_allocation(bytes, true);
        }
        result
    }
}

#[global_allocator]
static ALLOCATOR: MeasuringAllocator = MeasuringAllocator;

fn measure<T>(execute: impl FnOnce() -> T) -> (T, AllocationCounts) {
    struct Restore(Option<AllocationCounts>);
    impl Drop for Restore {
        fn drop(&mut self) {
            COUNTS.with(|cell| cell.set(self.0));
        }
    }
    let previous = COUNTS.with(|cell| cell.replace(Some(AllocationCounts::default())));
    let restore = Restore(previous);
    let result = execute();
    let counts = COUNTS.with(|cell| cell.get()).expect("active measurement");
    drop(restore);
    (result, counts)
}

struct ParameterOrderPlugin;
struct ParameterOrderMain;
struct ParameterOrderProcessor;

impl Plugin for ParameterOrderPlugin {
    type AudioProcessor<'a> = ParameterOrderProcessor;
    type Shared<'a> = ();
    type MainThread<'a> = ParameterOrderMain;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&()>) {
        builder.register::<PluginAudioPorts>().register::<PluginParams>();
    }
}

impl DefaultPluginFactory for ParameterOrderPlugin {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "org.mondrian.test.parameter-storage",
            "Parameter Storage Oracle",
        )
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<(), PluginError> {
        Ok(())
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        _shared: &'a (),
    ) -> Result<ParameterOrderMain, PluginError> {
        Ok(ParameterOrderMain)
    }
}

impl PluginMainThread<'_, ()> for ParameterOrderMain {}

impl PluginAudioPortsImpl for ParameterOrderMain {
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

impl PluginMainThreadParams for ParameterOrderMain {
    fn count(&self) -> u32 {
        2
    }

    fn get_info(&self, index: u32, writer: &mut ParamInfoWriter) {
        let Some(id) = [17, 3].get(index as usize).copied() else {
            return;
        };
        writer.set(&ParamInfo {
            id: ClapId::new(id),
            flags: ParamInfoFlags::IS_AUTOMATABLE,
            cookie: Default::default(),
            name: if id == 17 { b"Left" } else { b"Right" },
            module: b"",
            min_value: 0.0,
            max_value: 1.0,
            default_value: 1.0,
        });
    }

    fn get_value(&self, id: ClapId) -> Option<f64> {
        [ClapId::new(17), ClapId::new(3)].contains(&id).then_some(1.0)
    }

    fn value_to_text(
        &self,
        _id: ClapId,
        _value: f64,
        _writer: &mut ParamDisplayWriter,
    ) -> std::fmt::Result {
        Err(std::fmt::Error)
    }

    fn text_to_value(&self, _id: ClapId, _text: &CStr) -> Option<f64> {
        None
    }

    fn flush(&self, _input: &clack_plugin::prelude::InputEvents, _output: &mut OutputEvents) {}
}

impl PluginAudioProcessorParams for ParameterOrderProcessor {
    fn flush(&mut self, _input: &clack_plugin::prelude::InputEvents, _output: &mut OutputEvents) {}
}

impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, (), ParameterOrderMain>
    for ParameterOrderProcessor
{
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main: &ParameterOrderMain,
        _shared: &'a (),
        _config: PluginConfig,
    ) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let frames = audio.frames_count() as usize;
        if events.input.len() as usize != frames * 2 {
            return Err(PluginError::Message(
                "the oracle requires full admitted parameter storage",
            ));
        }
        // Independent native ABI oracle: IDs are intentionally not numeric order.
        // Equal-time values must retain lane insertion order 17 then 3.
        for (index, event) in events.input.iter().enumerate() {
            let Some(CoreEventSpace::ParamValue(value)) = event.as_core_event() else {
                return Err(PluginError::Message(
                    "expected a standard parameter value event",
                ));
            };
            let frame = index / 2;
            let lane = index % 2;
            let expected_value = if lane == 0 {
                (frame + 1) as f64 / frames as f64
            } else {
                (frames - frame) as f64 / frames as f64
            };
            if value.param_id() != Some(ClapId::new(if lane == 0 { 17 } else { 3 }))
                || event.header().time() as usize != frame
                || value.value() != expected_value
                || !value.cookie().as_raw().is_null()
            {
                return Err(PluginError::Message(
                    "parameter order, timestamp or value differs",
                ));
            }
        }
        let mut pair = audio.port_pair(0).ok_or(PluginError::Message("missing main port"))?;
        let mut channels = pair
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("missing Float32 channels"))?;
        for (channel, pair) in channels.iter_mut().enumerate() {
            match pair {
                ChannelPair::InputOutput(input, output) => {
                    for (frame, (input, output)) in input.iter().zip(output).enumerate() {
                        let gain = if channel == 0 {
                            frame + 1
                        } else {
                            frames - frame
                        };
                        *output = *input * (gain as f32 / frames as f32);
                    }
                }
                ChannelPair::InPlace(samples) => {
                    for (frame, sample) in samples.iter_mut().enumerate() {
                        let gain = if channel == 0 {
                            frame + 1
                        } else {
                            frames - frame
                        };
                        *sample *= gain as f32 / frames as f32;
                    }
                }
                _ => return Err(PluginError::Message("unpaired channel")),
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

fn render_contract(frames: usize, mode: AudioProcessingMode) -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: frames,
        processing_mode: mode,
        processor_session_scratch_budget_bytes: usize::MAX,
        public_output_lookahead_budget_frames: usize::MAX,
        compensation_delay_scratch_budget_bytes: usize::MAX,
    }
}

fn prepared_worker(
    frames: usize,
    mode: AudioProcessingMode,
) -> (Box<dyn IsolatedAudioProcessorWorker>, AllocationCounts) {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<ParameterOrderPlugin>>(
        c"/test/parameter-storage",
    )
    .expect("SDK-owned reference entry");
    let request = IsolatedAudioProcessorWorkerPrepareRequest {
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
        .expect("reference contract"),
        auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
        parameter_ids: vec![
            ParameterId::new("clap.param.17").expect("left parameter"),
            ParameterId::new("clap.param.3").expect("right parameter"),
        ],
        render_contract: render_contract(frames, mode),
    };
    let descriptors = [17, 3].map(|id| ClapParameterDescriptor {
        id,
        name: if id == 17 { "Left" } else { "Right" }.to_owned(),
        min_value: 0.0,
        max_value: 1.0,
        default_value: 1.0,
        current_value: 1.0,
        automatable: true,
        stepped: false,
        enumeration: false,
        read_only: false,
        hidden: false,
    });
    let (worker, counts) = measure(|| {
        prepare_loaded(
            request,
            entry,
            c"org.mondrian.test.parameter-storage",
            None,
            &descriptors,
            &[17, 3],
        )
    });
    (worker.expect("actual product CLAP preparation"), counts)
}

#[test]
fn clap_parameter_preparation_allocates_only_typed_events_ordinals_and_pcm_per_frame() {
    // Exercise lazy owner/test/SDK setup outside measurement before both samples.
    drop(prepared_worker(1, AudioProcessingMode::Offline).0);
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let (small, small_counts) = prepared_worker(64, mode);
        let (large, large_counts) = prepared_worker(4096, mode);
        assert_eq!(large_counts.reallocations, small_counts.reallocations);
        assert_eq!(large_counts.allocations, small_counts.allocations);
        let native_event_owners = 2 * (size_of::<ParamValueEvent>() + size_of::<usize>());
        let planar_pcm_owners = 2 * 2 * size_of::<f32>();
        assert_eq!(
            large_counts.requested_bytes.checked_sub(small_counts.requested_bytes),
            Some((4096 - 64) * (native_event_owners + planar_pcm_owners)),
            "only host-owned per-frame storage may scale; actual small={small_counts:?}, large={large_counts:?}",
        );
        drop((small, large));
    }
}

#[test]
fn clap_parameter_full_capacity_reset_process_is_allocation_free_and_preserves_abi_order() {
    let frames = 64;
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let (mut worker, _) = prepared_worker(frames, mode);
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let parameter_ids = [
            ParameterId::new("clap.param.17").expect("left parameter"),
            ParameterId::new("clap.param.3").expect("right parameter"),
        ];
        let mut events = Vec::with_capacity(frames * 2);
        for lane in 0..2 {
            for frame in 0..frames {
                events.push(SharedParameterEvent {
                    lane,
                    sample_offset: frame as u32,
                    value: if lane == 0 {
                        (frame + 1) as f64 / frames as f64
                    } else {
                        (frames - frame) as f64 / frames as f64
                    },
                });
            }
        }
        let mut pcm = vec![0.5_f32; frames * 2];
        // Warm the same owner and first-start SDK path outside the measured interval.
        worker.enter_state(0).expect("warm state entry");
        worker
            .process(IsolatedAudioProcessorWorkerBlock::new(
                0,
                frames,
                render_contract(frames, mode),
                &mut pcm,
                &auxiliary,
                &[],
                &parameter_ids,
                &events,
            ))
            .expect("warm full-capacity process");
        for origin in [0, 73, -31, 4096] {
            pcm.fill(0.5);
            let (result, counts) = measure(|| {
                worker.enter_state(origin)?;
                worker.process(IsolatedAudioProcessorWorkerBlock::new(
                    origin,
                    frames,
                    render_contract(frames, mode),
                    &mut pcm,
                    &auxiliary,
                    &[],
                    &parameter_ids,
                    &events,
                ))
            });
            result.expect("reset and full-capacity product process");
            assert_eq!(
                counts,
                AllocationCounts::default(),
                "callback allocation: {counts:?}"
            );
            for (frame, samples) in pcm.chunks_exact(2).enumerate() {
                assert_eq!(samples[0], 0.5 * ((frame + 1) as f32 / frames as f32));
                assert_eq!(samples[1], 0.5 * ((frames - frame) as f32 / frames as f32));
            }
        }
    }
}

// This reference reuses the existing independent parameter-order PCM oracle.
// It only adds actual native CLAP log callbacks; it links no host log parser.
struct LogOrderPlugin;
struct LogOrderProcessor<'a> {
    inner: ParameterOrderProcessor,
    host: HostSharedHandle<'a>,
    logger: clack_extensions::log::HostLog,
}

impl Plugin for LogOrderPlugin {
    type AudioProcessor<'a> = LogOrderProcessor<'a>;
    type Shared<'a> = ();
    type MainThread<'a> = ParameterOrderMain;
    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&()>) {
        builder.register::<PluginAudioPorts>().register::<PluginParams>();
    }
}
impl DefaultPluginFactory for LogOrderPlugin {
    fn get_descriptor() -> PluginDescriptor {
        ParameterOrderPlugin::get_descriptor()
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<(), PluginError> {
        Ok(())
    }
    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        _shared: &'a (),
    ) -> Result<ParameterOrderMain, PluginError> {
        Ok(ParameterOrderMain)
    }
}
impl PluginAudioProcessorParams for LogOrderProcessor<'_> {
    fn flush(&mut self, input: &clack_plugin::prelude::InputEvents, output: &mut OutputEvents) {
        self.inner.flush(input, output);
    }
}
impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, (), ParameterOrderMain>
    for LogOrderProcessor<'a>
{
    fn activate(
        host: HostAudioProcessorHandle<'a>,
        _main: &ParameterOrderMain,
        _shared: &'a (),
        _config: PluginConfig,
    ) -> Result<Self, PluginError> {
        let host = host.shared();
        let logger = host
            .get_extension::<clack_extensions::log::HostLog>()
            .ok_or(PluginError::Message("host log extension is unavailable"))?;
        Ok(Self { inner: ParameterOrderProcessor, host, logger })
    }
    fn process(
        &mut self,
        process: Process,
        audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        use clack_extensions::log::LogSeverity;
        for severity in [
            LogSeverity::Debug,
            LogSeverity::Info,
            LogSeverity::Warning,
            LogSeverity::Error,
            LogSeverity::Fatal,
            LogSeverity::HostMisbehaving,
            LogSeverity::PluginMisbehaving,
        ] {
            self.logger.log(&self.host, severity, c"bounded native log 🙂");
        }
        self.inner.process(process, audio, events)
    }
}

fn prepared_log_worker(
    frames: usize,
    mode: AudioProcessingMode,
) -> Result<Box<dyn IsolatedAudioProcessorWorker>, AudioProcessorHostError> {
    let entry =
        PluginEntry::load_from_clack::<SinglePluginEntry<LogOrderPlugin>>(c"/test/log-order")
            .expect("owned independent native logger entry");
    let request = IsolatedAudioProcessorWorkerPrepareRequest {
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
        .expect("oracle contract"),
        auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
        parameter_ids: vec![
            ParameterId::new("clap.param.17").expect("left"),
            ParameterId::new("clap.param.3").expect("right"),
        ],
        render_contract: render_contract(frames, mode),
    };
    let descriptors = [17, 3].map(|id| ClapParameterDescriptor {
        id,
        name: if id == 17 { "Left" } else { "Right" }.to_owned(),
        min_value: 0.0,
        max_value: 1.0,
        default_value: 1.0,
        current_value: 1.0,
        automatable: true,
        stepped: false,
        enumeration: false,
        read_only: false,
        hidden: false,
    });
    prepare_loaded(
        request,
        entry,
        c"org.mondrian.test.parameter-storage",
        None,
        &descriptors,
        &[17, 3],
    )
}

#[test]
fn clap_host_log_native_producer_requires_actual_host_extension_on_activation() {
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        // Existing public/native API only. Before host behavior is implemented
        // this fails at actual CLAP activation, not a missing new Rust method.
        assert!(
            prepared_log_worker(64, mode).is_ok(),
            "an actual legal producer needs the advertised host log extension"
        );
    }
}

#[test]
fn clap_host_log_full_parameter_callback_has_zero_allocations_and_preserves_seven_severities_pcm() {
    let frames = 64;
    for mode in [AudioProcessingMode::Realtime, AudioProcessingMode::Offline] {
        let mut worker =
            prepared_log_worker(frames, mode).expect("actual log-capable product worker");
        let parameter_ids = [
            ParameterId::new("clap.param.17").expect("left"),
            ParameterId::new("clap.param.3").expect("right"),
        ];
        let mut events = Vec::with_capacity(frames * 2);
        for lane in 0..2 {
            for frame in 0..frames {
                events.push(SharedParameterEvent {
                    lane,
                    sample_offset: frame as u32,
                    value: if lane == 0 {
                        (frame + 1) as f64 / frames as f64
                    } else {
                        (frames - frame) as f64 / frames as f64
                    },
                });
            }
        }
        let auxiliary = AudioProcessorAuxiliaryInputContract::default();
        let mut pcm = vec![0.5; frames * 2];
        worker.enter_state(0).expect("warm same owner entry");
        worker
            .process(IsolatedAudioProcessorWorkerBlock::new(
                0,
                frames,
                render_contract(frames, mode),
                &mut pcm,
                &auxiliary,
                &[],
                &parameter_ids,
                &events,
            ))
            .expect("warm native callback");
        worker
            .poll_native_observations(crate::NativeAudioObservationConsumption::Drain)
            .expect("warm same-owner observation");
        for origin in [0, 73, -31, 4096] {
            pcm.fill(0.5);
            let (result, counts) = measure(|| {
                worker.enter_state(origin)?;
                worker.process(IsolatedAudioProcessorWorkerBlock::new(
                    origin,
                    frames,
                    render_contract(frames, mode),
                    &mut pcm,
                    &auxiliary,
                    &[],
                    &parameter_ids,
                    &events,
                ))
            });
            result.expect("actual full-capacity native logged DSP");
            assert_eq!(
                counts,
                AllocationCounts::default(),
                "no callback allocations/bookkeeping: {counts:?}"
            );
            let preserved = worker
                .poll_native_observations(crate::NativeAudioObservationConsumption::Preserve)
                .expect("pure capability poll");
            assert!(preserved.logs.records().is_empty());
            let observed = worker
                .poll_native_observations(crate::NativeAudioObservationConsumption::Drain)
                .expect("sole bounded receiver");
            assert!(!observed.logs.incomplete());
            assert_eq!(observed.logs.records().len(), 7);
            for (raw, record) in observed.logs.records().iter().enumerate() {
                assert_eq!(record.raw_severity(), raw as i32);
                assert!(record.severity().is_some());
                assert_eq!(record.text(), Ok("bounded native log 🙂"));
                assert!(!record.truncated() && !record.invalid_utf8());
            }
            assert!(worker
                .poll_native_observations(crate::NativeAudioObservationConsumption::Drain)
                .expect("no duplicate delivery")
                .logs
                .records()
                .is_empty());
            for (frame, samples) in pcm.chunks_exact(2).enumerate() {
                assert_eq!(samples[0], 0.5 * ((frame + 1) as f32 / frames as f32));
                assert_eq!(samples[1], 0.5 * ((frames - frame) as f32 / frames as f32));
            }
        }
    }
}

#[test]
fn native_factory_storage_actual_isolated_visitor_is_nonallocating_after_owners_exist() {
    let factory = super::super::tests::pure_seed_factory(
        crate::NativeConfigurationFormat::ClapState,
        Some(&[8; 32]),
    );
    let mut observed = Vec::with_capacity(64);
    // Warm the same interface and allocator TLS before measuring; clear only
    // local observation metadata, never the immutable factory or native state.
    factory
        .visit_host_owned_storage(&mut |storage| {
            observed.push(storage);
            Ok(())
        })
        .expect("warm observer");
    let expected = observed.len();
    observed.clear();
    let (result, counts) = measure(|| {
        factory.visit_host_owned_storage(&mut |storage| {
            observed.push(storage);
            Ok(())
        })
    });
    result.expect("same actual isolated factory observation");
    assert_eq!(observed.len(), expected);
    assert_eq!(counts.allocations, 0);
    assert_eq!(counts.reallocations, 0);
    assert_eq!(
        counts.requested_bytes, 0,
        "owning visitor/observation bookkeeping must not allocate"
    );
}
