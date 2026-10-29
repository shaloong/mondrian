//! Stopped, real COM-backed observation controls. No DLL, DSP or whole-service peak claim.
use super::*;
use crate::audio::{AudioBusObservationError as Stage, AudioBusQueryFrame, AudioBusRole};
use vst3::Class;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    Count(i32, i32),
    Info(i32, i32),
    Arrangement(i32, i32),
}
struct Probe {
    counts: [i32; 4],
    count_index: usize,
    calls: Vec<Call>,
    fail_info: Option<(i32, i32)>,
    role: Option<i32>,
    arrangement: Option<u64>,
    arrangement_result: tresult,
    fail_arrangement: Option<(i32, i32, tresult)>,
}
impl Probe {
    fn reset(&mut self, counts: [i32; 4]) {
        self.counts = counts;
        self.count_index = 0;
        self.calls.clear();
    }
}
struct Component {
    original: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    probe: Arc<Mutex<Probe>>,
}
impl Class for Component {
    type Interfaces = (IComponent, IAudioProcessor);
}
impl IPluginBaseTrait for Component {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        self.original.initialize(context)
    }
    unsafe fn terminate(&self) -> tresult {
        self.original.terminate()
    }
}
impl IComponentTrait for Component {
    unsafe fn getControllerClassId(&self, id: *mut TUID) -> tresult {
        self.original.getControllerClassId(id)
    }
    unsafe fn setIoMode(&self, mode: i32) -> tresult {
        self.original.setIoMode(mode)
    }
    unsafe fn getBusCount(&self, media: i32, direction: i32) -> i32 {
        if media != kAudio as i32 {
            return self.original.getBusCount(media, direction);
        }
        let mut p = self.probe.lock().unwrap();
        let count = p.counts[p.count_index];
        p.count_index += 1;
        p.calls.push(Call::Count(direction, count));
        count
    }
    unsafe fn getBusInfo(
        &self,
        media: i32,
        direction: i32,
        index: i32,
        info: *mut BusInfo,
    ) -> tresult {
        if media != kAudio as i32 {
            return self.original.getBusInfo(media, direction, index, info);
        }
        let (fail, role) = {
            let mut p = self.probe.lock().unwrap();
            p.calls.push(Call::Info(direction, index));
            (p.fail_info == Some((direction, index)), p.role)
        };
        // The real caller must reset the same POD slot before every COM query.
        assert_eq!((*info).channelCount, 0);
        assert_eq!((*info).name, [0; 128]);
        if fail {
            return kResultFalse;
        }
        let result = self.original.getBusInfo(media, direction, index, info);
        if result == kResultOk {
            // Unpaired UTF-16 is display-only and must not become a constraint error.
            (*info).name[0] = 0xd800;
            if let Some(role) = role {
                (*info).busType = role;
            }
        }
        result
    }
    unsafe fn getRoutingInfo(&self, input: *mut RoutingInfo, output: *mut RoutingInfo) -> tresult {
        self.original.getRoutingInfo(input, output)
    }
    unsafe fn activateBus(&self, media: i32, direction: i32, index: i32, active: u8) -> tresult {
        self.original.activateBus(media, direction, index, active)
    }
    unsafe fn setActive(&self, active: u8) -> tresult {
        self.original.setActive(active)
    }
    unsafe fn setState(&self, stream: *mut IBStream) -> tresult {
        self.original.setState(stream)
    }
    unsafe fn getState(&self, stream: *mut IBStream) -> tresult {
        self.original.getState(stream)
    }
}
impl IAudioProcessorTrait for Component {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut u64,
        ni: i32,
        outputs: *mut u64,
        no: i32,
    ) -> tresult {
        self.processor.setBusArrangements(inputs, ni, outputs, no)
    }
    unsafe fn getBusArrangement(&self, direction: i32, index: i32, output: *mut u64) -> tresult {
        let mut p = self.probe.lock().unwrap();
        p.calls.push(Call::Arrangement(direction, index));
        assert_eq!(*output, 0, "each native arrangement output starts at zero");
        if let Some(value) = p.arrangement {
            *output = value;
        }
        p.fail_arrangement
            .filter(|(dir, bus, _)| *dir == direction && *bus == index)
            .map(|(_, _, status)| status)
            .unwrap_or(p.arrangement_result)
    }
    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        self.processor.canProcessSampleSize(size)
    }
    unsafe fn getLatencySamples(&self) -> u32 {
        self.processor.getLatencySamples()
    }
    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        self.processor.setupProcessing(setup)
    }
    unsafe fn setProcessing(&self, active: u8) -> tresult {
        self.processor.setProcessing(active)
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        self.processor.process(data)
    }
    unsafe fn getTailSamples(&self) -> u32 {
        self.processor.getTailSamples()
    }
}
fn attach(plugin: &mut PluginImpl, counts: [i32; 4]) -> Arc<Mutex<Probe>> {
    let probe = Arc::new(Mutex::new(Probe {
        counts,
        count_index: 0,
        calls: Vec::with_capacity(4 + 4 * 32),
        fail_info: None,
        role: None,
        arrangement: Some(3),
        arrangement_result: kResultOk,
        fail_arrangement: None,
    }));
    let component = ComWrapper::new(Component {
        original: plugin.component.clone(),
        processor: plugin.processor.clone(),
        probe: Arc::clone(&probe),
    });
    plugin.component = component.to_com_ptr().unwrap();
    plugin.processor = component.to_com_ptr().unwrap();
    probe
}
fn assert_metadata_error(
    result: std::result::Result<crate::audio::AudioBusObservationRef<'_>, Stage>,
) {
    assert!(matches!(result, Err(Stage::Metadata(_))));
}

