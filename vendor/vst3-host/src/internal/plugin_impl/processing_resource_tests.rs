//! Finite known-buffer-family controls using the original real COM owner.
//! Allocation traces exclude plugin/COM/Arc-wrapper qualification and Root grants.
use super::super::data_exchange::{
    measure_host_allocator_operations_for_test as measure,
    visit_host_allocator_events_for_test as trace,
};
use super::*;
use crate::audio::processing_resources as buffers;
use crate::audio::{AudioBusConfig, AudioBusLayout, ProcessingResourceBounds as Bounds};
use crate::error::ProcessingResourceError as Refusal;

fn resource<T: std::fmt::Debug>(result: Result<T>, reason: Refusal) {
    assert!(matches!(result, Err(Error::ProcessingResources(actual)) if actual==reason));
}
fn bus(channels: i32) -> AudioBusBuffers {
    let mut bus: AudioBusBuffers = unsafe { std::mem::zeroed() };
    bus.numChannels = channels;
    bus
}
struct Owned(Vec<(usize, usize)>);
impl crate::host_storage::HostStorageVisitor for Owned {
    fn visit(
        &mut self,
        value: crate::host_storage::HostStorageRef<'_>,
    ) -> std::result::Result<(), crate::host_storage::HostStorageError> {
        if value.layout().size() != 0 {
            self.0.push((value.address(), value.layout().size()));
        }
        Ok(())
    }
}
fn process_allocations(data: &HostProcessData) -> Vec<(usize, usize)> {
    use crate::host_storage::{HostStorageRef as Storage, HostStorageVisitor};
    let mut result = Owned(Vec::new());
    result.0.push((
        std::ptr::from_ref(data) as usize,
        std::alloc::Layout::new::<HostProcessData>().size(),
    ));
    result
        .visit(Storage::vector(&data.input_bus_buffers).unwrap())
        .unwrap();
    result
        .visit(Storage::vector(&data.output_bus_buffers).unwrap())
        .unwrap();
    match &data.sample_buffers {
        HostSampleBuffers::F32(value) => value.visit_owned_storage(&mut result).unwrap(),
        HostSampleBuffers::F64(value) => value.visit_owned_storage(&mut result).unwrap(),
    }
    result.0
}
pub(super) fn geometry() {
    let (bounds, operations) = measure(|| Bounds::new(2, 2, 8, 4).unwrap());
    assert_eq!(operations, 0);
    let bytes = bounds.host_buffer_peak_bytes().unwrap();
    assert!(bytes >= 2 * std::alloc::Layout::new::<HostProcessData>().size());
    let (result, operations) = measure(|| {
        Bounds::new(
            i32::MAX as usize,
            i32::MAX as usize,
            usize::MAX,
            i32::MAX as usize,
        )
    });
    resource(result, Refusal::ExtentOverflow);
    assert_eq!(operations, 0);
    resource(Bounds::new(1, 2, 4, 0), Refusal::InvalidGeometry);
    resource(Bounds::new(usize::MAX, 2, 4, 4), Refusal::InvalidGeometry);
    let empty = Bounds::new(0, 0, 0, 4).unwrap();
    assert!(
        empty.host_buffer_peak_bytes().unwrap()
            >= 2 * std::mem::size_of::<crate::audio::ChannelLevel>(),
        "zero-output meter fallback remains covered"
    );
    let mut layout = AudioBusLayout {
        inputs: vec![AudioBusConfig {
            channel_count: 2,
            active: false,
        }],
        outputs: vec![AudioBusConfig {
            channel_count: 2,
            active: true,
        }],
    };
    let (result, operations) = measure(|| buffers::caller_buffers(bounds, &layout, 5, 48000.0));
    resource(result, Refusal::BlockFrames);
    assert_eq!(operations, 0);
    layout.inputs[0].channel_count = 3;
    let (result, operations) = measure(|| buffers::caller_buffers(bounds, &layout, 4, 48000.0));
    resource(result, Refusal::ChannelCount);
    assert_eq!(operations, 0);
    layout.inputs[0].channel_count = 2;
    let total = Bounds::new(2, 2, 3, 4).unwrap();
    let (result, operations) = measure(|| buffers::caller_buffers(total, &layout, 4, 48000.0));
    resource(result, Refusal::TotalChannels);
    assert_eq!(operations, 0);
    let (result, operations) = measure(|| {
        build_typed_sample_buffers_bounded::<f64>(bounds, 4, &[bus(3)], &[true], &[], &[])
    });
    assert!(matches!(
        result,
        Err(Error::ProcessingResources(Refusal::ChannelCount))
    ));
    assert_eq!(operations, 0);
    let (result, operations) = measure(|| buffers::samples::<f64>(0, 4));
    assert!(result.unwrap().is_empty());
    assert_eq!(
        operations, 0,
        "zero channels do not allocate a template block"
    );
}
pub(super) fn rebuild(plugin: &mut PluginImpl) {
    let bounds = Bounds::new(1, 2, 4, 4).unwrap();
    plugin.processing_resource_bounds = Some(bounds);
    // Establish a bounded old generation before measuring a bounded replacement;
    // adopting the fixture's earlier generic capacities is not the production path.
    plugin.setup_processing().unwrap();
    plugin.bus_activation.audio_inputs[0] = false;
    plugin.sample_size = ProcessingSampleSize::F64;
    let old = process_allocations(plugin.process_data.as_ref().unwrap());
    let old_box = old[0].0;
    let old_ack = [
        plugin.bus_activation.audio_inputs.as_ptr() as usize,
        plugin.bus_activation.audio_outputs.as_ptr() as usize,
    ];
    let mut events = Vec::with_capacity(64);
    let result = trace(
        || plugin.setup_processing(),
        |op, address, bytes, _| events.push((op, address, bytes)),
    );
    result.unwrap();
    let new = process_allocations(plugin.process_data.as_ref().unwrap());
    let new_box = new[0].0;
    assert_ne!(old_box, new_box);
    let position = |op, address| {
        events
            .iter()
            .position(|event| event.0 == op && event.1 == address)
            .expect("actual allocation lifecycle event")
    };
    let created = position(1, new_box);
    let retired = position(2, old_box);
    assert!(
        created < retired,
        "old ProcessData stays owned while the replacement exists"
    );
    for &(address, _) in &old {
        assert!(created < position(2, address));
    }
    let new_ack = [
        plugin.bus_activation.audio_inputs.as_ptr() as usize,
        plugin.bus_activation.audio_outputs.as_ptr() as usize,
    ];
    for address in old_ack {
        assert!(position(1, new_ack[0]) < position(2, address));
        assert!(position(1, new_ack[1]) < position(2, address));
    }
    assert!(
        old.iter().chain(&new).map(|value| value.1).sum::<usize>()
            <= bounds.host_buffer_peak_bytes().unwrap()
    );
    let HostSampleBuffers::F64(samples) = &plugin.process_data.as_ref().unwrap().sample_buffers
    else {
        panic!("actual f64 storage")
    };
    assert!(samples.inputs.is_empty());
    assert_eq!(samples.input_channel_ptrs.0[0].len(), 2);
    assert!(samples.input_channel_ptrs.0[0].iter().all(|p| p.is_null()));
    assert_eq!(samples.outputs.len(), 2);
    for channel in &samples.outputs {
        assert_eq!(channel.as_slice(), [0.0; 4]);
        assert_eq!(channel.capacity(), 4);
    }
    // Q's native COM calls do not allocate; the four bounded vectors are real
    // owned/raw inventories and must coexist before their original Drop.
    events.clear();
    trace(
        || unsafe { plugin.negotiate_default_bus_arrangements() },
        |op, address, bytes, _| events.push((op, address, bytes)),
    )
    .unwrap();
    let mut live = 0;
    let mut peak = 0;
    for (op, _, bytes) in events {
        match op {
            1 => {
                live += bytes;
                peak = peak.max(live);
            }
            2 => live -= bytes,
            _ => panic!("bounded Q must not grow/reallocate"),
        }
    }
    assert_eq!(peak, 4 * std::mem::size_of::<u64>());
    assert_eq!(live, 0);
}
pub(super) fn caller_and_meter(mut plugin: PluginImpl) {
    let bounds = Bounds::new(1, 2, 4, 4).unwrap();
    plugin.processing_resource_bounds = Some(bounds);
    assert_eq!(plugin.prepared_meter_channels(), Some(Some(2)));
    let old = std::ptr::from_ref(plugin.process_data.as_ref().unwrap().as_ref()) as usize;
    resource(plugin.reconfigure(48000.0, 5), Refusal::BlockFrames);
    assert_eq!(
        std::ptr::from_ref(plugin.process_data.as_ref().unwrap().as_ref()) as usize,
        old
    );
    let mut wrapped = wrap(plugin, bounds);
    let (meter, capacity) = {
        let levels = wrapped.audio_levels.lock().unwrap();
        assert!(levels.channels.is_empty());
        (levels.channels.as_ptr(), levels.channels.capacity())
    };
    for _ in 0..2 {
        wrapped
            .set_process_mode(crate::plugin::ProcessMode::Offline)
            .unwrap();
        let levels = wrapped.audio_levels.lock().unwrap();
        assert_eq!(levels.channels.len(), 2);
        assert_eq!(
            (levels.channels.as_ptr(), levels.channels.capacity()),
            (meter, capacity),
            "control success reuses the one pre-admitted meter Vec"
        );
    }
    let (result, operations) = measure(|| wrapped.create_bus_audio_buffers(5));
    resource(result, Refusal::BlockFrames);
    assert_eq!(operations, 0);
    let caller = wrapped.create_bus_audio_buffers(4).unwrap();
    assert_eq!((caller.inputs.len(), caller.outputs.len()), (1, 1));
    for bus in caller.inputs.iter().chain(&caller.outputs) {
        assert_eq!(bus.channels.len(), 2);
        for channel in &bus.channels {
            assert_eq!(channel.as_slice(), [0.0; 4]);
            assert_eq!(channel.capacity(), 4);
        }
    }
}

pub(super) fn wrap(plugin: PluginImpl, bounds: Bounds) -> crate::plugin::Plugin {
    let info = plugin.info.clone();
    crate::plugin::Plugin {
        info,
        compatibility: Vec::new(),
        sample_rate: 48000.0,
        block_size: 4,
        audio_levels: Arc::new(Mutex::new(buffers::reserved_levels(bounds).unwrap())),
        parameter_change_callback: None,
        audio_callback: None,
        internal: Some(Box::new(plugin)),
    }
}