pub(super) fn query_order(plugin: &mut PluginImpl) {
    let probe = attach(plugin, [2, 1, 2, 1]);
    let native = plugin.native_audio_bus_info().unwrap();
    let layout = plugin.audio_bus_layout().unwrap();
    let arrangements = plugin.bus_arrangements().unwrap();
    let calls = probe.lock().unwrap().calls.clone();
    assert_eq!(
        calls,
        [
            Call::Count(kInput as i32, 2),
            Call::Info(kInput as i32, 0),
            Call::Info(kInput as i32, 1),
            Call::Count(kOutput as i32, 1),
            Call::Info(kOutput as i32, 0),
            Call::Count(kInput as i32, 2),
            Call::Arrangement(kInput as i32, 0),
            Call::Arrangement(kInput as i32, 1),
            Call::Count(kOutput as i32, 1),
            Call::Arrangement(kOutput as i32, 0),
        ]
    );
    probe.lock().unwrap().reset([2, 1, 2, 1]);
    let mut frame = AudioBusQueryFrame::default();
    let observed = plugin.observe_audio_buses_bounded(&mut frame).unwrap();
    for (facts, owned) in [
        (observed.native_inputs, &native.inputs),
        (observed.native_outputs, &native.outputs),
    ] {
        assert_eq!(facts.len(), owned.len());
        for (fact, owned) in facts.iter().zip(owned) {
            assert_eq!(
                (fact.role, fact.channel_count),
                (owned.role, owned.channel_count)
            );
        }
    }
    assert_eq!(observed.layout_inputs, layout.inputs);
    assert_eq!(observed.layout_outputs, layout.outputs);
    assert_eq!(observed.input_arrangements, arrangements.inputs);
    assert_eq!(observed.output_arrangements, arrangements.outputs);
    assert_eq!(probe.lock().unwrap().calls, calls);

    // Successful queries preserve the raw mask, including an explicit zero or
    // an untouched zero-initialized output. A prior value cannot leak through.
    for value in [Some(5), Some(0), None] {
        let mut p = probe.lock().unwrap();
        p.reset([2, 1, 2, 1]);
        p.arrangement_result = kResultOk;
        p.arrangement = value;
        drop(p);
        let observed = plugin.observe_audio_buses_bounded(&mut frame).unwrap();
        assert!(observed
            .input_arrangements
            .iter()
            .chain(observed.output_arrangements)
            .all(|v| v.raw() == value.unwrap_or(0)));
        assert_eq!(probe.lock().unwrap().calls, calls);
    }
}

// Exercise the same three successful stages as the borrowed observation. This
// helper owns only test results; it does not introduce a production query path.
fn owned_arrangements(plugin: &PluginImpl) -> Result<crate::audio::BusArrangements> {
    plugin.native_audio_bus_info()?;
    plugin.audio_bus_layout()?;
    plugin.bus_arrangements()
}

fn arrangement_failure_detail(direction: i32, index: i32, status: tresult) -> String {
    let direction = if direction == kInput as i32 {
        "input"
    } else {
        "output"
    };
    format!("getBusArrangement failed: direction={direction} index={index} status={status:#x}")
}

pub(super) fn arrangement_status(plugin: &mut PluginImpl) {
    let probe = attach(plugin, [2; 4]);
    let mut frame = AudioBusQueryFrame::default();
    let full_calls = [
        Call::Count(kInput as i32, 2),
        Call::Info(kInput as i32, 0),
        Call::Info(kInput as i32, 1),
        Call::Count(kOutput as i32, 2),
        Call::Info(kOutput as i32, 0),
        Call::Info(kOutput as i32, 1),
        Call::Count(kInput as i32, 2),
        Call::Arrangement(kInput as i32, 0),
        Call::Arrangement(kInput as i32, 1),
        Call::Count(kOutput as i32, 2),
        Call::Arrangement(kOutput as i32, 0),
        Call::Arrangement(kOutput as i32, 1),
    ];
    // No-bounds and bounded owning APIs must agree with the fixed-frame borrow.
    for bounded in [false, true] {
        plugin.processing_resource_bounds =
            bounded.then(|| crate::audio::ProcessingResourceBounds::new(2, 2, 8, 4).unwrap());
        for direction in [kInput as i32, kOutput as i32] {
            for index in [0, 1] {
                let stop = full_calls
                    .iter()
                    .position(|call| *call == Call::Arrangement(direction, index))
                    .unwrap()
                    + 1;
                let expected_calls = &full_calls[..stop];
                for status in [kResultFalse, kInvalidArgument, kNotImplemented] {
                    // Even a plausible stereo mask, arbitrary bits, or untouched
                    // output cannot turn a failed native query into a fact.
                    for value in [Some(3), Some(u64::MAX), None] {
                        let configure = || {
                            let mut p = probe.lock().unwrap();
                            p.reset([2; 4]);
                            p.arrangement = value;
                            p.arrangement_result = kResultOk;
                            p.fail_arrangement = Some((direction, index, status));
                            // Legal native roles may already fail a host's shape;
                            // the complete SDK census must succeed before that
                            // separate domain comparison can consume these facts.
                            p.role = Some(BusTypes_::kAux as i32);
                        };
                        let expected = arrangement_failure_detail(direction, index, status);
                        configure();
                        let error = owned_arrangements(plugin).expect_err(
                            "native arrangement failure must not produce an owned inventory",
                        );
                        let Error::InterfaceError(detail) = error else {
                            panic!("owned arrangement error must retain InterfaceError: {error:?}");
                        };
                        assert_eq!(detail, expected);
                        assert_eq!(probe.lock().unwrap().calls, expected_calls);
                        configure();
                        match plugin.observe_audio_buses_bounded(&mut frame) {
                            Err(Stage::Arrangements(Error::InterfaceError(detail))) => {
                                assert_eq!(detail, expected);
                            }
                            other => panic!(
                                "borrowed arrangement error must retain stage and facts: {other:?}"
                            ),
                        }
                        assert_eq!(probe.lock().unwrap().calls, expected_calls);
                        // Reuse the same frame after the failed prefix. All of
                        // its successful rows must come from this new zero mask.
                        {
                            let mut p = probe.lock().unwrap();
                            p.reset([2; 4]);
                            p.fail_arrangement = None;
                            p.arrangement = Some(0);
                        }
                        let observed = plugin.observe_audio_buses_bounded(&mut frame).unwrap();
                        assert_eq!(observed.input_arrangements.len(), 2);
                        assert_eq!(observed.output_arrangements.len(), 2);
                        assert!(observed
                            .input_arrangements
                            .iter()
                            .chain(observed.output_arrangements)
                            .all(|mask| mask.raw() == 0));
                        assert_eq!(probe.lock().unwrap().calls, full_calls);
                        probe.lock().unwrap().reset([2; 4]);
                        let owned = owned_arrangements(plugin).unwrap();
                        assert_eq!(observed.input_arrangements, owned.inputs);
                        assert_eq!(observed.output_arrangements, owned.outputs);
                        assert_eq!(probe.lock().unwrap().calls, full_calls);
                    }
                }
            }
        }
    }
    plugin.processing_resource_bounds = None;
}

pub(super) fn bounds(plugin: &mut PluginImpl) {
    let probe = attach(plugin, [32; 4]);
    let mut frame = AudioBusQueryFrame::default();
    let observed = plugin.observe_audio_buses_bounded(&mut frame).unwrap();
    assert_eq!(
        (
            observed.native_inputs.len(),
            observed.native_outputs.len(),
            observed.layout_inputs.len(),
            observed.layout_outputs.len(),
            observed.input_arrangements.len(),
            observed.output_arrangements.len()
        ),
        (32, 32, 32, 32, 32, 32)
    );
    assert_eq!(probe.lock().unwrap().calls.len(), 4 + 4 * 32);
    for bad in [-1, 33] {
        for position in 0..4 {
            let mut counts = [1; 4];
            counts[position] = bad;
            probe.lock().unwrap().reset(counts);
            let result = plugin.observe_audio_buses_bounded(&mut frame);
            if position < 2 {
                assert_metadata_error(result);
            } else {
                assert!(matches!(result, Err(Stage::Arrangements(_))));
            }
            let p = probe.lock().unwrap();
            assert_eq!(p.count_index, position + 1);
            assert_eq!(
                p.calls.last(),
                Some(&Call::Count(
                    if position % 2 == 0 {
                        kInput as i32
                    } else {
                        kOutput as i32
                    },
                    bad
                ))
            );
        }
    }
    // Cached extents are checked independently, before copying/iteration and
    // before the fresh native census. Generic owned restore still supports 33.
    for input in [true, false] {
        let data = plugin.process_data.as_mut().unwrap();
        let buses = if input {
            &mut data.input_bus_buffers
        } else {
            &mut data.output_bus_buffers
        };
        buses.push(unsafe { std::mem::zeroed() });
        probe.lock().unwrap().reset([1; 4]);
        let result = plugin.observe_audio_buses_bounded(&mut frame);
        let data = plugin.process_data.as_mut().unwrap();
        if input {
            data.input_bus_buffers.pop();
        } else {
            data.output_bus_buffers.pop();
        }
        assert!(matches!(result, Err(Stage::Layout(_))));
        assert_eq!(probe.lock().unwrap().count_index, 2);
    }
    let data = plugin.process_data.as_mut().unwrap();
    let old_inputs = std::mem::take(&mut data.input_bus_buffers);
    let old_outputs = std::mem::take(&mut data.output_bus_buffers);
    probe.lock().unwrap().reset([0; 4]);
    let empty = plugin.observe_audio_buses_bounded(&mut frame).unwrap();
    assert!(
        empty.native_inputs.is_empty()
            && empty.native_outputs.is_empty()
            && empty.layout_inputs.is_empty()
            && empty.layout_outputs.is_empty()
            && empty.input_arrangements.is_empty()
            && empty.output_arrangements.is_empty()
    );
    let data = plugin.process_data.as_mut().unwrap();
    data.input_bus_buffers = old_inputs;
    data.output_bus_buffers = old_outputs;
    assert_eq!(probe.lock().unwrap().count_index, 4);
    probe.lock().unwrap().reset([1, 1, 2, 0]);
    let observed = plugin.observe_audio_buses_bounded(&mut frame).unwrap();
    assert_eq!(
        (
            observed.native_inputs.len(),
            observed.input_arrangements.len(),
            observed.native_outputs.len(),
            observed.output_arrangements.len()
        ),
        (1, 2, 1, 0)
    );
    assert_eq!(
        probe.lock().unwrap().count_index,
        4,
        "a valid count drift is evidence, not an early shape error"
    );
}

pub(super) fn errors_and_cached_facts(plugin: &mut PluginImpl) {
    let probe = attach(plugin, [1; 4]);
    let mut frame = AudioBusQueryFrame::default();
    {
        let mut p = probe.lock().unwrap();
        p.role = Some(BusTypes_::kAux as i32); // legal native role, wrong host main shape
        p.fail_info = Some((kOutput as i32, 0));
    }
    let old = plugin.native_audio_bus_info().unwrap_err().to_string();
    probe.lock().unwrap().reset([1; 4]);
    let Err(Stage::Metadata(error)) = plugin.observe_audio_buses_bounded(&mut frame) else {
        panic!("later native Info error must precede host shape comparison")
    };
    assert_eq!(error.to_string(), old);
    assert_eq!(
        probe.lock().unwrap().calls.last(),
        Some(&Call::Info(kOutput as i32, 0))
    );
    {
        let mut p = probe.lock().unwrap();
        p.reset([1; 4]);
        p.fail_info = None;
    }
    let data = plugin.process_data.take();
    let result = plugin.observe_audio_buses_bounded(&mut frame);
    plugin.process_data = data;
    assert!(matches!(result, Err(Stage::Layout(_))));
    assert_eq!(probe.lock().unwrap().count_index, 2);
    {
        let mut p = probe.lock().unwrap();
        p.reset([1, 1, 33, 1]);
    }
    assert!(matches!(
        plugin.observe_audio_buses_bounded(&mut frame),
        Err(Stage::Arrangements(_))
    ));
    assert_eq!(probe.lock().unwrap().count_index, 3);

    // An unknown SDK role remains an immediate metadata error, before output count.
    {
        let mut p = probe.lock().unwrap();
        p.reset([1; 4]);
        p.role = Some(99);
    }
    assert_metadata_error(plugin.observe_audio_buses_bounded(&mut frame));
    assert_eq!(probe.lock().unwrap().count_index, 1);
    probe.lock().unwrap().role = None;
    probe.lock().unwrap().reset([1; 4]);
    let old_channels = plugin.process_data.as_mut().unwrap().input_bus_buffers[0].numChannels;
    plugin.process_data.as_mut().unwrap().input_bus_buffers[0].numChannels = -1;
    let old_ack = std::mem::take(&mut plugin.bus_activation.audio_inputs);
    let observed = plugin.observe_audio_buses_bounded(&mut frame).unwrap();
    assert_eq!(observed.native_inputs[0].role, AudioBusRole::Main);
    assert_eq!(
        observed.layout_inputs,
        [AudioBusConfig {
            channel_count: 0,
            active: false
        }]
    );
    assert!(observed.layout_outputs[0].active);
    assert_eq!(probe.lock().unwrap().count_index, 4);
    plugin.process_data.as_mut().unwrap().input_bus_buffers[0].numChannels = old_channels;
    plugin.bus_activation.audio_inputs = old_ack;
}

pub(super) fn owner_and_allocations(plugin: &mut PluginImpl) {
    let probe = attach(plugin, [1; 4]);
    let mut frame = AudioBusQueryFrame::default();
    plugin.is_processing = true;
    let processing = plugin.observe_audio_buses_bounded(&mut frame);
    plugin.is_processing = false;
    assert_metadata_error(processing);
    assert!(probe.lock().unwrap().calls.is_empty());
    let owner = plugin.control_thread;
    plugin.control_thread = std::thread::spawn(|| std::thread::current().id())
        .join()
        .unwrap();
    let foreign = plugin.observe_audio_buses_bounded(&mut frame);
    plugin.control_thread = owner;
    assert_metadata_error(foreign);
    assert!(probe.lock().unwrap().calls.is_empty());
    for fresh_inputs in [1, 2] {
        probe.lock().unwrap().reset([1, 1, fresh_inputs, 1]);
        let (extents, operations) =
            super::super::data_exchange::measure_host_allocator_operations_for_test(|| {
                let observed = plugin.observe_audio_buses_bounded(&mut frame).unwrap();
                (
                    observed.native_inputs.len(),
                    observed.input_arrangements.len(),
                )
            });
        assert_eq!(extents, (1, fresh_inputs as usize));
        assert_eq!(operations, 0, "successful bounded metadata and legal extent mismatch retain no metadata heap containers");
        assert_eq!(probe.lock().unwrap().count_index, 4);
    }
}
