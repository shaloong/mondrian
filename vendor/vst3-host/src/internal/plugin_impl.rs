//! Internal VST3 plugin implementation

use crate::{
    audio::{AudioBuffers, AudioBusBuffer, AudioBusConfig, AudioBusLayout, BusAudioBuffers},
    error::{Error, MusicalTimeError, NativeEventError, Result},
    midi::{MidiChannel, MidiEvent, PluginEvent},
    parameters::{Parameter, ParameterChange},
    plugin::{
        decode_state_snapshot, encode_state_snapshot, PluginInfo, PluginInternal, StateContext,
        StateSnapshot,
    },
};
use crossbeam_queue::ArrayQueue;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use vst3::Steinberg::Vst::BusDirections_::*;
use vst3::Steinberg::Vst::Event_::EventTypes_::*;
use vst3::Steinberg::Vst::MediaTypes_::*;
use vst3::Steinberg::{
    IPlugView, IPlugViewContentScaleSupport, IPlugViewContentScaleSupportTrait, IPlugViewTrait,
};
use vst3::{ComPtr, ComWrapper, Interface, Steinberg::Vst::*, Steinberg::*};

#[cfg(target_os = "linux")]
use super::com_implementations::RunLoopRegistry;
use super::{
    com_implementations::{
        create_event_list, create_host_application_with_interfaces, create_host_plug_frame,
        create_memory_stream_from_with_metadata, create_memory_stream_with_metadata,
        create_state_restore_stream, ComponentHandler, ConnectionPair, HostApplication,
        HostEventList, HostPlugFrame, ParameterChanges, StreamStateType, MAX_EDITOR_FEEDBACK,
        MAX_QUEUED_EVENTS,
    },
    editor_attachment::EditorAttachment,
    module_loader::{load_module, VstModule},
};

/// Cap on the buffered output MIDI a plugin emits, so a host that never polls can't grow it
/// forever. The buffer is pre-reserved to this size so steady-state pushes never reallocate.
const MAX_OUTPUT_MIDI: usize = 4096;

/// Cap on simultaneously tracked `note_on` ids (see `PluginImpl::active_notes`). Far above any
/// real polyphony, and bounds a caller that starts notes it never releases.
const MAX_TRACKED_NOTES: usize = 1024;

/// Cap on parameter changes queued for the next block (see `PluginImpl::pending_param_changes`).
/// The queue's only drain is `process()`, which returns early while the plugin isn't processing —
/// so a host whose stream keeps running after `stop_processing`, or one automating parameters
/// before playback starts, would otherwise grow it forever and then flood a single block with the
/// whole backlog. The sibling of `MAX_QUEUED_EVENTS` on the MIDI side: pre-reserved so the audio
/// path never reallocates, and changes past the cap are dropped (and counted).
const MAX_PENDING_PARAM_CHANGES: usize = 4096;
const MIDI_CONTROLLER_COUNT: usize = ControllerNumbers_::kCtrlProgramChange as usize + 1;
const MIDI_CHANNEL_COUNT: usize = 16;
const MAX_OUTPUT_PARAMETER_FEEDBACK: usize = 4096;

/// Cap on the event-input buses consulted when building the MIDI-mapping table. That table is
/// dense — `buses × MIDI_CHANNEL_COUNT × MIDI_CONTROLLER_COUNT` entries — so the bus count a
/// plugin reports is a multiplier on a host allocation and on a burst of controller COM calls.
/// One event input is the norm; 32 is far past any real plugin, and mirrors the `MAX_*` caps on
/// every other plugin-sized host buffer.
const MAX_MIDI_MAPPING_BUSES: usize = 32;

/// Cap on parameter values waiting to be mirrored into `IEditController` (see
/// `PluginImpl::deferred_controller_sync`). Bounded so a host that never services the plugin
/// cannot grow it; when full the oldest value is dropped, because the newest value for a
/// parameter is the one worth applying.
const MAX_DEFERRED_CONTROLLER_SYNC: usize = 4096;

#[derive(Default)]
struct MidiMappingCache {
    buses: usize,
    assignments: Vec<Option<u32>>,
}

/// Which controller-derived caches a `restartComponent` or host notification left stale.
///
/// Rebuilding either one is a burst of `IEditController` calls, which belong to the main-thread
/// domain — but notifications arrive on whichever thread the plugin chose. So an off-control
/// thread only records what went stale here, and the next control-thread entry point rebuilds.
#[derive(Default, Clone, Copy)]
struct DirtyCaches {
    midi_mapping: bool,
    program_change: bool,
}

impl MidiMappingCache {
    fn index(&self, bus: i32, channel: i16, controller: u16) -> Option<usize> {
        let bus = usize::try_from(bus).ok()?;
        let channel = usize::try_from(channel).ok()?;
        let controller = usize::from(controller);
        (bus < self.buses && channel < MIDI_CHANNEL_COUNT && controller < MIDI_CONTROLLER_COUNT)
            .then_some((bus * MIDI_CHANNEL_COUNT + channel) * MIDI_CONTROLLER_COUNT + controller)
    }

    fn get(&self, bus: i32, channel: i16, controller: u16) -> Option<u32> {
        self.index(bus, channel, controller)
            .and_then(|index| self.assignments[index])
    }
}

/// How many event-input buses the MIDI-mapping table covers, given the count a plugin reports.
/// Negative counts read as none, and the total is capped at [`MAX_MIDI_MAPPING_BUSES`].
fn midi_mapping_bus_count(reported: i32) -> usize {
    (reported.max(0) as usize).min(MAX_MIDI_MAPPING_BUSES)
}

#[derive(Clone, Copy)]
struct ProgramChangeMapping {
    unit_id: i32,
    param_id: u32,
    program_count: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessingSampleSize {
    F32,
    F64,
}

impl ProcessingSampleSize {
    fn symbolic(self) -> i32 {
        match self {
            Self::F32 => SymbolicSampleSizes_::kSample32 as i32,
            Self::F64 => SymbolicSampleSizes_::kSample64 as i32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EventBusActivation {
    role: crate::audio::NativeEventBusRole,
    channel_count: usize,
    active: bool,
}

#[derive(Default)]
struct BusActivationState {
    audio_inputs: Vec<bool>,
    audio_outputs: Vec<bool>,
    event_inputs: Vec<EventBusActivation>,
    event_outputs: Vec<EventBusActivation>,
    event_readiness: Option<std::result::Result<(), NativeEventError>>,
}

/// Internal plugin implementation that handles all VST3 COM interactions
pub struct PluginImpl {
    // Core VST3 interfaces
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: Option<ComPtr<IEditController>>,
    /// True when the component and controller are the same object (single-component
    /// plugin). Then `IComponent::setState` already restores the controller, and calling
    /// `setComponentState` on top of it would double-apply and corrupt parameters.
    single_component: bool,

    // Plugin metadata
    pub(crate) info: PluginInfo,
    pub(crate) compatibility: Vec<crate::discovery::ClassCompatibility>,

    // Processing state
    is_active: bool,
    is_processing: bool,
    sample_rate: f64,
    block_size: usize,
    /// The configuration the plugin's last successful `setupProcessing` carried, so
    /// `start_processing` can tell a re-start (nothing to do) from a configuration change
    /// (which has to be applied while the component is inactive). `None` until the first setup.
    applied_setup: Option<AppliedSetup>,
    /// Transport tempo (BPM) advertised in the host `ProcessContext`.
    tempo: f64,
    /// Time signature numerator advertised in the host `ProcessContext`.
    time_sig_numerator: i32,
    /// Time signature denominator advertised in the host `ProcessContext`.
    time_sig_denominator: i32,
    /// Whether the transport is playing (the `kPlaying` flag in `ProcessContext.state`).
    playing: bool,
    /// Real-time vs offline processing, baked into `ProcessSetup`/`process_data` at setup.
    process_mode: crate::plugin::ProcessMode,
    /// Optional VST3 3.7 declaration of exactly which ProcessContext fields the processor reads.
    /// `None` is the safe legacy path for older processors and advertises the historical context.
    process_context_requirements: Option<u32>,
    /// Current IPrefetchableSupport value, or `None` for pre-3.6 processors.
    prefetchable_support: Option<u32>,
    sample_size: ProcessingSampleSize,
    bus_activation: BusActivationState,
    /// Monotonic allocator for per-voice note ids (note_on); 0/-1 reserved for "unset".
    next_note_id: i32,
    // `(note_id, channel, pitch)` for notes started via `note_on`, so `note_off` can address the
    // release by pitch as well as by id. Pre-reserved and capped, so tracking never allocates on
    // the audio thread and a caller that never releases its notes can't grow it without bound.
    active_notes: Vec<(i32, i16, i16)>,
    /// Counts ordinary (noteId = -1) note-ons by channel/pitch. Fixed-size so MIDI tracking
    /// never allocates on the device callback.
    ordinary_note_counts: [u16; MIDI_CHANNEL_COUNT * 128],

    // Controller-derived routing tables. Both are built outside process() and only read from
    // the callback; `restartComponent` merely records invalidation flags atomically.
    midi_mapping_cache: MidiMappingCache,
    program_change_cache: Vec<ProgramChangeMapping>,
    dirty_caches: DirtyCaches,
    unit_cache: Mutex<Option<Vec<crate::plugin::PluginUnit>>>,

    // Host data structures
    process_data: Option<Box<HostProcessData>>,
    component_handler: Option<ComWrapper<ComponentHandler>>,
    connection: Option<ConnectionPair>,

    // Parameter changes queued by the host (set_parameter / automation) to be fed into the
    // processor's input parameter queue at the start of the next process() block. Serialized
    // with process() by the caller's &mut access, so a plain Vec (no lock) is sufficient.
    // Pre-reserved to MAX_PENDING_PARAM_CHANGES and capped there — see the constant.
    pending_param_changes: Vec<ParameterChange>,
    // How many parameter changes have been dropped because the queue was full. Reported in the
    // warning so a host sees a running total rather than one line per lost change.
    dropped_param_changes: u64,

    // Parameter edits the plugin's *own editor* reported via `IComponentHandler::performEdit`,
    // after `process()` has routed them into the processor's input queue. Drained by the host
    // via `get_parameter_changes()` to update its UI. Separate from the raw performEdit sink
    // (`component_handler.parameter_changes`) so feeding the DSP and updating the display are
    // not two consumers racing to drain the same buffer.
    gui_param_changes_for_host: Arc<Mutex<Vec<(u32, f64)>>>,
    // Processor-originated output parameter points. Bounded and lock-free: process() pushes,
    // the host/UI drains through get_parameter_changes().
    output_param_feedback: Arc<ArrayQueue<(u32, f64)>>,
    // Parameter values that reached the processor queue from a thread that must not call
    // `IEditController` (the audio callback, via the playback handles). Bounded, lock-free and
    // drop-oldest; the control-thread service paths drain it into setParamNormalized so the
    // plugin's own editor, get_parameter/format_parameter and saved state track the DSP.
    deferred_controller_sync: ArrayQueue<(u32, f64)>,

    // Event handling
    input_events: ComWrapper<HostEventList>,
    output_events: ComWrapper<HostEventList>,
    // Holds a block's queued input events while `process` distributes them over the block's
    // chunks: each chunk is staged into `input_events` with only the events that fall inside it.
    // Pre-reserved to MAX_QUEUED_EVENTS (the input list's own cap), so splitting a block never
    // allocates on the audio thread.
    chunk_events: Vec<Option<PluginEvent>>,
    // MIDI the plugin has emitted (captured from output_events after each process block,
    // converted to MidiEvent), buffered for the host to poll. A lock-free bounded queue so the
    // audio thread can push without locking and a UI thread can drain concurrently; when full
    // the oldest event is dropped (bounded memory if the host never polls).
    output_events_owned: Arc<ArrayQueue<PluginEvent>>,

    // Plugin view
    native_editor_owner: crate::plugin::NativeEditorAttachmentOwner,
    plugin_view: EditorAttachment,
    editor_scale_factor: f32,

    // Editor resize plumbing: the IPlugFrame handed to the plugin's view, and the slot it
    // writes requested sizes into (drained via take_editor_resize_request).
    plug_frame: ComWrapper<HostPlugFrame>,
    editor_resize: Arc<Mutex<Option<(i32, i32)>>>,
    // Linux IRunLoop registrations from the plugin's editor (fd handlers +
    // timers), serviced on the host UI thread via `service_run_loop`.
    #[cfg(target_os = "linux")]
    run_loop: Arc<Mutex<RunLoopRegistry>>,

    // VST3 module handle (kept alive). Declared after every plugin-side COM reference above so
    // those are released while the module's vtables still exist, and before `_host_app` below.
    _module: Box<dyn VstModule>,

    // Host application context passed to initialize() — kept alive for the plugin's lifetime
    // because the plugin may retain a reference to it. It has to outlive `_module`: the SDK's
    // `CPluginFactory::setHostContext` parks this pointer in a module-global (`gPluginContext`)
    // without an addRef, and module teardown — static destructors, `DeinitModule` — still reads
    // it. Rust drops fields in declaration order, so this one is declared last.
    _host_app: ComWrapper<HostApplication>,

    // Thread which initialized the component/controller. Lifecycle-sensitive restart handling
    // and controller cache refresh must return to this thread.
    control_thread: ThreadId,
}

// Processing data structure
struct HostProcessData {
    process_data: ProcessData,
    sample_buffers: HostSampleBuffers,
    input_bus_buffers: Vec<AudioBusBuffers>,
    output_bus_buffers: Vec<AudioBusBuffers>,
    process_context: ProcessContext,
    process_context_requirements: Option<u32>,
    transport_tempo: f64,
    musical_reference: Option<crate::audio::ProcessMusicalReference>,
    input_param_changes: ComWrapper<ParameterChanges>,
    output_param_changes: ComWrapper<ParameterChanges>,
    // Preallocated channel-pointer arrays, built once in prepare_buffers. The audio buffers'
    // addresses are stable after allocation, so process() reuses these instead of rebuilding
    // them every block — keeping the steady-state audio path allocation-free.
}

impl<T> TypedSampleBuffers<T> {
    fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> std::result::Result<(), crate::host_storage::HostStorageError> {
        use crate::host_storage::HostStorageRef;
        visitor.visit(HostStorageRef::vector(&self.inputs)?)?;
        for channel in &self.inputs {
            visitor.visit(HostStorageRef::vector(channel)?)?;
        }
        visitor.visit(HostStorageRef::vector(&self.outputs)?)?;
        for channel in &self.outputs {
            visitor.visit(HostStorageRef::vector(channel)?)?;
        }
        visitor.visit(HostStorageRef::vector(&self.input_channel_ptrs.0)?)?;
        for bus in &self.input_channel_ptrs.0 {
            visitor.visit(HostStorageRef::vector(bus)?)?;
        }
        visitor.visit(HostStorageRef::vector(&self.output_channel_ptrs.0)?)?;
        for bus in &self.output_channel_ptrs.0 {
            visitor.visit(HostStorageRef::vector(bus)?)?;
        }
        Ok(())
    }
}

impl HostProcessData {
    fn visit_owned_storage(
        &self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> std::result::Result<(), crate::host_storage::HostStorageError> {
        use crate::host_storage::HostStorageRef;
        visitor.visit(HostStorageRef::vector(&self.input_bus_buffers)?)?;
        visitor.visit(HostStorageRef::vector(&self.output_bus_buffers)?)?;
        match &self.sample_buffers {
            HostSampleBuffers::F32(value) => value.visit_owned_storage(visitor)?,
            HostSampleBuffers::F64(value) => value.visit_owned_storage(visitor)?,
        }
        visitor.visit(HostStorageRef::com(&self.input_param_changes))?;
        self.input_param_changes.visit_owned_storage(visitor)?;
        visitor.visit(HostStorageRef::com(&self.output_param_changes))?;
        self.output_param_changes.visit_owned_storage(visitor)
    }
}

enum CallerAudioBuffers<'a> {
    Flat(&'a mut AudioBuffers),
    Buses(&'a mut BusAudioBuffers),
}

impl CallerAudioBuffers<'_> {
    fn frame_count(&self, fallback: usize) -> usize {
        match self {
            Self::Flat(buffers) => buffers
                .outputs
                .iter()
                .chain(&buffers.inputs)
                .map(Vec::len)
                .next()
                .unwrap_or(fallback),
            Self::Buses(buffers) => buffers
                .outputs
                .iter()
                .chain(&buffers.inputs)
                .flat_map(|bus| &bus.channels)
                .map(Vec::len)
                .next()
                .unwrap_or(buffers.block_size),
        }
    }
}

struct TypedSampleBuffers<T> {
    inputs: Vec<Vec<T>>,
    outputs: Vec<Vec<T>>,
    input_channel_ptrs: SendChannelPtrs<T>,
    output_channel_ptrs: SendChannelPtrs<T>,
}

enum HostSampleBuffers {
    F32(TypedSampleBuffers<f32>),
    F64(TypedSampleBuffers<f64>),
}

/// The processing configuration handed to the plugin by `setupProcessing`, cached so a
/// re-start can tell "nothing changed" (skip setup, the plugin still has it) from "the host
/// reconfigured me" (setup must be re-run, and only while the component is inactive).
#[derive(Debug, Clone, Copy, PartialEq)]
struct AppliedSetup {
    sample_rate: f64,
    block_size: usize,
    /// The VST3 `ProcessModes` value, not the safe enum — this is compared against what was
    /// actually written into `ProcessSetup`.
    process_mode: i32,
    symbolic_sample_size: i32,
}

/// Per-bus channel pointers into the (audio-thread-owned) audio buffers.
///
/// `Send` because the pointers are only ever dereferenced on the one thread that owns the
/// `HostProcessData` (and thus the buffers they point into); the `Plugin` is moved to that
/// thread as a unit. The raw pointers never escape to another thread.
struct SendChannelPtrs<T>(Vec<Vec<*mut T>>);
unsafe impl<T: Send> Send for SendChannelPtrs<T> {}

#[derive(Clone, Copy)]
struct SavedProcessAudioIo {
    num_inputs: i32,
    num_outputs: i32,
    inputs: *mut AudioBusBuffers,
    outputs: *mut AudioBusBuffers,
}

fn hide_audio_io_for_zero_sample(
    process_data: &mut ProcessData,
    frames: usize,
) -> Option<SavedProcessAudioIo> {
    if frames != 0 {
        return None;
    }
    let saved = SavedProcessAudioIo {
        num_inputs: process_data.numInputs,
        num_outputs: process_data.numOutputs,
        inputs: process_data.inputs,
        outputs: process_data.outputs,
    };
    process_data.numInputs = 0;
    process_data.numOutputs = 0;
    process_data.inputs = ptr::null_mut();
    process_data.outputs = ptr::null_mut();
    Some(saved)
}

fn restore_process_audio_io(process_data: &mut ProcessData, saved: Option<SavedProcessAudioIo>) {
    if let Some(saved) = saved {
        process_data.numInputs = saved.num_inputs;
        process_data.numOutputs = saved.num_outputs;
        process_data.inputs = saved.inputs;
        process_data.outputs = saved.outputs;
    }
}

fn build_typed_sample_buffers<T: Default + Clone>(
    block_size: usize,
    input_buses: &[AudioBusBuffers],
    input_active: &[bool],
    output_buses: &[AudioBusBuffers],
    output_active: &[bool],
) -> TypedSampleBuffers<T> {
    fn side<T: Default + Clone>(
        block_size: usize,
        buses: &[AudioBusBuffers],
        active: &[bool],
    ) -> (Vec<Vec<T>>, SendChannelPtrs<T>) {
        let channel_count: usize = buses
            .iter()
            .zip(active.iter().copied().chain(std::iter::repeat(false)))
            .filter(|(_, active)| *active)
            .map(|(bus, _)| bus.numChannels.max(0) as usize)
            .sum();
        let mut buffers = vec![vec![T::default(); block_size]; channel_count];
        let mut next_channel = 0usize;
        let mut pointers = Vec::with_capacity(buses.len());
        for (index, bus) in buses.iter().enumerate() {
            let channels = bus.numChannels.max(0) as usize;
            let mut bus_pointers = Vec::with_capacity(channels);
            if active.get(index).copied().unwrap_or(false) {
                for _ in 0..channels {
                    bus_pointers.push(buffers[next_channel].as_mut_ptr());
                    next_channel += 1;
                }
            } else {
                // VST3 requires the host to supply the channel-buffer *array* for every bus
                // whether or not the bus is active; only the sample addresses inside it may be
                // null while a bus is inactive. `numChannels` stays as reported, so a plugin
                // that walks the array sees the shape it expects.
                bus_pointers.resize(channels, ptr::null_mut());
            }
            pointers.push(bus_pointers);
        }
        (buffers, SendChannelPtrs(pointers))
    }

    let (inputs, input_channel_ptrs) = side(block_size, input_buses, input_active);
    let (outputs, output_channel_ptrs) = side(block_size, output_buses, output_active);
    TypedSampleBuffers {
        inputs,
        outputs,
        input_channel_ptrs,
        output_channel_ptrs,
    }
}

impl HostSampleBuffers {
    fn input_channel_count(&self) -> usize {
        match self {
            Self::F32(samples) => samples.inputs.len(),
            Self::F64(samples) => samples.inputs.len(),
        }
    }

    fn output_channel_count(&self) -> usize {
        match self {
            Self::F32(samples) => samples.outputs.len(),
            Self::F64(samples) => samples.outputs.len(),
        }
    }

    fn copy_inputs_from(&mut self, inputs: &[Vec<f32>], frame_offset: usize, frames: usize) {
        match self {
            Self::F32(samples) => {
                for (index, destination) in samples.inputs.iter_mut().enumerate() {
                    let source = inputs.get(index);
                    let start = source
                        .map(|source| frame_offset.min(source.len()))
                        .unwrap_or(0);
                    let count = source
                        .map(|source| {
                            frames
                                .min(destination.len())
                                .min(source.len().saturating_sub(start))
                        })
                        .unwrap_or(0);
                    if let Some(source) = source {
                        destination[..count].copy_from_slice(&source[start..start + count]);
                    }
                    let end = frames.min(destination.len());
                    destination[count..end].fill(0.0);
                }
            }
            Self::F64(samples) => {
                for (index, destination) in samples.inputs.iter_mut().enumerate() {
                    let source = inputs.get(index);
                    let start = source
                        .map(|source| frame_offset.min(source.len()))
                        .unwrap_or(0);
                    let count = source
                        .map(|source| {
                            frames
                                .min(destination.len())
                                .min(source.len().saturating_sub(start))
                        })
                        .unwrap_or(0);
                    if let Some(source) = source {
                        for (to, from) in destination[..count]
                            .iter_mut()
                            .zip(&source[start..start + count])
                        {
                            *to = f64::from(*from);
                        }
                    }
                    let end = frames.min(destination.len());
                    destination[count..end].fill(0.0);
                }
            }
        }
    }

    fn copy_inputs_from_buses(
        &mut self,
        inputs: &[AudioBusBuffer],
        frame_offset: usize,
        frames: usize,
        buses: &[AudioBusBuffers],
        active: &[bool],
    ) {
        match self {
            Self::F32(samples) => copy_bus_inputs_to(
                &mut samples.inputs,
                inputs,
                frame_offset,
                frames,
                buses,
                active,
                |sample| sample,
            ),
            Self::F64(samples) => copy_bus_inputs_to(
                &mut samples.inputs,
                inputs,
                frame_offset,
                frames,
                buses,
                active,
                f64::from,
            ),
        }
    }

    fn clear_outputs(&mut self) {
        match self {
            Self::F32(samples) => {
                for channel in &mut samples.outputs {
                    channel.fill(0.0);
                }
            }
            Self::F64(samples) => {
                for channel in &mut samples.outputs {
                    channel.fill(0.0);
                }
            }
        }
    }

    fn update_input_silence_flags(
        &self,
        buses: &mut [AudioBusBuffers],
        active: &[bool],
        frames: usize,
    ) {
        match self {
            Self::F32(samples) => {
                update_bus_silence_flags(&samples.inputs, buses, active, frames, false, |sample| {
                    sample.to_bits() == 0
                })
            }
            Self::F64(samples) => {
                update_bus_silence_flags(&samples.inputs, buses, active, frames, false, |sample| {
                    sample.to_bits() == 0
                })
            }
        }
    }

    fn update_output_silence_flags(
        &self,
        buses: &mut [AudioBusBuffers],
        active: &[bool],
        frames: usize,
    ) {
        match self {
            Self::F32(samples) => {
                update_bus_silence_flags(&samples.outputs, buses, active, frames, true, |sample| {
                    sample.to_bits() == 0
                })
            }
            Self::F64(samples) => {
                update_bus_silence_flags(&samples.outputs, buses, active, frames, true, |sample| {
                    sample.to_bits() == 0
                })
            }
        }
    }

    fn copy_outputs_to(
        &self,
        outputs: &mut [Vec<f32>],
        frame_offset: usize,
        frames: usize,
        buses: &[AudioBusBuffers],
        active: &[bool],
    ) {
        match self {
            Self::F32(samples) => {
                copy_bus_outputs_to(
                    &samples.outputs,
                    outputs,
                    frame_offset,
                    frames,
                    buses,
                    active,
                    |sample| sample,
                );
            }
            Self::F64(samples) => {
                copy_bus_outputs_to(
                    &samples.outputs,
                    outputs,
                    frame_offset,
                    frames,
                    buses,
                    active,
                    |sample| sample as f32,
                );
            }
        }
    }

    fn copy_outputs_to_buses(
        &self,
        outputs: &mut [AudioBusBuffer],
        frame_offset: usize,
        frames: usize,
        buses: &[AudioBusBuffers],
        active: &[bool],
    ) {
        match self {
            Self::F32(samples) => copy_bus_outputs_to_buses(
                &samples.outputs,
                outputs,
                frame_offset,
                frames,
                buses,
                active,
                |sample| sample,
            ),
            Self::F64(samples) => copy_bus_outputs_to_buses(
                &samples.outputs,
                outputs,
                frame_offset,
                frames,
                buses,
                active,
                |sample| sample as f32,
            ),
        }
    }
}

fn copy_bus_inputs_to<T: Copy + Default>(
    destinations: &mut [Vec<T>],
    inputs: &[AudioBusBuffer],
    frame_offset: usize,
    frames: usize,
    buses: &[AudioBusBuffers],
    active: &[bool],
    convert: impl Fn(f32) -> T,
) {
    let mut destination_index = 0usize;
    for (bus_index, bus) in buses.iter().enumerate() {
        if !active.get(bus_index).copied().unwrap_or(false) {
            continue;
        }
        let source_bus = inputs.get(bus_index);
        for channel in 0..bus.numChannels.max(0) as usize {
            let Some(destination) = destinations.get_mut(destination_index) else {
                return;
            };
            let source = source_bus.and_then(|bus| bus.channels.get(channel));
            let start = source
                .map(|source| frame_offset.min(source.len()))
                .unwrap_or(0);
            let count = source
                .map(|source| {
                    frames
                        .min(destination.len())
                        .min(source.len().saturating_sub(start))
                })
                .unwrap_or(0);
            if let Some(source) = source {
                for (to, from) in destination[..count]
                    .iter_mut()
                    .zip(&source[start..start + count])
                {
                    *to = convert(*from);
                }
            }
            let clear_end = frames.min(destination.len());
            destination[count..clear_end].fill(T::default());
            destination_index += 1;
        }
    }
}

fn copy_bus_outputs_to_buses<T: Copy>(
    samples: &[Vec<T>],
    outputs: &mut [AudioBusBuffer],
    frame_offset: usize,
    frames: usize,
    buses: &[AudioBusBuffers],
    active: &[bool],
    convert: impl Fn(T) -> f32,
) {
    let mut sample_index = 0usize;
    for (bus_index, bus) in buses.iter().enumerate() {
        let is_active = active.get(bus_index).copied().unwrap_or(false);
        let Some(destination_bus) = outputs.get_mut(bus_index) else {
            return;
        };
        for channel in 0..bus.numChannels.max(0) as usize {
            let Some(destination) = destination_bus.channels.get_mut(channel) else {
                return;
            };
            let start = frame_offset.min(destination.len());
            let clear_count = frames.min(destination.len().saturating_sub(start));
            if !is_active {
                destination[start..start + clear_count].fill(0.0);
                continue;
            }
            let source = samples.get(sample_index + channel);
            let count = source
                .map(|source| clear_count.min(source.len()))
                .unwrap_or(0);
            if channel < 64 && bus.silenceFlags & (1u64 << channel) != 0 {
                destination[start..start + count].fill(0.0);
            } else if let Some(source) = source {
                for (to, from) in destination[start..start + count]
                    .iter_mut()
                    .zip(&source[..count])
                {
                    *to = convert(*from);
                }
            }
            destination[start + count..start + clear_count].fill(0.0);
        }
        if is_active {
            sample_index += bus.numChannels.max(0) as usize;
        }
    }
}

fn channel_mask(channel_count: i32) -> u64 {
    match channel_count.max(0) as u32 {
        0 => 0,
        64.. => u64::MAX,
        count => (1u64 << count) - 1,
    }
}

fn update_bus_silence_flags<T>(
    samples: &[Vec<T>],
    buses: &mut [AudioBusBuffers],
    active: &[bool],
    frames: usize,
    preserve_plugin_flags: bool,
    is_positive_zero: impl Fn(&T) -> bool,
) {
    // Silence hints are optional. Inferring them only from positive-zero bits
    // preserves signed-zero samples that native or host silence optimizations
    // would otherwise replace with +0. Plugin-declared flags remain authoritative.
    let mut sample_index = 0usize;
    for (bus_index, bus) in buses.iter_mut().enumerate() {
        let channel_count = bus.numChannels.max(0) as usize;
        if !active.get(bus_index).copied().unwrap_or(false) {
            bus.silenceFlags = channel_mask(bus.numChannels);
            continue;
        }

        let mut computed = 0u64;
        for channel in 0..channel_count {
            if channel < 64
                && samples
                    .get(sample_index + channel)
                    .is_none_or(|samples| samples.iter().take(frames).all(&is_positive_zero))
            {
                computed |= 1u64 << channel;
            }
        }
        bus.silenceFlags = if preserve_plugin_flags {
            (bus.silenceFlags | computed) & channel_mask(bus.numChannels)
        } else {
            computed
        };
        sample_index += channel_count;
    }
}

fn prepare_output_silence_flags(buses: &mut [AudioBusBuffers], active: &[bool]) {
    for (index, bus) in buses.iter_mut().enumerate() {
        bus.silenceFlags = if active.get(index).copied().unwrap_or(false) {
            0
        } else {
            channel_mask(bus.numChannels)
        };
    }
}

fn copy_bus_outputs_to<T: Copy>(
    samples: &[Vec<T>],
    outputs: &mut [Vec<f32>],
    frame_offset: usize,
    frames: usize,
    buses: &[AudioBusBuffers],
    active: &[bool],
    convert: impl Fn(T) -> f32,
) {
    let mut sample_index = 0usize;
    let mut destination_index = 0usize;
    for (bus_index, bus) in buses.iter().enumerate() {
        if !active.get(bus_index).copied().unwrap_or(false) {
            continue;
        }
        for channel in 0..bus.numChannels.max(0) as usize {
            let source = samples.get(sample_index + channel);
            let Some(destination) = outputs.get_mut(destination_index) else {
                return;
            };
            let start = frame_offset.min(destination.len());
            let count = source
                .map(|source| {
                    frames
                        .min(source.len())
                        .min(destination.len().saturating_sub(start))
                })
                .unwrap_or(0);
            if channel < 64 && bus.silenceFlags & (1u64 << channel) != 0 {
                destination[start..start + count].fill(0.0);
            } else if let Some(source) = source {
                for (to, from) in destination[start..start + count]
                    .iter_mut()
                    .zip(&source[..count])
                {
                    *to = convert(*from);
                }
            }
            let clear_end = frames.min(destination.len().saturating_sub(start));
            destination[start + count..start + clear_end].fill(0.0);
            destination_index += 1;
        }
        sample_index += bus.numChannels.max(0) as usize;
    }
    for destination in outputs.iter_mut().skip(destination_index) {
        let start = frame_offset.min(destination.len());
        let count = frames.min(destination.len().saturating_sub(start));
        destination[start..start + count].fill(0.0);
    }
}

fn view_rect_size(rect: &ViewRect) -> Result<(i32, i32)> {
    let width = rect
        .right
        .checked_sub(rect.left)
        .filter(|width| *width > 0)
        .ok_or_else(|| Error::Other("plugin editor returned an invalid width".to_string()))?;
    let height = rect
        .bottom
        .checked_sub(rect.top)
        .filter(|height| *height > 0)
        .ok_or_else(|| Error::Other("plugin editor returned an invalid height".to_string()))?;
    Ok((width, height))
}

/// Offer a content scale factor to an editor view.
///
/// `Ok(true)` means the view took the factor. `Ok(false)` means it declined — either it does not
/// implement `IPlugViewContentScaleSupport` at all, or it implements it and answered with a
/// non-success code. Declining is a normal answer, not a failure: JUCE editors implement the
/// interface and return `kResultFalse` on macOS, where the window server already scales the
/// backing store. Treating that as an error takes the editor away from every JUCE plugin, so it
/// never becomes one. Only a nonsensical *request* (non-finite or non-positive) is an error.
unsafe fn set_view_scale_factor(view: &ComPtr<IPlugView>, factor: f32) -> Result<bool> {
    if !factor.is_finite() || factor <= 0.0 {
        return Err(Error::Other(
            "editor scale factor must be finite and greater than zero".to_string(),
        ));
    }
    let Some(scale_support) = view.cast::<IPlugViewContentScaleSupport>() else {
        return Ok(false);
    };
    let result = scale_support.setContentScaleFactor(factor);
    let accepted = result == kResultOk || result == kResultTrue;
    if !accepted {
        log::debug!("plugin declined editor scale factor {factor}: {result:#x}");
    }
    Ok(accepted)
}

/// Fill `event` with the VST3 note event a MIDI note-on maps to, returning whether that message
/// *releases* the note rather than starting it.
///
/// MIDI uses a note-on with velocity 0 as an alias for note-off. VST3 has no running status and
/// no such alias, so the alias has to become a real `kNoteOffEvent`: a zero-velocity
/// `kNoteOnEvent` leaves the plugin holding the voice while the host's own note tracker counts
/// it as released, and `midi_panic` then has nothing left to release it with.
fn write_midi_note_on(event: &mut Event, channel: MidiChannel, note: u8, velocity: u8) -> bool {
    if velocity == 0 {
        write_note_off_event(event, channel, note, 0);
        return true;
    }
    event.r#type = kNoteOnEvent as u16;
    event.__field0.noteOn.channel = channel.as_index() as i16;
    event.__field0.noteOn.pitch = note as i16;
    event.__field0.noteOn.tuning = 0.0;
    event.__field0.noteOn.velocity = velocity as f32 / 127.0;
    event.__field0.noteOn.length = 0;
    event.__field0.noteOn.noteId = -1;
    false
}

/// Fill `event` with a `kNoteOffEvent` for an ordinary (host-untracked) note.
fn write_note_off_event(event: &mut Event, channel: MidiChannel, note: u8, velocity: u8) {
    event.r#type = kNoteOffEvent as u16;
    event.__field0.noteOff.channel = channel.as_index() as i16;
    event.__field0.noteOff.pitch = note as i16;
    event.__field0.noteOff.velocity = velocity as f32 / 127.0;
    event.__field0.noteOff.noteId = -1;
    event.__field0.noteOff.tuning = 0.0;
}

impl PluginImpl {
    fn ensure_control_thread(&self, operation: &str) -> Result<()> {
        if thread::current().id() == self.control_thread {
            Ok(())
        } else {
            Err(Error::Other(format!(
                "{operation} must run on the plugin control thread"
            )))
        }
    }

    fn ensure_stream_size(&self, data: &[u8]) -> Result<()> {
        if data.len() <= super::com_implementations::MAX_STREAM_BYTES {
            Ok(())
        } else {
            Err(Error::Other(format!(
                "plugin data stream is too large ({} bytes, maximum {})",
                data.len(),
                super::com_implementations::MAX_STREAM_BYTES
            )))
        }
    }

    fn check_controller_result(result: tresult, operation: &str) -> Result<()> {
        if result == kResultOk || result == kResultTrue {
            Ok(())
        } else {
            Err(Error::Other(format!("{operation} failed: {result:#x}")))
        }
    }

    /// Configure the transport advertised to the plugin in the host `ProcessContext`.
    ///
    /// Call before processing starts (the values are baked into the context when
    /// `create_process_data` runs during `start_processing`). The musical playhead
    /// (`projectTimeMusic`) derives from `tempo` as the transport advances.
    pub fn set_transport(
        &mut self,
        tempo: f64,
        time_sig_numerator: i32,
        time_sig_denominator: i32,
    ) {
        self.tempo = tempo;
        self.time_sig_numerator = time_sig_numerator;
        self.time_sig_denominator = time_sig_denominator;
    }

    /// Update the transport tempo for the **next** processed block, even while processing is
    /// active: the stored tempo (used to rebuild the context after a reconfigure) and the live
    /// `ProcessContext` both move, and the musical playhead derives from the new tempo.
    #[allow(clippy::unnecessary_cast)]
    fn update_tempo(&mut self, bpm: f64) {
        self.tempo = bpm;
        if let Some(ref mut data) = self.process_data {
            data.transport_tempo = bpm;
            if process_context_needs(
                data.process_context_requirements,
                IProcessContextRequirements_::Flags_::kNeedTempo as u32,
            ) {
                data.process_context.tempo = bpm;
                data.process_context.state |= ProcessContext_::StatesAndFlags_::kTempoValid as u32;
            }
        }
    }

    /// Update the transport time signature for the **next** processed block, even while
    /// processing is active (stored fields plus the live `ProcessContext`).
    #[allow(clippy::unnecessary_cast)]
    fn update_time_signature(&mut self, numerator: i32, denominator: i32) {
        self.time_sig_numerator = numerator;
        self.time_sig_denominator = denominator;
        if let Some(ref mut data) = self.process_data {
            if process_context_needs(
                data.process_context_requirements,
                IProcessContextRequirements_::Flags_::kNeedTimeSignature as u32,
            ) {
                data.process_context.timeSigNumerator = numerator;
                data.process_context.timeSigDenominator = denominator;
                data.process_context.state |=
                    ProcessContext_::StatesAndFlags_::kTimeSigValid as u32;
            }
        }
    }

    /// Toggle the transport playing state (`kPlaying`) for the **next** processed block, even
    /// while processing is active (stored field plus the live `ProcessContext.state`).
    fn update_playing(&mut self, playing: bool) {
        self.playing = playing;
        if let Some(ref mut data) = self.process_data {
            data.process_context.state = process_context_state(
                data.process_context_requirements,
                playing,
                data.process_context.state,
            );
        }
    }

    /// Apply the host-configured sample rate / block size before processing starts. Called at
    /// load so `setupProcessing` (which runs at `start_processing`) uses the builder's settings
    /// rather than the internal defaults.
    pub fn set_audio_config(&mut self, sample_rate: f64, block_size: usize) {
        self.bus_activation.event_readiness = None;
        self.sample_rate = sample_rate;
        self.block_size = block_size;
    }

    /// Get parameter changes the plugin's editor made (for the host to update its UI).
    ///
    /// Returns edits that `process()` has already routed into the processor's input queue, so
    /// the DSP and the host display stay in sync. (Before processing has started, falls back to
    /// the raw performEdit sink so edits aren't lost.)
    pub fn get_parameter_changes(&self) -> Vec<(u32, f64)> {
        self.drain_deferred_controller_sync();
        let mut changes = Vec::new();
        while let Some(change) = self.output_param_feedback.pop() {
            changes.push(change);
        }
        // Both drains take the elements in place rather than `mem::take`-ing the `Vec`: taking it
        // would leave a zero-capacity buffer behind, so the next block's `append` would
        // reallocate — on the audio thread, for the stash.
        if let Ok(mut stash) = self.gui_param_changes_for_host.lock() {
            if !stash.is_empty() {
                changes.extend(stash.drain(..));
            }
        }
        // Not processing yet (process() hasn't run to move edits into the stash): drain the raw
        // performEdit sink directly so the host UI still reflects editor changes.
        if !self.is_processing {
            if let Some(ref handler) = self.component_handler {
                if let Ok(mut raw_changes) = handler.parameter_changes.lock() {
                    changes.extend(raw_changes.drain(..));
                }
            }
        }
        changes
    }

    /// Push a value to the edit controller, tolerating the result codes real plugins return.
    ///
    /// The SDK's reference `EditController` answers `kResultTrue` on success and `kResultFalse`
    /// for an id it doesn't own, which makes the code look like a usable success signal — but
    /// shipping plugins don't honour it. Dexed (JUCE) returns `kResultFalse` for parameter ids
    /// 0/1/2 that it then applies correctly, so refusing to apply on a false would break
    /// automation on real plugins. Log it and carry on; `log` skips the formatting entirely when
    /// the level is disabled.
    fn apply_controller_parameter(&self, id: u32, value: f64) {
        apply_controller_value(&self.controller, id, value)
    }

    /// Keep `IEditController` in step with a value queued for the processor.
    ///
    /// Controller calls belong to the main-thread domain, so a value arriving from the audio
    /// callback is parked in a bounded queue instead and applied by the next control-thread
    /// service call. The queue drops its oldest entry when full: the newest value for a
    /// parameter is the one worth showing.
    fn mirror_parameter_to_controller(&self, id: u32, value: f64) {
        if thread::current().id() == self.control_thread {
            self.apply_controller_parameter(id, value);
        } else {
            self.deferred_controller_sync.force_push((id, value));
        }
    }

    /// Apply parameter values that were queued off the control thread to `IEditController`.
    /// A no-op anywhere but the control thread, so the audio path never reaches the controller.
    fn drain_deferred_controller_sync(&self) {
        if thread::current().id() != self.control_thread {
            return;
        }
        while let Some((id, value)) = self.deferred_controller_sync.pop() {
            self.apply_controller_parameter(id, value);
        }
    }

    /// Rebuild whichever controller-derived caches were marked stale off the control thread,
    /// and apply parameter values queued for the controller from the same place. A no-op
    /// anywhere but the control thread; both halves are `IEditController` traffic.
    fn service_control_thread_caches(&mut self) {
        if thread::current().id() != self.control_thread {
            return;
        }
        self.drain_deferred_controller_sync();
        if self
            ._host_app
            .consumes_plugin_interface(crate::PluginInterface::MidiMapping)
            && std::mem::take(&mut self.dirty_caches.midi_mapping)
        {
            self.refresh_midi_mapping_cache();
        }
        if self
            ._host_app
            .consumes_plugin_interface(crate::PluginInterface::UnitInfo)
            && std::mem::take(&mut self.dirty_caches.program_change)
        {
            self.refresh_program_change_cache();
        }
    }

    fn refresh_midi_mapping_cache(&mut self) {
        if !self
            ._host_app
            .consumes_plugin_interface(crate::PluginInterface::MidiMapping)
        {
            return;
        }
        let buses = unsafe {
            midi_mapping_bus_count(self.component.getBusCount(kEvent as i32, kInput as i32))
        };
        let mut cache = MidiMappingCache {
            buses,
            assignments: vec![None; buses * MIDI_CHANNEL_COUNT * MIDI_CONTROLLER_COUNT],
        };
        let Some(mapping) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IMidiMapping>())
        else {
            self.midi_mapping_cache = cache;
            return;
        };
        unsafe {
            for bus in 0..buses {
                for channel in 0..MIDI_CHANNEL_COUNT {
                    for controller in 0..MIDI_CONTROLLER_COUNT {
                        let mut id = 0;
                        if mapping.getMidiControllerAssignment(
                            bus as i32,
                            channel as i16,
                            controller as CtrlNumber,
                            &mut id,
                        ) == kResultOk
                        {
                            let index = (bus * MIDI_CHANNEL_COUNT + channel)
                                * MIDI_CONTROLLER_COUNT
                                + controller;
                            cache.assignments[index] = Some(id);
                        }
                    }
                }
            }
        }
        self.midi_mapping_cache = cache;
    }

    fn refresh_program_change_cache(&mut self) {
        if !self
            ._host_app
            .consumes_plugin_interface(crate::PluginInterface::UnitInfo)
        {
            return;
        }
        self.program_change_cache.clear();
        let Some(controller) = self.controller.as_ref() else {
            return;
        };
        let Some(unit_info) = controller.cast::<IUnitInfo>() else {
            return;
        };
        unsafe {
            let mut unit_lists = Vec::new();
            for index in 0..unit_info.getUnitCount() {
                let mut unit: UnitInfo = std::mem::zeroed();
                if unit_info.getUnitInfo(index, &mut unit) == kResultOk {
                    unit_lists.push((unit.id, unit.programListId));
                }
            }
            let mut list_counts = Vec::new();
            for index in 0..unit_info.getProgramListCount() {
                let mut list: ProgramListInfo = std::mem::zeroed();
                if unit_info.getProgramListInfo(index, &mut list) == kResultOk {
                    list_counts.push((list.id, list.programCount));
                }
            }
            for index in 0..controller.getParameterCount() {
                let mut parameter: ParameterInfo = std::mem::zeroed();
                if controller.getParameterInfo(index, &mut parameter) != kResultOk
                    || parameter.flags & ParameterInfo_::ParameterFlags_::kIsProgramChange == 0
                {
                    continue;
                }
                let Some((_, list_id)) = unit_lists
                    .iter()
                    .find(|(unit_id, _)| *unit_id == parameter.unitId)
                else {
                    continue;
                };
                let Some((_, program_count)) = list_counts.iter().find(|(id, _)| id == list_id)
                else {
                    continue;
                };
                if *program_count > 0 {
                    self.program_change_cache.push(ProgramChangeMapping {
                        unit_id: parameter.unitId,
                        param_id: parameter.id,
                        program_count: *program_count,
                    });
                }
            }
        }
    }

    fn cached_program_change(&self, unit_id: i32) -> Option<ProgramChangeMapping> {
        self.program_change_cache
            .iter()
            .copied()
            .find(|mapping| mapping.unit_id == unit_id)
    }

    /// Load a VST3 plugin from the given path
    pub fn load(path: &std::path::Path, interfaces: crate::PluginInterfaceSupport) -> Result<Self> {
        Self::load_with_class(path, None, interfaces)
    }

    /// Load a specific current or moduleinfo-retired audio class from a VST3 bundle.
    pub fn load_class(
        path: &std::path::Path,
        requested_class_id: &str,
        interfaces: crate::PluginInterfaceSupport,
    ) -> Result<Self> {
        Self::load_with_class(path, Some(requested_class_id), interfaces)
    }

    fn load_with_class(
        path: &std::path::Path,
        requested_class_id: Option<&str>,
        interfaces: crate::PluginInterfaceSupport,
    ) -> Result<Self> {
        let native_editor_owner = crate::plugin::NativeEditorAttachmentOwner::issue()?;
        let requested_class_id = requested_class_id
            .map(|requested| -> Result<String> {
                crate::internal::utils::parse_class_uid(requested).ok_or_else(|| {
                    Error::PluginLoadFailed(
                        "class id must be exactly 32 hexadecimal characters".to_string(),
                    )
                })?;
                Ok(requested.to_ascii_uppercase())
            })
            .transpose()?;
        // A file is authoritative. The runtime compatibility class is queried only when the
        // bundle has no moduleinfo.json.
        let module_info = crate::internal::module_info::read(path)?;

        unsafe {
            log::info!("=== PLUGIN LOADING START ===");
            log::info!("Loading plugin from: {}", path.display());

            // Load the VST3 module using platform-specific loader. Declared first so it drops
            // LAST: everything below lives in the module's address space, and the COM teardown
            // (whether via `InitializedComponent` or `Drop for PluginImpl`) must complete before
            // the module unmaps.
            log::debug!("Step 1: Loading VST3 module...");
            let module = load_module(path)?;
            log::debug!("VST3 module loaded successfully");

            // Get factory from module
            log::debug!("Step 2: Getting factory from module...");
            let factory_ptr = module.get_factory()?;
            log::debug!("Factory obtained, ptr: {:?}", factory_ptr);
            if factory_ptr.is_null() {
                return Err(Error::PluginLoadFailed(
                    "GetPluginFactory returned null".to_string(),
                ));
            }

            log::debug!("Step 3: Wrapping factory in ComPtr...");
            let factory = ComPtr::<IPluginFactory>::from_raw(factory_ptr).ok_or_else(|| {
                Error::PluginLoadFailed("Failed to create factory ComPtr".to_string())
            })?;
            log::debug!("Factory wrapped successfully");

            // Factory3 must receive the host context before any class is instantiated. Keep the
            // same context alive for the complete component/controller lifetime.
            let host_app = create_host_application_with_interfaces(interfaces);
            let host_ctx = host_app.to_com_ptr::<IHostApplication>();
            let context = host_ctx
                .as_ref()
                .map(|p| p.as_ptr() as *mut FUnknown)
                .unwrap_or(ptr::null_mut());
            if let Some(factory3) = factory.cast::<IPluginFactory3>() {
                let result = factory3.setHostContext(context);
                if result != kResultOk && result != kResultTrue {
                    log::warn!("IPluginFactory3::setHostContext failed: {result:#x}");
                }
            }
            let compatibility = match module_info.as_ref() {
                Some(module_info) => module_info.compatibility.clone(),
                None => crate::internal::module_info::read_factory_compatibility(&factory)?,
            };
            let target_class_id = match requested_class_id.as_deref() {
                Some(requested) if module_info.is_some() => Some(
                    module_info
                        .as_ref()
                        .and_then(|module_info| module_info.resolve_class_id(requested))
                        .ok_or_else(|| {
                            Error::PluginLoadFailed(format!(
                                "class id {requested} is not declared by {}",
                                path.display()
                            ))
                        })?
                        .to_string(),
                ),
                Some(requested) => Some(
                    crate::internal::module_info::resolve_factory_audio_class_id(
                        &factory,
                        &compatibility,
                        requested,
                    )?,
                ),
                None => None,
            };

            // Find and create the audio component
            log::debug!("Step 4: Creating audio component...");
            let (component, class_index) =
                Self::create_component(&factory, target_class_id.as_deref())?;
            log::debug!("Component created successfully (class index {class_index})");

            // Initialize component with a host-application context. Passing null here
            // crashes plugins that query the host (u-he, Waves, ...); see HostApplication.
            log::debug!("Step 5: Initializing component...");
            let init_result = component.initialize(context);
            if init_result != kResultOk {
                // A component that failed to initialize must NOT be terminated (terminate pairs
                // with a *successful* initialize), and must certainly not be driven on through
                // activateBus/setActive/process — so bail before anything else touches it. The
                // ComPtr releases it here and the module unloads as this frame unwinds.
                return Err(Error::PluginLoadFailed(format!(
                    "IComponent::initialize failed: {init_result:#x}"
                )));
            }
            log::debug!("Component initialized with result: {init_result:#x}");

            // The component is live from here: it may have spawned threads and registered
            // callbacks, so every failure below has to run the full ordered teardown before the
            // module unloads. The guard does that until the finished `PluginImpl` takes over.
            let mut initialized = InitializedComponent::new(component.clone());

            // VST3 3.7 requires hosts to query this after initialize and before activation.
            // Older processors do not expose it; `None` selects the safe legacy context.
            let process_context_requirements = component
                .cast::<IProcessContextRequirements>()
                .map(|requirements| requirements.getProcessContextRequirements());
            let prefetchable_support = Self::query_prefetchable_support(&component);

            // Establish the component's declared default bus state while it is inactive.
            log::debug!("Step 6: Activating default buses...");
            let bus_activation = Self::activate_default_buses(&component)?;
            log::debug!("Default buses activated");

            // Get processor interface
            log::debug!("Step 7: Getting IAudioProcessor interface...");
            let processor = component.cast::<IAudioProcessor>().ok_or_else(|| {
                Error::InterfaceError("Component does not implement IAudioProcessor".to_string())
            })?;
            let sample_size = Self::select_sample_size(&processor)?;
            log::debug!("IAudioProcessor interface obtained");

            // Create component handler for parameter change notifications
            log::debug!("Step 8: Creating component handler...");
            let parameter_changes = Arc::new(Mutex::new(Vec::with_capacity(MAX_EDITOR_FEEDBACK)));
            let component_handler =
                ComWrapper::new(ComponentHandler::new(parameter_changes.clone()));
            log::debug!("Component handler created");

            // Get or create controller (handles both single-component and separate controller)
            log::debug!("Step 9: Getting or creating controller...");
            // A component that directly implements IEditController is a single-component
            // plugin; this distinction matters for state restore (see `single_component`).
            let single_component = component.cast::<IEditController>().is_some();
            let controller = Self::get_or_create_controller(&component, &factory, context)?;
            initialized.attach_controller(controller.clone(), single_component);
            log::debug!(
                "Controller obtained: {} (single_component: {single_component})",
                controller.is_some()
            );

            // Connect component and controller if they are separate
            if let Some(ref ctrl) = controller {
                log::debug!("Step 10: Connecting component and controller...");
                let connection = Self::connect_component_and_controller(&component, ctrl)?;
                initialized.attach_connection(connection);
                log::debug!("Component and controller connected");

                // Set component handler on controller for parameter change notifications
                log::debug!("Step 11: Setting component handler on controller...");
                if let Some(handler_ptr) = component_handler.as_com_ref::<IComponentHandler>() {
                    let result = ctrl.setComponentHandler(handler_ptr.as_ptr());
                    if result == kResultOk {
                        log::debug!("Component handler set on controller successfully");
                    } else {
                        log::warn!(
                            "Failed to set component handler on controller: {:#x}",
                            result
                        );
                    }
                } else {
                    log::error!("Failed to get IComponentHandler COM pointer");
                }

                if !single_component {
                    // A separate controller does not share the component's state implicitly. The
                    // initial synchronization belongs here: both objects are initialized and
                    // connected, while the component is still inactive.
                    let component_state =
                        create_memory_stream_with_metadata(None, StreamStateType::Project);
                    host_app.register_memory_stream(&component_state)?;
                    let component_state_ptr =
                        component_state.to_com_ptr::<IBStream>().ok_or_else(|| {
                            Error::InterfaceError(
                                "failed to create initial component-state stream".to_string(),
                            )
                        })?;
                    let get_state_result = component.getState(component_state_ptr.as_ptr());
                    if get_state_result == kResultOk || get_state_result == kResultTrue {
                        let controller_state = create_memory_stream_from_with_metadata(
                            component_state.to_vec(),
                            None,
                            StreamStateType::Project,
                        );
                        host_app.register_memory_stream(&controller_state)?;
                        let controller_state_ptr =
                            controller_state.to_com_ptr::<IBStream>().ok_or_else(|| {
                                Error::InterfaceError(
                                    "failed to create initial controller-state stream".to_string(),
                                )
                            })?;
                        let set_state_result =
                            ctrl.setComponentState(controller_state_ptr.as_ptr());
                        if set_state_result != kResultOk
                            && set_state_result != kResultTrue
                            && set_state_result != kNotImplemented
                            && set_state_result != kResultFalse
                        {
                            return Err(Error::PluginLoadFailed(format!(
                                "initial controller state synchronization failed: \
                                 {set_state_result:#x}"
                            )));
                        }
                    } else if get_state_result != kNotImplemented
                        && get_state_result != kResultFalse
                    {
                        return Err(Error::PluginLoadFailed(format!(
                            "initial component state read failed: {get_state_result:#x}"
                        )));
                    }
                }
            }

            // Editor resize plumbing (an IPlugFrame the view can call into),
            // plus the Linux IRunLoop registry VSTGUI-based editors need.
            let editor_resize = Arc::new(Mutex::new(None));
            #[cfg(target_os = "linux")]
            let run_loop = Arc::new(Mutex::new(RunLoopRegistry::new()));
            #[cfg(target_os = "linux")]
            let plug_frame = create_host_plug_frame(editor_resize.clone(), run_loop.clone());
            #[cfg(not(target_os = "linux"))]
            let plug_frame = create_host_plug_frame(editor_resize.clone());

            // Create event lists
            log::debug!("Step 13: Creating event lists...");
            let input_events = create_event_list();
            let output_events = create_event_list();
            log::debug!("Event lists created");

            // Extract plugin info from the factory and component. Keyed on the class that was
            // actually instantiated, so a multi-class factory reports the uid the host can
            // reload (or respawn under isolation) to get this same component back.
            let info = Self::extract_plugin_info(path, &factory, &component, class_index)?;

            let has_gui = controller.is_some() && {
                if let Some(ref ctrl) = controller {
                    let view_type = c"editor".as_ptr();
                    let view_ptr = ctrl.createView(view_type);
                    if !view_ptr.is_null() {
                        // Release the probe view; never call `removed()` on it — that pairs with
                        // `attached()`, and an unmatched `removed()` crashes some plugins that
                        // initialize their close state only on attach.
                        let _ = ComPtr::<IPlugView>::from_raw(view_ptr);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            };

            let mut updated_info = info;
            updated_info.has_gui = has_gui;

            host_app.configure_data_exchange(
                processor.as_ptr(),
                controller
                    .as_ref()
                    .and_then(|controller| controller.cast::<IDataExchangeReceiver>()),
            );

            log::info!(
                "Plugin info: {} by {}",
                updated_info.name,
                updated_info.vendor
            );

            let mut plugin = Self {
                component,
                processor,
                controller,
                single_component,
                info: updated_info,
                compatibility,
                is_active: false,
                is_processing: false,
                sample_rate: 44100.0,
                block_size: 512,
                applied_setup: None,
                tempo: 120.0,
                time_sig_numerator: 4,
                time_sig_denominator: 4,
                playing: true,
                process_mode: crate::plugin::ProcessMode::Realtime,
                process_context_requirements,
                prefetchable_support,
                sample_size,
                bus_activation,
                next_note_id: 1,
                active_notes: Vec::with_capacity(MAX_TRACKED_NOTES),
                ordinary_note_counts: [0; MIDI_CHANNEL_COUNT * 128],
                midi_mapping_cache: MidiMappingCache::default(),
                program_change_cache: Vec::new(),
                dirty_caches: DirtyCaches::default(),
                unit_cache: Mutex::new(None),
                process_data: None,
                component_handler: Some(component_handler),
                connection: initialized.take_connection(),
                pending_param_changes: Vec::with_capacity(MAX_PENDING_PARAM_CHANGES),
                dropped_param_changes: 0,
                gui_param_changes_for_host: Arc::new(Mutex::new(Vec::with_capacity(
                    MAX_EDITOR_FEEDBACK,
                ))),
                output_param_feedback: Arc::new(ArrayQueue::new(MAX_OUTPUT_PARAMETER_FEEDBACK)),
                deferred_controller_sync: ArrayQueue::new(MAX_DEFERRED_CONTROLLER_SYNC),
                input_events,
                output_events,
                chunk_events: Vec::with_capacity(MAX_QUEUED_EVENTS),
                output_events_owned: Arc::new(ArrayQueue::new(MAX_OUTPUT_MIDI)),
                native_editor_owner,
                plugin_view: EditorAttachment::Empty,
                editor_scale_factor: 1.0,
                plug_frame,
                editor_resize,
                #[cfg(target_os = "linux")]
                run_loop,
                _module: module,
                _host_app: host_app,
                control_thread: thread::current().id(),
            };
            // From here on `plugin` owns the teardown: dropping it runs the same ordered
            // sequence the guard would.
            initialized.disarm();

            // IMidiMapping and unit/program metadata are controller calls and therefore belong
            // here on the loading thread, never in process() or a playback command drain.
            plugin.refresh_midi_mapping_cache();
            plugin.refresh_program_change_cache();

            // Give the processor its own currently-advertised arrangements before the first
            // setup. A refusal is legitimate (the plugin keeps its layout); other failures are
            // surfaced instead of being silently discarded.
            plugin.negotiate_default_bus_arrangements()?;

            // VST3 lifecycle: `setupProcessing` and bus activation require an INACTIVE
            // component, and a plugin sizes its DSP buffers when it goes active, from the
            // ProcessSetup it was last given. So set up first, activate second — activating an
            // un-set-up component hands it whatever defaults it was born with.
            //
            // Both are best-effort here, and neither failure fails the load: this runs at the
            // internal default configuration (the host applies its real sample rate and block
            // size immediately after load), the plugin remains fully inspectable either way, and
            // `start_processing` re-runs both with the real settings and reports a genuine
            // failure there.
            if let Err(e) = plugin.setup_processing() {
                log::warn!("setupProcessing failed at load ({e}); deferring to start_processing");
            } else if let Err(e) = plugin.activate() {
                log::warn!(
                    "Component activation failed at load ({e}); deferring to start_processing"
                );
            }

            log::info!("=== PLUGIN LOADING COMPLETE ===");
            log::info!(
                "Has GUI: {}, Active: {}",
                plugin.info.has_gui,
                plugin.is_active
            );
            Ok(plugin)
        }
    }

    /// Put the component into the active state (`IComponent::setActive(true)`).
    ///
    /// Only valid once the component has been given a `ProcessSetup` — see the ordering note in
    /// [`Self::load`].
    fn activate(&mut self) -> Result<()> {
        let result = self.set_component_active(true);
        if result != kResultOk {
            return Err(Error::Other(format!(
                "Failed to activate component: {result:#x}"
            )));
        }
        self.is_active = true;
        Ok(())
    }

    fn checked_event_readiness(&self) -> std::result::Result<(), NativeEventError> {
        self.visit_native_event_bus_constraints(&mut |_, _, _, _, _, _| Ok(()))
            .map_err(|error| match error {
                Error::NativeEvent(error) => error,
                _ => NativeEventError::PreparationRequired,
            })
    }

    fn set_component_active(&mut self, active: bool) -> tresult {
        self.bus_activation.event_readiness = None;
        set_native_component_active(&self.component, &self._host_app, active)
    }

    fn deactivate_for_configuration(&mut self) -> Result<()> {
        let result = self.set_component_active(false);
        if result != kResultOk {
            return Err(Error::NativeEvent(NativeEventError::ComponentActivation {
                active: false,
                result,
            }));
        }
        self.is_active = false;
        Ok(())
    }

    /// Describe the plugin class at `class_index` — the class [`Self::create_component`]
    /// actually instantiated, not merely the first audio class the factory lists. A factory can
    /// export several audio classes (a synth plus its effect sibling, or per-format variants),
    /// and the reported `uid` is what a host stores to reload this exact plugin.
    fn extract_plugin_info(
        path: &std::path::Path,
        factory: &ComPtr<IPluginFactory>,
        component: &ComPtr<IComponent>,
        class_index: i32,
    ) -> Result<PluginInfo> {
        unsafe {
            // Get factory info
            let mut factory_info: PFactoryInfo = std::mem::zeroed();
            factory.getFactoryInfo(&mut factory_info);
            let mut vendor = crate::internal::utils::c_str_to_string(&factory_info.vendor);

            let mut class_info: PClassInfo = std::mem::zeroed();
            if factory.getClassInfo(class_index, &mut class_info) != kResultOk {
                return Err(Error::PluginLoadFailed(format!(
                    "Could not read class info for the instantiated class (index {class_index})"
                )));
            }

            let mut name = crate::internal::utils::c_str_to_string(&class_info.name);
            let uid = crate::internal::utils::format_class_uid(&class_info.cid);

            // Count audio buses
            let audio_inputs = component.getBusCount(kAudio as i32, kInput as i32) as u32;
            let audio_outputs = component.getBusCount(kAudio as i32, kOutput as i32) as u32;

            // Real version + sub-categories via IPluginFactory2 when the factory
            // provides it; left empty (honest) rather than faked when it doesn't.
            let (mut version, mut category) = factory
                .cast::<IPluginFactory2>()
                .and_then(|f2| {
                    let mut info2: PClassInfo2 = std::mem::zeroed();
                    if f2.getClassInfo2(class_index, &mut info2) == kResultOk {
                        Some((
                            crate::internal::utils::c_str_to_string(&info2.version),
                            crate::internal::utils::c_str_to_string(&info2.subCategories),
                        ))
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            // Factory3 carries class/vendor/version names as UTF-16. Prefer it so non-ASCII
            // metadata is not mangled, while retaining Factory2/legacy fallbacks.
            if let Some(factory3) = factory.cast::<IPluginFactory3>() {
                let mut info3: PClassInfoW = std::mem::zeroed();
                if factory3.getClassInfoUnicode(class_index, &mut info3) == kResultOk {
                    let utf16 = |value: &[u16]| {
                        let end = value.iter().position(|&ch| ch == 0).unwrap_or(value.len());
                        String::from_utf16_lossy(&value[..end])
                    };
                    let unicode_name = utf16(&info3.name);
                    let unicode_vendor = utf16(&info3.vendor);
                    let unicode_version = utf16(&info3.version);
                    if !unicode_name.is_empty() {
                        name = unicode_name;
                    }
                    if !unicode_vendor.is_empty() {
                        vendor = unicode_vendor;
                    }
                    if !unicode_version.is_empty() {
                        version = unicode_version;
                    }
                    let unicode_category =
                        crate::internal::utils::c_str_to_string(&info3.subCategories);
                    if !unicode_category.is_empty() {
                        category = unicode_category;
                    }
                }
            }

            // MIDI capability from the presence of event buses, not a guess.
            let has_midi_input = component.getBusCount(kEvent as i32, kInput as i32) > 0;
            let has_midi_output = component.getBusCount(kEvent as i32, kOutput as i32) > 0;

            Ok(PluginInfo {
                path: path.to_path_buf(),
                name,
                vendor,
                version,
                category,
                uid,
                audio_inputs,
                audio_outputs,
                has_gui: false, // Will be updated by caller
                has_midi_input,
                has_midi_output,
            })
        }
    }

    /// Find and create the audio component from the factory, returning it together with the
    /// factory class index it came from (so the reported [`PluginInfo`] describes that class).
    unsafe fn create_component(
        factory: &ComPtr<IPluginFactory>,
        target_class_id: Option<&str>,
    ) -> Result<(ComPtr<IComponent>, i32)> {
        let num_classes = factory.countClasses();

        for i in 0..num_classes {
            let mut class_info = std::mem::zeroed();
            if factory.getClassInfo(i, &mut class_info) == kResultOk {
                let category = crate::internal::utils::c_str_to_string(&class_info.category);

                let class_id = crate::internal::utils::format_class_uid(&class_info.cid);
                let is_target = target_class_id.is_none_or(|target| {
                    crate::internal::utils::class_uid_matches(&class_id, target)
                });

                if category.contains("Audio Module Class") && is_target {
                    let mut component_ptr: *mut IComponent = ptr::null_mut();

                    let result = factory.createInstance(
                        class_info.cid.as_ptr() as *const std::os::raw::c_char,
                        IComponent::IID.as_ptr() as *const std::os::raw::c_char,
                        &mut component_ptr as *mut _ as *mut _,
                    );

                    if result == kResultOk && !component_ptr.is_null() {
                        let component = ComPtr::from_raw(component_ptr).ok_or_else(|| {
                            Error::PluginLoadFailed("Failed to create component".to_string())
                        })?;
                        return Ok((component, i));
                    }
                }
            }
        }

        Err(Error::PluginLoadFailed(match target_class_id {
            Some(class_id) => format!("Audio component class {class_id} was not found in plugin"),
            None => "No audio component found in plugin".to_string(),
        }))
    }

    /// Map the configured [`ProcessMode`](crate::plugin::ProcessMode) to the VST3 enum value.
    fn vst_process_mode(&self) -> i32 {
        match self.process_mode {
            crate::plugin::ProcessMode::Offline => ProcessModes_::kOffline as i32,
            crate::plugin::ProcessMode::Prefetch => ProcessModes_::kPrefetch as i32,
            crate::plugin::ProcessMode::Realtime => ProcessModes_::kRealtime as i32,
        }
    }

    #[allow(clippy::unnecessary_cast)]
    unsafe fn query_prefetchable_support(component: &ComPtr<IComponent>) -> Option<u32> {
        let support = component.cast::<IPrefetchableSupport>()?;
        let mut value = ePrefetchableSupport_::kIsNotYetPrefetchable as u32;
        let result = support.getPrefetchableSupport(&mut value);
        (result == kResultOk || result == kResultTrue).then_some(value)
    }

    unsafe fn select_sample_size(
        processor: &ComPtr<IAudioProcessor>,
    ) -> Result<ProcessingSampleSize> {
        let sample32 = SymbolicSampleSizes_::kSample32 as i32;
        if processor.canProcessSampleSize(sample32) == kResultOk {
            return Ok(ProcessingSampleSize::F32);
        }
        let sample64 = SymbolicSampleSizes_::kSample64 as i32;
        if processor.canProcessSampleSize(sample64) == kResultOk {
            return Ok(ProcessingSampleSize::F64);
        }
        Err(Error::InterfaceError(
            "plugin supports neither 32-bit nor 64-bit processing".to_string(),
        ))
    }

    unsafe fn negotiate_default_bus_arrangements(&self) -> Result<()> {
        let arrangements = self.bus_arrangements()?;
        let mut inputs: Vec<u64> = arrangements.inputs.iter().map(|a| a.raw()).collect();
        let mut outputs: Vec<u64> = arrangements.outputs.iter().map(|a| a.raw()).collect();
        let result = self.processor.setBusArrangements(
            inputs.as_mut_ptr(),
            inputs.len() as i32,
            outputs.as_mut_ptr(),
            outputs.len() as i32,
        );
        if result == kResultOk || result == kResultTrue || result == kResultFalse {
            if result == kResultFalse {
                log::debug!("plugin declined its advertised default bus arrangements");
            }
            Ok(())
        } else {
            Err(Error::InterfaceError(format!(
                "default setBusArrangements failed: {result:#x}"
            )))
        }
    }

    /// The configuration currently baked into the plugin via `setupProcessing`.
    fn current_setup(&self) -> AppliedSetup {
        AppliedSetup {
            sample_rate: self.sample_rate,
            block_size: self.block_size,
            process_mode: self.vst_process_mode(),
            symbolic_sample_size: self.sample_size.symbolic(),
        }
    }

    /// Set up processing with the current configuration. VST3 requires the component to be
    /// **inactive**; every caller either runs before the first activation or deactivates first.
    fn setup_processing(&mut self) -> Result<()> {
        self.bus_activation.event_readiness = None;
        if self.pending_processor_parameter_input()? {
            return Err(Error::PendingParameterDelivery);
        }

        unsafe {
            // Set up processing
            let setup = ProcessSetup {
                processMode: self.vst_process_mode(),
                symbolicSampleSize: self.sample_size.symbolic(),
                maxSamplesPerBlock: self.block_size as i32,
                sampleRate: self.sample_rate,
            };

            let result = self.processor.setupProcessing(&setup as *const _ as *mut _);
            if result != kResultOk {
                return Err(Error::InterfaceError(format!(
                    "Failed to setup processing: {:#x}",
                    result
                )));
            }

            // Create process data
            self.create_process_data()?;
            self.applied_setup = Some(self.current_setup());

            Ok(())
        }
    }

    /// Create processing data structures
    // `as u32` on the StatesAndFlags_ constants is required where they are generated as
    // `i32`; on targets where they are already `u32` clippy flags it as redundant.
    #[allow(clippy::unnecessary_cast)]
    fn create_process_data(&mut self) -> Result<()> {
        if self.pending_processor_parameter_input()? {
            return Err(Error::PendingParameterDelivery);
        }
        unsafe {
            let mut data = Box::new(HostProcessData {
                process_data: std::mem::zeroed(),
                sample_buffers: match self.sample_size {
                    ProcessingSampleSize::F32 => HostSampleBuffers::F32(TypedSampleBuffers {
                        inputs: Vec::new(),
                        outputs: Vec::new(),
                        input_channel_ptrs: SendChannelPtrs(Vec::new()),
                        output_channel_ptrs: SendChannelPtrs(Vec::new()),
                    }),
                    ProcessingSampleSize::F64 => HostSampleBuffers::F64(TypedSampleBuffers {
                        inputs: Vec::new(),
                        outputs: Vec::new(),
                        input_channel_ptrs: SendChannelPtrs(Vec::new()),
                        output_channel_ptrs: SendChannelPtrs(Vec::new()),
                    }),
                },
                input_bus_buffers: Vec::new(),
                output_bus_buffers: Vec::new(),
                process_context: std::mem::zeroed(),
                process_context_requirements: self.process_context_requirements,
                transport_tempo: self.tempo,
                musical_reference: None,
                input_param_changes: ComWrapper::new(ParameterChanges::default()),
                output_param_changes: ComWrapper::new(ParameterChanges::default()),
            });

            // Initialize process context
            data.process_context.sampleRate = self.sample_rate;
            if process_context_needs(
                self.process_context_requirements,
                IProcessContextRequirements_::Flags_::kNeedSystemTime as u32,
            ) && self.process_context_requirements.is_some()
            {
                data.process_context.systemTime = current_system_time_nanos();
            }
            if process_context_needs(
                self.process_context_requirements,
                IProcessContextRequirements_::Flags_::kNeedTempo as u32,
            ) {
                data.process_context.tempo = self.tempo;
            }
            if process_context_needs(
                self.process_context_requirements,
                IProcessContextRequirements_::Flags_::kNeedTimeSignature as u32,
            ) {
                data.process_context.timeSigNumerator = self.time_sig_numerator;
                data.process_context.timeSigDenominator = self.time_sig_denominator;
            }
            data.process_context.state = process_context_state(
                self.process_context_requirements,
                self.playing,
                PROCESS_CONTEXT_STATE
                    | if self.process_context_requirements.is_some()
                        && process_context_needs(
                            self.process_context_requirements,
                            IProcessContextRequirements_::Flags_::kNeedSystemTime as u32,
                        )
                    {
                        ProcessContext_::StatesAndFlags_::kSystemTimeValid as u32
                    } else {
                        0
                    },
            );

            // Set up process data
            data.process_data.processMode = self.vst_process_mode();
            data.process_data.numSamples = self.block_size as i32;
            data.process_data.symbolicSampleSize = self.sample_size.symbolic();
            data.process_data.processContext = &mut data.process_context;

            // Set up event lists
            data.process_data.inputEvents = self
                .input_events
                .as_com_ref::<IEventList>()
                .map(|ptr| ptr.as_ptr())
                .unwrap_or(ptr::null_mut());
            data.process_data.outputEvents = self
                .output_events
                .as_com_ref::<IEventList>()
                .map(|ptr| ptr.as_ptr())
                .unwrap_or(ptr::null_mut());

            // Set up parameter changes
            data.process_data.inputParameterChanges = data
                .input_param_changes
                .as_com_ref::<IParameterChanges>()
                .map(|ptr| ptr.as_ptr())
                .unwrap_or(ptr::null_mut());
            data.process_data.outputParameterChanges = data
                .output_param_changes
                .as_com_ref::<IParameterChanges>()
                .map(|ptr| ptr.as_ptr())
                .unwrap_or(ptr::null_mut());

            // Prepare buffers
            self.prepare_buffers(&mut data)?;

            self.process_data = Some(data);
            Ok(())
        }
    }

    /// Prepare audio buffers based on plugin bus configuration
    unsafe fn prepare_buffers(&mut self, data: &mut HostProcessData) -> Result<()> {
        let input_bus_count = self.component.getBusCount(kAudio as i32, kInput as i32);
        let output_bus_count = self.component.getBusCount(kAudio as i32, kOutput as i32);
        data.input_bus_buffers.clear();
        data.output_bus_buffers.clear();

        let channel_count = |direction: i32, bus_idx: i32| {
            let mut bus_info: BusInfo = std::mem::zeroed();
            if self
                .component
                .getBusInfo(kAudio as i32, direction, bus_idx, &mut bus_info)
                == kResultOk
            {
                return bus_info.channelCount.max(0);
            }
            // A failed getBusInfo must not remove the slot and shift every later bus index.
            // Fall back to the processor's arrangement, whose set bits define its channels.
            let mut arrangement = 0u64;
            if self
                .processor
                .getBusArrangement(direction, bus_idx, &mut arrangement)
                == kResultOk
            {
                arrangement.count_ones() as i32
            } else {
                0
            }
        };

        for bus_idx in 0..input_bus_count {
            let mut bus: AudioBusBuffers = std::mem::zeroed();
            bus.numChannels = channel_count(kInput as i32, bus_idx);
            data.input_bus_buffers.push(bus);
        }
        for bus_idx in 0..output_bus_count {
            let mut bus: AudioBusBuffers = std::mem::zeroed();
            bus.numChannels = channel_count(kOutput as i32, bus_idx);
            data.output_bus_buffers.push(bus);
        }

        data.process_data.numInputs = data.input_bus_buffers.len() as i32;
        data.process_data.numOutputs = data.output_bus_buffers.len() as i32;

        match &mut data.sample_buffers {
            HostSampleBuffers::F32(samples) => {
                *samples = build_typed_sample_buffers(
                    self.block_size,
                    &data.input_bus_buffers,
                    &self.bus_activation.audio_inputs,
                    &data.output_bus_buffers,
                    &self.bus_activation.audio_outputs,
                );
                for (bus, pointers) in data
                    .input_bus_buffers
                    .iter_mut()
                    .zip(&mut samples.input_channel_ptrs.0)
                {
                    bus.__field0.channelBuffers32 = if pointers.is_empty() {
                        ptr::null_mut()
                    } else {
                        pointers.as_mut_ptr()
                    };
                }
                for (bus, pointers) in data
                    .output_bus_buffers
                    .iter_mut()
                    .zip(&mut samples.output_channel_ptrs.0)
                {
                    bus.__field0.channelBuffers32 = if pointers.is_empty() {
                        ptr::null_mut()
                    } else {
                        pointers.as_mut_ptr()
                    };
                }
            }
            HostSampleBuffers::F64(samples) => {
                *samples = build_typed_sample_buffers(
                    self.block_size,
                    &data.input_bus_buffers,
                    &self.bus_activation.audio_inputs,
                    &data.output_bus_buffers,
                    &self.bus_activation.audio_outputs,
                );
                for (bus, pointers) in data
                    .input_bus_buffers
                    .iter_mut()
                    .zip(&mut samples.input_channel_ptrs.0)
                {
                    bus.__field0.channelBuffers64 = if pointers.is_empty() {
                        ptr::null_mut()
                    } else {
                        pointers.as_mut_ptr()
                    };
                }
                for (bus, pointers) in data
                    .output_bus_buffers
                    .iter_mut()
                    .zip(&mut samples.output_channel_ptrs.0)
                {
                    bus.__field0.channelBuffers64 = if pointers.is_empty() {
                        ptr::null_mut()
                    } else {
                        pointers.as_mut_ptr()
                    };
                }
            }
        }

        data.process_data.inputs = if data.input_bus_buffers.is_empty() {
            ptr::null_mut()
        } else {
            data.input_bus_buffers.as_mut_ptr()
        };
        data.process_data.outputs = if data.output_bus_buffers.is_empty() {
            ptr::null_mut()
        } else {
            data.output_bus_buffers.as_mut_ptr()
        };

        log::debug!(
            "Prepared buffers: {} input buses, {} output buses, {} input channels, {} output channels",
            input_bus_count,
            output_bus_count,
            data.sample_buffers.input_channel_count(),
            data.sample_buffers.output_channel_count()
        );

        Ok(())
    }
}

impl PluginImpl {
    fn capture_native_state_bounded(
        &mut self,
        state_limit: usize,
        buffer_grant: usize,
    ) -> std::result::Result<
        crate::configuration_capture::BoundedConfigurationState,
        crate::configuration_capture::ConfigurationCaptureError,
    > {
        use super::bounded_configuration_stream::ConfigurationCaptureStream;
        use crate::configuration_capture::{
            BoundedConfigurationState, ConfigurationCaptureBuffers,
            ConfigurationCaptureError as CaptureError, ConfigurationStatePart,
        };
        if thread::current().id() != self.control_thread {
            return Err(CaptureError::WrongThread);
        }
        if self
            .pending_processor_parameter_input()
            .map_err(|_| CaptureError::PoisonedBuffer)?
        {
            return Err(CaptureError::PendingParameterDelivery);
        }
        let header = crate::plugin::STATE_SNAPSHOT_HEADER_SIZE;
        if !(header..=crate::plugin::MAX_STATE_SNAPSHOT_BYTES).contains(&state_limit) {
            return Err(CaptureError::InvalidLimit { limit: state_limit });
        }
        let payload_limit = state_limit - header;
        let separate_controller = self.controller.is_some() && !self.single_component;
        let stream_count = if separate_controller { 2 } else { 1 };
        let required = payload_limit
            .checked_mul(stream_count)
            .and_then(|bytes| bytes.checked_add(state_limit))
            .ok_or(CaptureError::ExtentOverflow)?;
        if required > buffer_grant {
            return Err(CaptureError::BufferGrantExceeded {
                required,
                granted: buffer_grant,
            });
        }
        // Reserve every capture/envelope buffer before calling native state code.
        let component_stream = ComWrapper::new(ConfigurationCaptureStream::new(payload_limit)?);
        let controller_stream = if separate_controller {
            Some(ComWrapper::new(ConfigurationCaptureStream::new(
                payload_limit,
            )?))
        } else {
            None
        };
        let mut output = Vec::new();
        output
            .try_reserve_exact(state_limit)
            .map_err(|_| CaptureError::AllocationFailed)?;
        let stream_payload_bytes = component_stream
            .payload_allocation_bytes()?
            .checked_add(match &controller_stream {
                Some(stream) => stream.payload_allocation_bytes()?,
                None => 0,
            })
            .ok_or(CaptureError::ExtentOverflow)?;
        let buffers = ConfigurationCaptureBuffers {
            stream_payload_bytes,
            envelope_capacity_bytes: output.capacity(),
        };
        let actual_peak = buffers
            .peak_buffer_bytes()
            .ok_or(CaptureError::ExtentOverflow)?;
        if actual_peak > buffer_grant {
            return Err(CaptureError::BufferGrantExceeded {
                required: actual_peak,
                granted: buffer_grant,
            });
        }
        self._host_app.register_capture_stream(&component_stream)?;
        if let Some(stream) = controller_stream.as_ref() {
            self._host_app.register_capture_stream(stream)?;
        }
        let component_ptr = component_stream
            .to_com_ptr::<IBStream>()
            .ok_or(CaptureError::StreamInterfaceUnavailable)?;
        let component_result = unsafe { self.component.getState(component_ptr.as_ptr()) };
        let component_len = component_stream.seal()?;
        if component_result == kNotImplemented || component_result == kResultFalse {
            return Err(CaptureError::Unavailable);
        }
        if component_result != kResultOk {
            return Err(CaptureError::NativeFailure {
                part: ConfigurationStatePart::Component,
                result: component_result,
            });
        }
        let mut controller_len = None;
        if let (Some(controller), Some(stream)) = (&self.controller, &controller_stream) {
            stream.set_write_limit(payload_limit - component_len)?;
            let stream_ptr = stream
                .to_com_ptr::<IBStream>()
                .ok_or(CaptureError::StreamInterfaceUnavailable)?;
            let result = unsafe { controller.getState(stream_ptr.as_ptr()) };
            let captured_len = stream.seal()?;
            if result == kResultOk {
                controller_len = Some(captured_len);
            } else if result != kNotImplemented && result != kResultFalse {
                return Err(CaptureError::NativeFailure {
                    part: ConfigurationStatePart::Controller,
                    result,
                });
            }
        }
        crate::plugin::encode_configuration_capture_into(
            &mut output,
            &component_stream,
            component_len,
            controller_stream
                .as_ref()
                .zip(controller_len)
                .map(|(stream, len)| (&**stream, len)),
        )?;
        Ok(BoundedConfigurationState {
            bytes: output,
            buffers,
        })
    }

    fn current_audio_bus_layout(&self) -> Result<AudioBusLayout> {
        process_audio_bus_layout(&self.process_data, &self.bus_activation)
    }
}

impl PluginInternal for PluginImpl {
    fn visit_retained_host_created_storage(
        &mut self,
        visitor: &mut dyn crate::host_storage::HostStorageVisitor,
    ) -> std::result::Result<(), crate::host_storage::HostStorageError> {
        if thread::current().id() != self.control_thread {
            return Err(crate::host_storage::HostStorageError::WrongThread);
        }
        use crate::host_storage::HostStorageRef;
        visitor.visit(HostStorageRef::path(&self.info.path)?)?;
        for text in [
            &self.info.name,
            &self.info.vendor,
            &self.info.version,
            &self.info.category,
            &self.info.uid,
        ] {
            visitor.visit(HostStorageRef::string(text)?)?;
        }
        visitor.visit(HostStorageRef::vector(&self.compatibility)?)?;
        for entry in &self.compatibility {
            visitor.visit(HostStorageRef::string(&entry.new_class_id)?)?;
            visitor.visit(HostStorageRef::vector(&entry.old_class_ids)?)?;
            for class in &entry.old_class_ids {
                visitor.visit(HostStorageRef::string(class)?)?;
            }
        }
        visitor.visit(HostStorageRef::vector(&self.bus_activation.audio_inputs)?)?;
        visitor.visit(HostStorageRef::vector(&self.bus_activation.audio_outputs)?)?;
        visitor.visit(HostStorageRef::vector(&self.bus_activation.event_inputs)?)?;
        visitor.visit(HostStorageRef::vector(&self.bus_activation.event_outputs)?)?;
        visitor.visit(HostStorageRef::vector(&self.active_notes)?)?;
        visitor.visit(HostStorageRef::vector(
            &self.midi_mapping_cache.assignments,
        )?)?;
        visitor.visit(HostStorageRef::vector(&self.program_change_cache)?)?;
        visitor.visit(HostStorageRef::vector(&self.pending_param_changes)?)?;
        let units = self
            .unit_cache
            .lock()
            .map_err(|_| crate::host_storage::HostStorageError::PoisonedOwner)?;
        if let Some(units) = units.as_ref() {
            visitor.visit(HostStorageRef::vector(units)?)?;
            for unit in units {
                visitor.visit(HostStorageRef::string(&unit.name)?)?;
                visitor.visit(HostStorageRef::vector(&unit.programs)?)?;
                for name in &unit.programs {
                    visitor.visit(HostStorageRef::string(name)?)?;
                }
            }
        }
        drop(units);
        if let Some(process) = &self.process_data {
            visitor.visit(HostStorageRef::boxed(process))?;
            process.visit_owned_storage(visitor)?;
        }
        if let Some(connection) = &self.connection {
            connection.visit_owned_storage(visitor)?;
        }
        visitor.visit(HostStorageRef::com(&self.input_events))?;
        self.input_events.visit_owned_storage(visitor)?;
        visitor.visit(HostStorageRef::com(&self.output_events))?;
        self.output_events.visit_owned_storage(visitor)?;
        visitor.visit(HostStorageRef::arc(&self.gui_param_changes_for_host))?;
        {
            let changes = self
                .gui_param_changes_for_host
                .lock()
                .map_err(|_| crate::host_storage::HostStorageError::PoisonedOwner)?;
            visitor.visit(HostStorageRef::vector(&changes)?)?;
        }
        visitor.visit(HostStorageRef::arc(&self.output_param_feedback))?;
        visitor.visit(HostStorageRef::queue(self.output_param_feedback.as_ref()))?;
        visitor.visit(HostStorageRef::queue(&self.deferred_controller_sync))?;
        visitor.visit(HostStorageRef::vector(&self.chunk_events)?)?;
        for event in self.chunk_events.iter().flatten() {
            super::com_implementations::visit_event_payload(event, visitor)?;
        }
        visitor.visit(HostStorageRef::com(&self.plug_frame))?;
        self.plug_frame.visit_owned_storage(visitor)?;
        visitor.visit(HostStorageRef::arc(&self.editor_resize))?;
        visitor.visit(HostStorageRef::arc(&self.output_events_owned))?;
        // The product's retained child has no external output consumer aliases.
        // A library user may create such handles; in that case a complete
        // payload borrow requires an explicit consumer/producer handoff and is
        // refused here instead of inspecting concurrent slots or charging zero.
        let output = Arc::get_mut(&mut self.output_events_owned)
            .ok_or(crate::host_storage::HostStorageError::NonExclusiveSharedOwner)?;
        visitor.visit(HostStorageRef::queue(output))?;
        output.try_for_each_occupied(|event| {
            super::com_implementations::visit_event_payload(event, visitor)
        })?;
        if let Some(handler) = &self.component_handler {
            visitor.visit(HostStorageRef::com(handler))?;
            handler.visit_owned_storage(visitor)?;
        }
        visitor.visit(HostStorageRef::boxed(&self._module))?;
        self._module.visit_owned_storage(visitor)?;
        visitor.visit(HostStorageRef::com(&self._host_app))?;
        self._host_app.visit_progress_storage(visitor)?;
        self._host_app.visit_data_exchange_storage(visitor)?;
        self._host_app.visit_retained_com_storage(visitor)
    }
    fn native_editor_attachment_owner(&self) -> Option<crate::plugin::NativeEditorAttachmentOwner> {
        Some(self.native_editor_owner)
    }

    fn borrow_editor_control(&mut self) -> Result<crate::plugin::PluginEditorControl<'_>> {
        self.ensure_control_thread("borrow native editor control")?;
        Ok(crate::plugin::PluginEditorControl::from_role(Box::new(
            self.editor_fields(),
        )))
    }

    fn can_split_editor_processing(&self) -> bool {
        self.is_active && self.is_processing && self.process_data.is_some()
    }

    fn split_editor_processing(
        &mut self,
    ) -> Result<crate::plugin::PluginEditorProcessingSplit<'_>> {
        self.ensure_control_thread("split editor and processing roles")?;
        if !self.can_split_editor_processing() {
            return Err(Error::NotProcessing);
        }
        let editor = PluginEditorFields {
            controller: &self.controller,
            component_handler: &self.component_handler,
            native_editor_owner: self.native_editor_owner,
            plugin_view: &mut self.plugin_view,
            editor_scale_factor: &mut self.editor_scale_factor,
            plug_frame: &self.plug_frame,
            editor_resize: &self.editor_resize,
            deferred_controller_sync: &self.deferred_controller_sync,
            host_app: &self._host_app,
            control_thread: self.control_thread,
            #[cfg(target_os = "linux")]
            run_loop: &self.run_loop,
        };
        let processor = PluginProcessFields {
            processor: &self.processor,
            is_active: self.is_active,
            is_processing: self.is_processing,
            sample_rate: self.sample_rate,
            block_size: self.block_size,
            process_data: &mut self.process_data,
            bus_activation: &self.bus_activation,
            component_handler: &self.component_handler,
            pending_param_changes: &mut self.pending_param_changes,
            input_events: &self.input_events,
            output_events: &self.output_events,
            chunk_events: &mut self.chunk_events,
            gui_param_changes_for_host: &self.gui_param_changes_for_host,
            output_param_feedback: &self.output_param_feedback,
            output_events_owned: &self.output_events_owned,
            deferred_controller_sync: &self.deferred_controller_sync,
            host_app: &self._host_app,
        };
        Ok(crate::plugin::PluginEditorProcessingSplit::from_roles(
            Box::new(editor),
            Box::new(processor),
        ))
    }

    fn set_process_timeline_context(
        &mut self,
        context: crate::audio::ProcessTimelineContext,
    ) -> Result<()> {
        self.process_fields().set_signal_timeline_context(context)
    }

    fn set_process_musical_timeline_context(
        &mut self,
        context: crate::audio::ProcessTimelineContext,
        reference: crate::audio::ProcessMusicalReference,
    ) -> Result<()> {
        self.process_fields()
            .install_musical_timeline_context(context, reference)
    }

    fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        self.set_parameter_at(id, value, 0)
    }

    fn set_parameter_at(&mut self, id: u32, value: f64, sample_offset: i32) -> Result<()> {
        if self.controller.is_none() {
            return Err(Error::InterfaceError("No controller available".to_string()));
        }
        // VST3 requires both halves: the controller (for GUI/display/formatting) and the
        // processor's input queue (so the change reaches the audio DSP) at `sample_offset` in
        // the next process() block. `queue_processor_parameter_at` owns both.
        self.queue_processor_parameter_at(id, value, sample_offset)
    }

    fn queue_processor_parameter_at(
        &mut self,
        id: u32,
        value: f64,
        sample_offset: i32,
    ) -> Result<()> {
        // Every route into the processor queue also updates the controller, so a value set
        // through `AudioHandle`/`RtControl` (or a mapped MIDI controller) is reflected by the
        // plugin's own editor, `get_parameter`, `format_parameter` and saved state — deferred
        // to the control thread when it arrives from the audio callback.
        self.mirror_parameter_to_controller(id, value);
        if self.pending_param_changes.len() >= MAX_PENDING_PARAM_CHANGES {
            // Only reachable when nothing is draining the queue (the plugin isn't
            // processing), so the dropped change would have arrived as part of a stale flood
            // anyway. The controller half above still ran, so the plugin's own display
            // stays correct.
            self.dropped_param_changes += 1;
            log::warn!(
                "dropping parameter change for {id}, queue full at \
                     {MAX_PENDING_PARAM_CHANGES} (is the plugin processing?); \
                     {} dropped so far",
                self.dropped_param_changes
            );
            return Ok(());
        }
        self.pending_param_changes.push(ParameterChange {
            id,
            value,
            sample_offset,
        });
        Ok(())
    }

    fn set_tempo(&mut self, bpm: f64) -> Result<()> {
        if self
            .process_data
            .as_ref()
            .is_some_and(|data| data.musical_reference.is_some())
        {
            return Err(Error::MusicalTime(MusicalTimeError::AnchoredTempo));
        }
        self.update_tempo(bpm);
        Ok(())
    }

    fn set_time_signature(&mut self, numerator: i32, denominator: i32) -> Result<()> {
        self.update_time_signature(numerator, denominator);
        Ok(())
    }

    fn set_playing(&mut self, playing: bool) -> Result<()> {
        self.update_playing(playing);
        Ok(())
    }

    fn get_parameter(&self, id: u32) -> Result<f64> {
        self.drain_deferred_controller_sync();
        if let Some(ref controller) = self.controller {
            unsafe { Ok(controller.getParamNormalized(id)) }
        } else {
            Err(Error::InterfaceError("No controller available".to_string()))
        }
    }

    fn visit_controller_parameters_bounded(
        &self,
        maximum: usize,
        frame: &mut crate::controller_parameters::ControllerParameterQueryFrame,
        visitor: &mut dyn FnMut(
            usize,
            crate::controller_parameters::ControllerParameterRef<'_>,
        ) -> Result<()>,
    ) -> Result<usize> {
        self.ensure_control_thread("bounded controller observation")?;
        // An observation, never a disguised controller synchronization setter.
        if !self.deferred_controller_sync.is_empty() {
            return Err(Error::Other(
                "deferred controller synchronization is pending".to_owned(),
            ));
        }
        let controller = self
            .controller
            .as_ref()
            .ok_or_else(|| Error::InterfaceError("No controller available".to_owned()))?;
        // SAFETY: this live COM controller belongs to the verified owner thread.
        // Each output pointer addresses a valid fixed SDK slot for the call only.
        unsafe {
            let raw_count = controller.getParameterCount();
            let count = usize::try_from(raw_count).map_err(|_| {
                Error::InvalidParameter("negative native parameter count".to_owned())
            })?;
            if count > maximum {
                return Err(Error::InvalidParameter(
                    "native parameter count exceeds admitted bound".to_owned(),
                ));
            }
            for index in 0..raw_count {
                *frame = crate::controller_parameters::ControllerParameterQueryFrame::default();
                if controller.getParameterInfo(index, &mut frame.info) != kResultOk {
                    return Err(Error::InvalidParameter(
                        "native parameter metadata query refused".to_owned(),
                    ));
                }
                frame.value = controller.getParamNormalized(frame.info.id);
                visitor(index as usize, frame.observation())?;
            }
            if !self.deferred_controller_sync.is_empty() {
                return Err(Error::Other(
                    "controller synchronization arrived during observation".to_owned(),
                ));
            }
            Ok(count)
        }
    }

    fn get_all_parameters(&self) -> Result<Vec<Parameter>> {
        self.drain_deferred_controller_sync();
        let mut params = Vec::new();

        if let Some(ref controller) = self.controller {
            unsafe {
                let count = controller.getParameterCount();

                for i in 0..count {
                    let mut info: ParameterInfo = std::mem::zeroed();
                    if controller.getParameterInfo(i, &mut info) == kResultOk {
                        let param = Parameter {
                            id: info.id,
                            name: crate::internal::utils::vst_string_to_string(&info.title),
                            value: controller.getParamNormalized(info.id),
                            min: 0.0,
                            max: 1.0,
                            default: info.defaultNormalizedValue,
                            unit: crate::internal::utils::vst_string_to_string(&info.units),
                            step_count: info.stepCount,
                            can_automate: (info.flags
                                & ParameterInfo_::ParameterFlags_::kCanAutomate)
                                != 0,
                            is_read_only: (info.flags
                                & ParameterInfo_::ParameterFlags_::kIsReadOnly)
                                != 0,
                            is_bypass: (info.flags & ParameterInfo_::ParameterFlags_::kIsBypass)
                                != 0,
                            flags: info.flags as u32,
                        };
                        params.push(param);
                    }
                }
            }
        }

        Ok(params)
    }

    fn format_parameter(&self, id: u32, normalized: f64) -> Result<String> {
        self.drain_deferred_controller_sync();
        if let Some(ref controller) = self.controller {
            unsafe {
                let mut buf: String128 = std::mem::zeroed();
                if controller.getParamStringByValue(id, normalized, &mut buf) == kResultOk {
                    return Ok(crate::internal::utils::vst_string_to_string(&buf));
                }
            }
            Err(Error::InvalidParameter(format!(
                "Plugin could not format parameter {id}"
            )))
        } else {
            Err(Error::InterfaceError("No controller available".to_string()))
        }
    }

    fn process(&mut self, buffers: &mut AudioBuffers) -> Result<()> {
        self.process_fields()
            .process_buffer_view(&mut CallerAudioBuffers::Flat(buffers))
    }

    fn native_audio_bus_info(&self) -> Result<crate::audio::NativeAudioBusInfoSet> {
        use crate::audio::{AudioBusRole, NativeAudioBusInfo, NativeAudioBusInfoSet};
        const MAX_AUDIO_BUSES: i32 = 32;
        if self.is_processing {
            return Err(Error::Other(
                "cannot query native bus metadata while processing".to_string(),
            ));
        }
        self.ensure_control_thread("native audio-bus metadata")?;
        let component = &self.component;
        let side = |direction: i32| -> Result<Vec<NativeAudioBusInfo>> {
            // SAFETY: live owning-thread component; output storage is valid for the SDK call.
            let count = unsafe { component.getBusCount(kAudio as i32, direction) };
            if !(0..=MAX_AUDIO_BUSES).contains(&count) {
                return Err(Error::InterfaceError(
                    "native audio-bus count exceeds the limit".to_string(),
                ));
            }
            let mut result = Vec::with_capacity(count as usize);
            for index in 0..count {
                // SAFETY: BusInfo is an SDK POD output structure.
                let mut info: BusInfo = unsafe { std::mem::zeroed() };
                // SAFETY: bounded native index and valid BusInfo output pointer.
                let status =
                    unsafe { component.getBusInfo(kAudio as i32, direction, index, &mut info) };
                if status != kResultOk
                    || info.mediaType != kAudio as i32
                    || info.direction != direction
                    || info.channelCount <= 0
                {
                    return Err(Error::InterfaceError(format!(
                        "invalid native audio-bus metadata at index {index}"
                    )));
                }
                let role = match info.busType {
                    value if value == BusTypes_::kMain as i32 => AudioBusRole::Main,
                    value if value == BusTypes_::kAux as i32 => AudioBusRole::Auxiliary,
                    _ => {
                        return Err(Error::InterfaceError(
                            "unknown native audio-bus role".to_string(),
                        ))
                    }
                };
                result.push(NativeAudioBusInfo {
                    role,
                    name: crate::internal::utils::vst_string_to_string(&info.name),
                    channel_count: info.channelCount as usize,
                    default_active: info.flags & BusInfo_::BusFlags_::kDefaultActive as u32 != 0,
                });
            }
            Ok(result)
        };
        Ok(NativeAudioBusInfoSet {
            inputs: side(kInput as i32)?,
            outputs: side(kOutput as i32)?,
        })
    }

    fn native_event_bus_info(&self) -> Result<crate::audio::NativeEventBusInfoSet> {
        if thread::current().id() != self.control_thread {
            return Err(Error::NativeEvent(NativeEventError::WrongThread));
        }
        if self.is_processing {
            return Err(Error::NativeEvent(NativeEventError::QueryWhileProcessing));
        }
        census_native_event_buses(&self.component, &self.bus_activation)
    }

    fn visit_native_event_bus_constraints(
        &self,
        visitor: &mut dyn FnMut(
            crate::audio::BusDirection,
            usize,
            crate::audio::NativeEventBusRole,
            usize,
            bool,
            bool,
        ) -> Result<()>,
    ) -> Result<()> {
        if thread::current().id() != self.control_thread {
            return Err(Error::NativeEvent(NativeEventError::WrongThread));
        }
        if self.is_processing {
            return Err(Error::NativeEvent(NativeEventError::QueryWhileProcessing));
        }
        walk_native_event_bus_constraints(&self.component, &self.bus_activation, visitor)
    }

    fn process_bus_audio_with_note_events(
        &mut self,
        buffers: &mut BusAudioBuffers,
        events: &[PluginEvent],
    ) -> Result<()> {
        self.process_fields()
            .process_checked_note_buffers(buffers, events)
    }

    fn audio_bus_layout(&self) -> Result<AudioBusLayout> {
        self.current_audio_bus_layout()
    }

    fn process_buses(&mut self, buffers: &mut BusAudioBuffers) -> Result<()> {
        validate_process_bus_buffers(&self.process_data, &self.bus_activation, buffers)?;
        self.process_fields()
            .process_buffer_view(&mut CallerAudioBuffers::Buses(buffers))
    }

    fn reconfigure(&mut self, sample_rate: f64, block_size: usize) -> Result<()> {
        if self.pending_processor_parameter_input()? {
            return Err(Error::PendingParameterDelivery);
        }
        if self.is_processing {
            return Err(Error::Other(
                "cannot reconfigure while processing".to_string(),
            ));
        }
        self.bus_activation.event_readiness = None;
        // VST3 requires the component to be inactive when setupProcessing is called.
        let was_active = self.is_active;
        if was_active {
            self.deactivate_for_configuration()?;
        }

        let (old_sr, old_bs) = (self.sample_rate, self.block_size);
        self.sample_rate = sample_rate;
        self.block_size = block_size;

        // Re-run setupProcessing and rebuild process data / buffers for the new size. On
        // failure restore the cached config so it stays consistent with the still-current
        // (previous) process_data — `process_data` is only swapped in on success. The
        // component is left inactive; `start_processing` reactivates and re-runs setup.
        if let Err(e) = self.setup_processing() {
            self.sample_rate = old_sr;
            self.block_size = old_bs;
            return Err(e);
        }

        if was_active {
            let result = self.set_component_active(true);
            if result != kResultOk {
                return Err(Error::Other(format!(
                    "Failed to reactivate after reconfigure: {:#x}",
                    result
                )));
            }
            self.is_active = true;
        }
        Ok(())
    }

    #[allow(clippy::unnecessary_cast)]
    fn set_process_mode(&mut self, mode: crate::plugin::ProcessMode) -> Result<()> {
        if self.pending_processor_parameter_input()? {
            return Err(Error::PendingParameterDelivery);
        }

        if self.is_processing {
            return Err(Error::Other(
                "cannot set process mode while processing".to_string(),
            ));
        }
        if mode == crate::plugin::ProcessMode::Prefetch {
            match self.prefetchable_support {
                Some(value)
                    if value == ePrefetchableSupport_::kIsNeverPrefetchable as u32
                        || value == ePrefetchableSupport_::kIsNotYetPrefetchable as u32 =>
                {
                    return Err(Error::Other(
                        "plugin currently reports that prefetch processing is unsupported"
                            .to_string(),
                    ));
                }
                // A processor without IPrefetchableSupport predates the policy interface. Keep
                // the legacy behaviour and let setupProcessing accept or reject kPrefetch.
                _ => {}
            }
        }
        // VST3 requires the component inactive for setupProcessing; mirror reconfigure.
        let was_active = self.is_active;
        if was_active {
            self.deactivate_for_configuration()?;
        }

        // Store the mode first so setup_processing bakes it into BOTH ProcessSetup and
        // the freshly rebuilt process_data; restore it if setup fails so the cached mode
        // always reflects the last successfully-applied configuration.
        let old_mode = self.process_mode;
        self.process_mode = mode;
        if let Err(e) = self.setup_processing() {
            self.process_mode = old_mode;
            return Err(e);
        }

        if was_active {
            let result = self.set_component_active(true);
            if result != kResultOk {
                return Err(Error::Other(format!(
                    "Failed to reactivate after set_process_mode: {:#x}",
                    result
                )));
            }
            self.is_active = true;
        }
        Ok(())
    }

    fn bus_arrangements(&self) -> Result<crate::audio::BusArrangements> {
        use crate::audio::SpeakerArrangement;
        unsafe {
            let read = |dir: i32| -> Vec<SpeakerArrangement> {
                let count = self.component.getBusCount(kAudio as i32, dir);
                (0..count)
                    .map(|idx| {
                        let mut arr: u64 = 0;
                        self.processor.getBusArrangement(dir, idx, &mut arr);
                        SpeakerArrangement::from_raw(arr)
                    })
                    .collect()
            };
            Ok(crate::audio::BusArrangements {
                inputs: read(kInput as i32),
                outputs: read(kOutput as i32),
            })
        }
    }

    fn set_bus_arrangements(
        &mut self,
        inputs: &[crate::audio::SpeakerArrangement],
        outputs: &[crate::audio::SpeakerArrangement],
    ) -> Result<()> {
        if self.pending_processor_parameter_input()? {
            return Err(Error::PendingParameterDelivery);
        }
        if self.is_processing {
            return Err(Error::Other(
                "cannot set bus arrangements while processing".to_string(),
            ));
        }
        let mut in_raw: Vec<u64> = inputs.iter().map(|a| a.raw()).collect();
        let mut out_raw: Vec<u64> = outputs.iter().map(|a| a.raw()).collect();
        self.bus_activation.event_readiness = None;
        unsafe {
            // VST3 requires the component inactive for setBusArrangements + setupProcessing.
            let was_active = self.is_active;
            if was_active {
                self.deactivate_for_configuration()?;
            }

            let arrangement_result = self.processor.setBusArrangements(
                in_raw.as_mut_ptr(),
                in_raw.len() as i32,
                out_raw.as_mut_ptr(),
                out_raw.len() as i32,
            );
            if arrangement_result == kResultFalse {
                if was_active {
                    let result = self.set_component_active(true);
                    self.is_active = result == kResultOk;
                }
                return Err(Error::Other(
                    "plugin declined the requested bus arrangements".to_string(),
                ));
            }
            if arrangement_result != kResultOk && arrangement_result != kResultTrue {
                if was_active {
                    let result = self.set_component_active(true);
                    self.is_active = result == kResultOk;
                }
                return Err(Error::InterfaceError(format!(
                    "setBusArrangements failed: {arrangement_result:#x}"
                )));
            }

            self.setup_processing()?;

            if was_active {
                let result = self.set_component_active(true);
                if result != kResultOk {
                    return Err(Error::Other(format!(
                        "Failed to reactivate after set_bus_arrangements: {:#x}",
                        result
                    )));
                }
                self.is_active = true;
            }
        }
        Ok(())
    }

    fn set_bus_active(
        &mut self,
        media_type: crate::audio::MediaType,
        direction: crate::audio::BusDirection,
        bus_index: i32,
        active: bool,
    ) -> Result<()> {
        use crate::audio::{BusDirection, MediaType};
        if self.pending_processor_parameter_input()? {
            return Err(Error::PendingParameterDelivery);
        }
        if self.is_processing {
            return Err(Error::Other(
                "cannot activate a bus while processing".to_string(),
            ));
        }
        if media_type == crate::audio::MediaType::Event {
            if thread::current().id() != self.control_thread {
                return Err(Error::NativeEvent(NativeEventError::WrongThread));
            }
            return activate_checked_event_bus(
                &self.component,
                &self._host_app,
                &mut self.bus_activation,
                &mut self.is_active,
                direction,
                bus_index,
                active,
            );
        }
        // VST3 bus activation requires the component inactive (it's a setup-time operation).
        let media = match media_type {
            MediaType::Audio => kAudio as i32,
            MediaType::Event => kEvent as i32,
        };
        let dir = match direction {
            BusDirection::Input => kInput as i32,
            BusDirection::Output => kOutput as i32,
        };
        unsafe {
            let count = self.component.getBusCount(media, dir);
            if bus_index < 0 || bus_index >= count {
                return Err(Error::InvalidParameter(format!(
                    "bus index {bus_index} out of range for {media_type:?} {direction:?} \
                     bus (count {count})"
                )));
            }

            let was_active = self.is_active;
            if was_active {
                self.set_component_active(false);
                self.is_active = false;
            }

            let result = self
                .component
                .activateBus(media, dir, bus_index, active as u8);

            let update_result = if result == kResultOk {
                let states = match direction {
                    BusDirection::Input => &mut self.bus_activation.audio_inputs,
                    BusDirection::Output => &mut self.bus_activation.audio_outputs,
                };
                if let Some(state) = states.get_mut(bus_index as usize) {
                    *state = active;
                }
                if media_type == MediaType::Audio && self.applied_setup.is_some() {
                    self.create_process_data()
                } else {
                    Ok(())
                }
            } else {
                Err(Error::Other(format!(
                    "activateBus failed for {media_type:?} {direction:?} bus {bus_index}: \
                     {result:#x}"
                )))
            };

            if was_active {
                let reactivate = self.set_component_active(true);
                if reactivate != kResultOk {
                    return Err(Error::Other(format!(
                        "Failed to reactivate after set_bus_active: {reactivate:#x}"
                    )));
                }
                self.is_active = true;
            }

            update_result?;
        }
        Ok(())
    }

    fn send_midi_event(&mut self, event: MidiEvent) -> Result<()> {
        self.send_midi_event_at(event, 0)
    }

    fn send_midi_event_at(&mut self, event: MidiEvent, sample_offset: i32) -> Result<()> {
        // Floor to non-negative (the VST3 SDK treats a negative sampleOffset as undefined);
        // `process()` additionally clamps queued offsets to the actual block length.
        let sample_offset = sample_offset.max(0);
        unsafe {
            let mut vst_event: Event = std::mem::zeroed();
            vst_event.busIndex = 0;
            vst_event.sampleOffset = sample_offset;
            vst_event.ppqPosition = 0.0;
            vst_event.flags = Event_::EventFlags_::kIsLive as u16;

            match event {
                MidiEvent::NoteOn {
                    channel,
                    note,
                    velocity,
                } => {
                    let index = channel.as_index() as usize * 128 + note as usize;
                    let released = write_midi_note_on(&mut vst_event, channel, note, velocity);
                    self.ordinary_note_counts[index] = if released {
                        self.ordinary_note_counts[index].saturating_sub(1)
                    } else {
                        self.ordinary_note_counts[index].saturating_add(1)
                    };
                }
                MidiEvent::NoteOff {
                    channel,
                    note,
                    velocity,
                } => {
                    let index = channel.as_index() as usize * 128 + note as usize;
                    self.ordinary_note_counts[index] =
                        self.ordinary_note_counts[index].saturating_sub(1);
                    write_note_off_event(&mut vst_event, channel, note, velocity);
                }
                MidiEvent::ControlChange {
                    channel,
                    controller,
                    value,
                } => {
                    return self.queue_mapped_midi_controller(
                        channel,
                        controller as u16,
                        value as f64 / 127.0,
                        sample_offset,
                    );
                }
                MidiEvent::PitchBend { channel, value } => {
                    return self.queue_mapped_midi_controller(
                        channel,
                        ControllerNumbers_::kPitchBend as u16,
                        value as f64 / 16_383.0,
                        sample_offset,
                    );
                }
                MidiEvent::ChannelAftertouch { channel, pressure } => {
                    return self.queue_mapped_midi_controller(
                        channel,
                        ControllerNumbers_::kAfterTouch as u16,
                        pressure as f64 / 127.0,
                        sample_offset,
                    );
                }
                MidiEvent::PolyAftertouch {
                    channel,
                    note,
                    pressure,
                } => {
                    // Per-note pressure maps to a first-class VST3 poly-pressure event.
                    vst_event.r#type = kPolyPressureEvent as u16;
                    vst_event.__field0.polyPressure.channel = channel.as_index() as i16;
                    vst_event.__field0.polyPressure.pitch = note as i16;
                    vst_event.__field0.polyPressure.pressure = pressure as f32 / 127.0;
                    vst_event.__field0.polyPressure.noteId = -1;
                }
                MidiEvent::ProgramChange { channel, program } => {
                    // Prefer the channel-aware IMidiMapping route. Multi-timbral instruments
                    // commonly expose kCtrlProgramChange there without publishing their patch
                    // banks through IUnitInfo (OsTIrus is one example).
                    let midi = self
                        ._host_app
                        .consumes_plugin_interface(crate::PluginInterface::MidiMapping);
                    let units = self
                        ._host_app
                        .consumes_plugin_interface(crate::PluginInterface::UnitInfo);
                    if !midi && !units {
                        return Err(Error::InterfaceNotConsumed(
                            crate::PluginInterface::MidiMapping,
                        ));
                    }
                    self.service_control_thread_caches();
                    let controller = ControllerNumbers_::kCtrlProgramChange as u16;
                    if midi
                        && self
                            .checked_midi_cc_to_parameter(0, channel.as_index() as i16, controller)?
                            .is_some()
                    {
                        return self.queue_mapped_midi_controller(
                            channel,
                            controller,
                            program as f64 / 127.0,
                            sample_offset,
                        );
                    }

                    // Otherwise use the root unit's IUnitInfo program-change parameter. A MIDI
                    // channel cannot be mapped to a unit generically, so this compatibility path
                    // targets the conventional root unit regardless of channel.
                    if let Some(mapping) = units.then(|| self.cached_program_change(0)).flatten() {
                        let index = program as i32;
                        if index < mapping.program_count {
                            let value = if mapping.program_count > 1 {
                                index as f64 / (mapping.program_count - 1) as f64
                            } else {
                                0.0
                            };
                            self.queue_processor_parameter_at(
                                mapping.param_id,
                                value,
                                sample_offset,
                            )?;
                        }
                    }
                    return Ok(());
                }
            }

            self.input_events.add_raw_event(&vst_event);
        }
        Ok(())
    }

    fn send_plugin_event(&mut self, event: PluginEvent) -> Result<()> {
        if matches!(
            &event.data,
            crate::midi::PluginEventData::NoteExpressionValue { .. }
                | crate::midi::PluginEventData::NoteExpressionText { .. }
                | crate::midi::PluginEventData::NoteExpressionIntValue { .. }
        ) {
            self._host_app
                .require_plugin_interface(crate::PluginInterface::NoteExpressionController)?;
        }
        self.input_events.add_event(event);
        Ok(())
    }

    fn processing_state(&self) -> bool {
        self.is_processing
    }

    fn start_processing(&mut self) -> Result<()> {
        self.bus_activation.event_readiness = None;
        unsafe {
            // The configuration may have moved since the last setup (`set_audio_config` after
            // load, or a device change), and a plugin sizes its DSP buffers when it goes active,
            // from the ProcessSetup it was last given. So apply a changed configuration the way
            // `reconfigure` does — deactivate, set up, reactivate — rather than calling
            // setupProcessing on a running component, which VST3 forbids. An unchanged
            // configuration needs no setup at all: the plugin still has it.
            if self.applied_setup != Some(self.current_setup()) {
                if self.pending_processor_parameter_input()? {
                    return Err(Error::PendingParameterDelivery);
                }
                if self.is_active {
                    self.deactivate_for_configuration()?;
                }
                self.setup_processing()?;
            }

            if !self.is_active {
                self.activate()?;
            }

            // Start processing. `setProcessing` is an optional notification — a plugin
            // may return kNotImplemented (e.g. u-he), which is not an error: it simply
            // doesn't need the start/stop signal and still processes audio normally.
            // Cold owning-thread validation grants only the new checked API.
            // Legacy start retains its historical return semantics.
            let event_readiness = self.checked_event_readiness();
            let result = self.processor.setProcessing(1);
            if result != kResultOk && result != kNotImplemented {
                return Err(Error::Other(format!(
                    "Failed to start processing: {:#x}",
                    result
                )));
            }

            self.is_processing = true;
            self.bus_activation.event_readiness = Some(event_readiness);
            log::debug!("Plugin processing started successfully");
            Ok(())
        }
    }

    fn stop_processing(&mut self) -> Result<()> {
        self.bus_activation.event_readiness = None;
        unsafe {
            if self.is_processing {
                self.processor.setProcessing(0);
                self.is_processing = false;
            }

            if self.is_active {
                self.set_component_active(false);
                self.is_active = false;
            }

            // Nothing can still be sounding, so no note-on is outstanding.
            self.active_notes.clear();

            Ok(())
        }
    }

    fn native_editor_pending_detach(&self) -> Option<crate::error::EditorDetachStage> {
        self.plugin_view.pending_detach()
    }

    fn has_editor(&self) -> bool {
        has_editor_view(self.info.has_gui, &self.plugin_view, &self.controller)
    }

    fn open_editor(&mut self, parent: *mut std::ffi::c_void) -> Result<()> {
        self.editor_fields().open_editor(parent)
    }

    fn close_editor(&mut self) -> Result<()> {
        self.editor_fields().close_editor()
    }

    #[cfg(target_os = "linux")]
    fn service_run_loop(&mut self) {
        self.editor_fields().service_run_loop()
    }

    fn get_editor_size(&self) -> Result<(i32, i32)> {
        query_editor_size(&self.plugin_view, &self.controller)
    }

    fn editor_can_resize(&self) -> bool {
        can_resize_editor(&self.plugin_view, &self.controller)
    }

    fn resize_editor(&mut self, width: i32, height: i32) -> Result<(i32, i32)> {
        self.editor_fields().resize_editor(width, height)
    }

    fn set_editor_scale_factor(&mut self, factor: f32) -> Result<bool> {
        self.editor_fields().set_editor_scale_factor(factor)
    }

    fn get_parameter_changes(&self) -> Vec<(u32, f64)> {
        self.get_parameter_changes()
    }

    fn editor_capture_source_status_since(
        &self,
        revision: u64,
    ) -> Result<Option<crate::plugin::NativeEditorEventSourceStatus>> {
        self.ensure_control_thread("native editor capture source coherence")?;
        let handler = self
            .component_handler
            .as_ref()
            .ok_or_else(|| Error::Other("component handler is unavailable".to_string()))?;
        Ok(handler.editor_capture_source_status_since(revision))
    }

    fn editor_event_source_status(&self) -> Result<crate::plugin::NativeEditorEventSourceStatus> {
        self.ensure_control_thread("native editor source observation")?;
        let handler = self
            .component_handler
            .as_ref()
            .ok_or_else(|| Error::Other("component handler is unavailable".to_string()))?;
        Ok(handler.editor_event_source_status())
    }

    fn pending_processor_parameter_input(&self) -> Result<bool> {
        self.ensure_control_thread("processor input observation")?;
        if !self.pending_param_changes.is_empty() {
            return Ok(true);
        }
        self.process_data.as_ref().map_or(Ok(false), |data| {
            data.input_param_changes.has_active_points()
        })
    }

    fn handoff_initial_parameter_input(
        &mut self,
        cohort: crate::plugin::RestartCohort,
        source: crate::plugin::NativeEditorEventSourceStatus,
        values: &[crate::initial_parameter_input::InitialParameterValue],
        grant: crate::initial_parameter_input::InitialParameterStorage,
    ) -> std::result::Result<(), crate::initial_parameter_input::InitialParameterHandoffError> {
        use crate::initial_parameter_input::InitialParameterHandoffError as Failure;
        if thread::current().id() != self.control_thread {
            return Err(Failure::WrongThread);
        }
        if !self.pending_param_changes.is_empty() {
            return Err(Failure::InputPending);
        }
        if cohort.flags().bits() & 16 != 0
            && (self
                ._host_app
                .consumes_plugin_interface(crate::PluginInterface::MidiMapping)
                || self
                    ._host_app
                    .consumes_plugin_interface(crate::PluginInterface::UnitInfo))
        {
            return Err(Failure::CohortChanged);
        }
        let data = self.process_data.as_ref().ok_or(Failure::Unsupported)?;
        self.component_handler
            .as_ref()
            .ok_or(Failure::Unsupported)?
            .handoff_initial_parameters(&data.input_param_changes, cohort, source, values, grant)
    }

    fn observe_editor_source_bounded(
        &self,
        maximum: usize,
        events: &mut Vec<crate::plugin::NativeEditorEvent>,
        feedback: &mut Vec<(u32, f64)>,
    ) -> Result<crate::plugin::NativeEditorEventSourceStatus> {
        self.ensure_control_thread("bounded native editor source observation")?;
        self.component_handler
            .as_ref()
            .ok_or_else(|| Error::Other("component handler is unavailable".to_owned()))?
            .observe_editor_source_bounded(maximum, events, feedback)
    }

    fn take_editor_events(&mut self) -> Result<crate::plugin::NativeEditorEventDrain> {
        self.ensure_control_thread("native editor event drain")?;
        self.service_control_thread_caches();
        let handler = self
            .component_handler
            .as_ref()
            .ok_or_else(|| Error::Other("component handler is unavailable".to_string()))?;
        Ok(handler.take_editor_events())
    }

    fn take_host_notifications(&mut self) -> Vec<crate::plugin::HostNotification> {
        let mut notifications = self
            .component_handler
            .as_ref()
            .map(|handler| handler.take_host_notifications())
            .unwrap_or_default();
        notifications.extend(self._host_app.take_progress_notifications());
        if notifications
            .iter()
            .any(crate::plugin::HostNotification::invalidates_unit_cache)
        {
            *self
                .unit_cache
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = None;
            // Mark the program-change table stale rather than clearing it: an emptied table
            // silently turns every MIDI ProgramChange into a no-op until something else happens
            // to rebuild it, whereas a stale table still routes to the previous parameter.
            self.dirty_caches.program_change = true;
        }
        self.service_control_thread_caches();
        notifications
    }

    fn take_data_exchange_blocks(&mut self) -> Vec<crate::plugin::DataExchangeBlock> {
        self._host_app.take_data_exchange_blocks()
    }

    fn execute_context_menu_item(&mut self, menu_id: u64, item_id: u32) -> Result<()> {
        self.component_handler
            .as_ref()
            .ok_or_else(|| Error::Other("component handler is unavailable".to_string()))?
            .execute_context_menu_item(menu_id, item_id)
    }

    fn dismiss_context_menu(&mut self, menu_id: u64) -> Result<()> {
        self.component_handler
            .as_ref()
            .ok_or_else(|| Error::Other("component handler is unavailable".to_string()))?
            .dismiss_context_menu(menu_id)
    }

    fn pending_restart_flags(&self) -> crate::plugin::RestartFlags {
        self.component_handler
            .as_ref()
            .map(|handler| handler.pending_restart_flags())
            .unwrap_or_default()
    }

    fn pending_restart_cohort(&self) -> Option<crate::plugin::RestartCohort> {
        self.component_handler
            .as_ref()
            .map(|handler| handler.pending_restart_cohort())
    }

    fn retire_restart_cohort(&mut self, expected: crate::plugin::RestartCohort) -> Result<bool> {
        self.ensure_control_thread("native restart cohort retirement")?;
        Ok(self
            .component_handler
            .as_ref()
            .is_some_and(|handler| handler.retire_restart_cohort(expected)))
    }

    fn claim_retired_restart_serial(&mut self, serial: u32) -> Result<bool> {
        self.ensure_control_thread("native retired restart serial claim")?;
        Ok(self
            .component_handler
            .as_ref()
            .is_some_and(|handler| handler.claim_retired_restart_serial(serial)))
    }

    fn take_restart_flags(&mut self) -> crate::plugin::RestartFlags {
        let flags = self
            .component_handler
            .as_ref()
            .map(|h| h.take_restart_flags())
            .unwrap_or_default();
        let bits = flags.bits();
        // `restartComponent` can arrive on any thread, and rebuilding these tables is a burst of
        // main-thread-domain controller calls (the MIDI map alone is buses × 16 × 130 of them).
        // So record what went stale and let the control thread rebuild.
        if bits & RestartFlags_::kMidiCCAssignmentChanged != 0 {
            self.dirty_caches.midi_mapping = true;
        }
        if bits & (RestartFlags_::kParamIDMappingChanged | RestartFlags_::kParamTitlesChanged) != 0
        {
            self.dirty_caches.midi_mapping = true;
            self.dirty_caches.program_change = true;
        }
        self.service_control_thread_caches();
        flags
    }

    fn service_host_requests(&mut self) -> Result<crate::plugin::RestartFlags> {
        if thread::current().id() != self.control_thread {
            return Err(Error::Other(
                "restart requests must be serviced on the plugin control thread".to_string(),
            ));
        }

        // Keep the original request cohort intact if its required queue/native
        // replacement cannot yet preserve real processor input obligations.
        if self.pending_processor_parameter_input()? {
            return Err(Error::PendingParameterDelivery);
        }
        let flags = self.take_restart_flags();
        if !flags.latency_changed() && !flags.io_changed() {
            return Ok(flags);
        }

        unsafe {
            let was_processing = self.is_processing;
            let was_active = self.is_active;

            if was_processing {
                let result = self.processor.setProcessing(0);
                if result != kResultOk && result != kNotImplemented {
                    return Err(Error::Other(format!(
                        "failed to suspend processing for restartComponent: {result:#x}"
                    )));
                }
                self.is_processing = false;
            }

            if was_active {
                let result = self.set_component_active(false);
                if result != kResultOk {
                    if was_processing {
                        let resume = self.processor.setProcessing(1);
                        self.is_processing = resume == kResultOk || resume == kNotImplemented;
                    }
                    return Err(Error::Other(format!(
                        "failed to deactivate for restartComponent: {result:#x}"
                    )));
                }
                self.is_active = false;
            }

            let apply_result = if flags.io_changed() {
                match Self::activate_default_buses(&self.component) {
                    Ok(activation) => {
                        self.bus_activation = activation;
                        self.negotiate_default_bus_arrangements()
                            .and_then(|()| self.setup_processing())
                    }
                    Err(error) => Err(error),
                }
            } else {
                Ok(())
            };

            let reactivate_result = if was_active { self.activate() } else { Ok(()) };

            let resume_result = if was_processing && reactivate_result.is_ok() {
                let result = self.processor.setProcessing(1);
                if result == kResultOk || result == kNotImplemented {
                    self.is_processing = true;
                    Ok(())
                } else {
                    Err(Error::Other(format!(
                        "failed to resume processing after restartComponent: {result:#x}"
                    )))
                }
            } else {
                Ok(())
            };

            apply_result?;
            reactivate_result?;
            resume_result?;
        }

        Ok(flags)
    }

    fn take_output_events(&self) -> Vec<PluginEvent> {
        let mut out = Vec::new();
        while let Some(event) = self.output_events_owned.pop() {
            out.push(event);
        }
        out
    }

    fn output_midi_handle(&self) -> Option<crate::plugin::OutputMidiConsumer> {
        Some(crate::plugin::OutputMidiConsumer::from_queue(
            self.output_events_owned.clone(),
        ))
    }

    fn output_event_handle(&self) -> Option<crate::plugin::OutputEventConsumer> {
        Some(crate::plugin::OutputEventConsumer::from_queue(
            self.output_events_owned.clone(),
        ))
    }

    fn take_editor_resize_request(&self) -> Option<(i32, i32)> {
        self.editor_resize.lock().ok().and_then(|mut s| s.take())
    }

    fn latency_samples(&self) -> u32 {
        unsafe { self.processor.getLatencySamples() }
    }

    fn tail_samples(&self) -> u32 {
        unsafe { self.processor.getTailSamples() }
    }

    fn midi_cc_to_parameter(&self, bus: i32, channel: i16, cc: u16) -> Option<u32> {
        // Legacy compatibility only; sensitive consumers use checked.
        self.checked_midi_cc_to_parameter(bus, channel, cc)
            .ok()
            .flatten()
    }
    fn checked_midi_cc_to_parameter(&self, bus: i32, channel: i16, cc: u16) -> Result<Option<u32>> {
        self._host_app
            .require_plugin_interface(crate::PluginInterface::MidiMapping)?;
        Ok(self.midi_mapping_cache.get(bus, channel, cc))
    }

    fn note_on(
        &mut self,
        channel: MidiChannel,
        note: u8,
        velocity: u8,
        sample_offset: i32,
    ) -> Result<crate::midi::NoteId> {
        let id = self.next_note_id;
        self.next_note_id = self.next_note_id.wrapping_add(1).max(1);
        // Remember which (channel, pitch) this id stands for. VST3 note-off carries both the
        // noteId *and* the pitch/channel, and plugins that don't track note ids (most non-MPE
        // synths) match the release by pitch — so `note_off` has to reproduce them.
        if self.active_notes.len() >= MAX_TRACKED_NOTES {
            // Full only if a caller started notes it never released (a MIDI panic sends CCs, not
            // note-offs, so entries can also be left behind that way). Evict one in O(1) rather
            // than refusing to track, so new notes keep getting correct releases.
            self.active_notes.swap_remove(0);
        }
        self.active_notes
            .push((id, channel.as_index() as i16, note as i16));
        unsafe {
            let mut ev: Event = std::mem::zeroed();
            ev.busIndex = 0;
            ev.sampleOffset = sample_offset.max(0);
            ev.flags = Event_::EventFlags_::kIsLive as u16;
            ev.r#type = kNoteOnEvent as u16;
            ev.__field0.noteOn.channel = channel.as_index() as i16;
            ev.__field0.noteOn.pitch = note as i16;
            ev.__field0.noteOn.velocity = velocity as f32 / 127.0;
            ev.__field0.noteOn.noteId = id;
            self.input_events.add_raw_event(&ev);
        }
        Ok(crate::midi::NoteId(id))
    }

    fn note_off(&mut self, id: crate::midi::NoteId, sample_offset: i32) -> Result<()> {
        // Recover the note's channel and pitch from the note-on. Without them the event carries
        // pitch 0 on channel 1, which any synth matching releases by pitch ignores — leaving the
        // real note sounding forever.
        let tracked = self
            .active_notes
            .iter()
            .position(|&(tracked_id, _, _)| tracked_id == id.0)
            .map(|i| self.active_notes.swap_remove(i));
        unsafe {
            let mut ev: Event = std::mem::zeroed();
            ev.busIndex = 0;
            ev.sampleOffset = sample_offset.max(0);
            ev.flags = Event_::EventFlags_::kIsLive as u16;
            ev.r#type = kNoteOffEvent as u16;
            ev.__field0.noteOff.noteId = id.0;
            if let Some((_, channel, pitch)) = tracked {
                ev.__field0.noteOff.channel = channel;
                ev.__field0.noteOff.pitch = pitch;
            }
            // A release velocity of 0 is the SDK's own default for "unspecified".
            self.input_events.add_raw_event(&ev);
        }
        Ok(())
    }

    fn send_note_expression(
        &mut self,
        id: crate::midi::NoteId,
        kind: crate::midi::NoteExpressionType,
        value: f64,
        sample_offset: i32,
    ) -> Result<()> {
        self._host_app
            .require_plugin_interface(crate::PluginInterface::NoteExpressionController)?;
        unsafe {
            let mut ev: Event = std::mem::zeroed();
            ev.busIndex = 0;
            ev.sampleOffset = sample_offset.max(0);
            ev.flags = Event_::EventFlags_::kIsLive as u16;
            ev.r#type = kNoteExpressionValueEvent as u16;
            ev.__field0.noteExpressionValue.typeId = kind.type_id();
            ev.__field0.noteExpressionValue.noteId = id.0;
            ev.__field0.noteExpressionValue.value = value.clamp(0.0, 1.0);
            self.input_events.add_raw_event(&ev);
        }
        Ok(())
    }

    fn note_expressions(
        &self,
        bus: i32,
        channel: i16,
    ) -> Result<Vec<crate::midi::NoteExpressionInfo>> {
        self._host_app
            .require_plugin_interface(crate::PluginInterface::NoteExpressionController)?;
        use crate::midi::{NoteExpressionInfo, NoteExpressionType};
        let Some(ctrl) = self
            .controller
            .as_ref()
            .and_then(|c| c.cast::<INoteExpressionController>())
        else {
            return Ok(Vec::new());
        };
        unsafe {
            let count = ctrl.getNoteExpressionCount(bus, channel);
            let mut out = Vec::with_capacity(count.max(0) as usize);
            for i in 0..count {
                let mut info: NoteExpressionTypeInfo = std::mem::zeroed();
                if ctrl.getNoteExpressionInfo(bus, channel, i, &mut info) == kResultOk {
                    use NoteExpressionTypeInfo_::NoteExpressionTypeFlags_ as Flags;
                    let flags = info.flags;
                    out.push(NoteExpressionInfo {
                        kind: NoteExpressionType::from_type_id(info.typeId),
                        title: crate::internal::utils::vst_string_to_string(&info.title),
                        short_title: crate::internal::utils::vst_string_to_string(&info.shortTitle),
                        units: crate::internal::utils::vst_string_to_string(&info.units),
                        default_value: info.valueDesc.defaultValue,
                        min: info.valueDesc.minimum,
                        max: info.valueDesc.maximum,
                        step_count: info.valueDesc.stepCount,
                        is_bipolar: flags & Flags::kIsBipolar as i32 != 0,
                        is_one_shot: flags & Flags::kIsOneShot as i32 != 0,
                        is_absolute: flags & Flags::kIsAbsolute as i32 != 0,
                    });
                }
            }
            Ok(out)
        }
    }

    fn get_units(&self) -> Result<Vec<crate::plugin::PluginUnit>> {
        self._host_app
            .require_plugin_interface(crate::PluginInterface::UnitInfo)?;
        use crate::plugin::PluginUnit;
        if thread::current().id() != self.control_thread {
            return Err(Error::Other(
                "unit metadata must be queried on the plugin control thread".to_string(),
            ));
        }
        if let Some(units) = self
            .unit_cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
            .cloned()
        {
            return Ok(units);
        }
        let Some(ref controller) = self.controller else {
            return Ok(Vec::new());
        };
        // IUnitInfo is optional; plugins without it (no units/program lists) return empty.
        let Some(unit_info) = controller.cast::<IUnitInfo>() else {
            return Ok(Vec::new());
        };
        unsafe {
            // First resolve program lists (id -> program names), then attach to units.
            let mut lists: std::collections::HashMap<i32, Vec<String>> =
                std::collections::HashMap::new();
            let list_count = unit_info.getProgramListCount();
            for i in 0..list_count {
                let mut pl: ProgramListInfo = std::mem::zeroed();
                if unit_info.getProgramListInfo(i, &mut pl) != kResultOk {
                    continue;
                }
                let mut programs = Vec::with_capacity(pl.programCount.max(0) as usize);
                for p in 0..pl.programCount {
                    let mut name: String128 = std::mem::zeroed();
                    let s = if unit_info.getProgramName(pl.id, p, &mut name) == kResultOk {
                        crate::internal::utils::vst_string_to_string(&name)
                    } else {
                        String::new()
                    };
                    programs.push(s);
                }
                lists.insert(pl.id, programs);
            }

            let unit_count = unit_info.getUnitCount();
            let mut units = Vec::with_capacity(unit_count.max(0) as usize);
            for i in 0..unit_count {
                let mut ui: UnitInfo = std::mem::zeroed();
                if unit_info.getUnitInfo(i, &mut ui) != kResultOk {
                    continue;
                }
                let programs = lists.get(&ui.programListId).cloned().unwrap_or_default();
                units.push(PluginUnit {
                    id: ui.id,
                    parent_id: ui.parentUnitId,
                    name: crate::internal::utils::vst_string_to_string(&ui.name),
                    program_list_id: (ui.programListId >= 0).then_some(ui.programListId),
                    programs,
                });
            }
            *self
                .unit_cache
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(units.clone());
            Ok(units)
        }
    }

    fn select_program(&mut self, unit_id: i32, program_index: i32) -> Result<()> {
        self._host_app
            .require_plugin_interface(crate::PluginInterface::UnitInfo)?;
        self.service_control_thread_caches();
        if self.cached_program_change(unit_id).is_none()
            && thread::current().id() == self.control_thread
        {
            self.refresh_program_change_cache();
        }
        let mapping = self.cached_program_change(unit_id).ok_or_else(|| {
            Error::InvalidParameter(format!(
                "unknown unit {unit_id} or no program-change parameter"
            ))
        })?;
        let param_id = mapping.param_id;
        let program_count = mapping.program_count;
        if program_index < 0 || program_index >= program_count {
            return Err(Error::InvalidParameter(format!(
                "program index {program_index} out of range for unit {unit_id} \
                 ({program_count} programs)"
            )));
        }
        // VST3 maps a discrete program list onto the program-change parameter's normalized
        // 0..1 range: index N of a count-C list is N / (C-1). A single-program list maps to 0.
        let normalized = if program_count > 1 {
            program_index as f64 / (program_count - 1) as f64
        } else {
            0.0
        };
        // Drive both halves (controller for display, processor queue for the DSP), exactly as
        // set_parameter does — a program change is just a parameter change in VST3.
        self.set_parameter_at(param_id, normalized, 0)
    }

    fn selected_unit(&self) -> Result<Option<i32>> {
        self._host_app
            .require_plugin_interface(crate::PluginInterface::UnitInfo)?;
        if thread::current().id() != self.control_thread {
            return Err(Error::Other(
                "selected unit must be queried on the plugin control thread".to_string(),
            ));
        }
        let Some(unit_info) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IUnitInfo>())
        else {
            return Ok(None);
        };
        let unit_id = unsafe { unit_info.getSelectedUnit() };
        Ok((unit_id >= 0).then_some(unit_id))
    }

    fn select_unit(&mut self, unit_id: i32) -> Result<()> {
        self._host_app
            .require_plugin_interface(crate::PluginInterface::UnitInfo)?;
        if thread::current().id() != self.control_thread {
            return Err(Error::Other(
                "unit selection must run on the plugin control thread".to_string(),
            ));
        }
        let unit_info = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IUnitInfo>())
            .ok_or_else(|| Error::Other("plugin does not implement IUnitInfo".to_string()))?;
        let result = unsafe { unit_info.selectUnit(unit_id) };
        if result == kResultOk {
            Ok(())
        } else {
            Err(Error::InvalidParameter(format!(
                "plugin rejected unit {unit_id}: {result:#x}"
            )))
        }
    }

    fn program_pitch_names(
        &self,
        program_list_id: i32,
        program_index: i32,
    ) -> Result<Vec<crate::plugin::ProgramPitchName>> {
        self._host_app
            .require_plugin_interface(crate::PluginInterface::UnitInfo)?;
        if program_index < 0 {
            return Err(Error::InvalidParameter(format!(
                "program index must be non-negative, got {program_index}"
            )));
        }
        if thread::current().id() != self.control_thread {
            return Err(Error::Other(
                "program pitch names must be queried on the plugin control thread".to_string(),
            ));
        }
        let Some(unit_info) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IUnitInfo>())
        else {
            return Ok(Vec::new());
        };
        unsafe {
            if unit_info.hasProgramPitchNames(program_list_id, program_index) != kResultTrue {
                return Ok(Vec::new());
            }
            let mut names = Vec::new();
            for midi_pitch in 0_i16..=127 {
                let mut name: String128 = std::mem::zeroed();
                if unit_info.getProgramPitchName(
                    program_list_id,
                    program_index,
                    midi_pitch,
                    &mut name,
                ) == kResultOk
                {
                    names.push(crate::plugin::ProgramPitchName {
                        midi_pitch,
                        name: crate::internal::utils::vst_string_to_string(&name),
                    });
                }
            }
            Ok(names)
        }
    }

    fn get_program_data(
        &self,
        program_list_id: i32,
        program_index: i32,
    ) -> Result<Option<Vec<u8>>> {
        self._host_app
            .require_plugin_interface(crate::PluginInterface::ProgramListData)?;
        if program_index < 0 {
            return Err(Error::InvalidParameter(format!(
                "program index must be non-negative, got {program_index}"
            )));
        }
        self.ensure_control_thread("program data read")?;
        let Some(data_interface) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IProgramListData>())
        else {
            return Ok(None);
        };
        unsafe {
            if data_interface.programDataSupported(program_list_id) != kResultTrue {
                return Ok(None);
            }
            let stream = create_memory_stream_with_metadata(None, StreamStateType::TrackPreset);
            self._host_app.register_memory_stream(&stream)?;
            let stream_ptr = stream
                .as_com_ref::<IBStream>()
                .ok_or_else(|| Error::InterfaceError("failed to create IBStream".to_string()))?;
            let result =
                data_interface.getProgramData(program_list_id, program_index, stream_ptr.as_ptr());
            if result == kResultOk {
                Ok(Some(stream.to_vec()))
            } else {
                Err(Error::Other(format!(
                    "IProgramListData::getProgramData failed: {result:#x}"
                )))
            }
        }
    }

    fn set_program_data(
        &mut self,
        program_list_id: i32,
        program_index: i32,
        data: &[u8],
    ) -> Result<()> {
        self._host_app
            .require_plugin_interface(crate::PluginInterface::ProgramListData)?;
        if program_index < 0 {
            return Err(Error::InvalidParameter(format!(
                "program index must be non-negative, got {program_index}"
            )));
        }
        self.ensure_stream_size(data)?;
        self.ensure_control_thread("program data restore")?;
        let data_interface = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IProgramListData>())
            .ok_or_else(|| {
                Error::Other("plugin does not implement IProgramListData".to_string())
            })?;
        unsafe {
            if data_interface.programDataSupported(program_list_id) != kResultTrue {
                return Err(Error::Other(format!(
                    "plugin does not support data for program list {program_list_id}"
                )));
            }
            let stream = create_memory_stream_from_with_metadata(
                data.to_vec(),
                None,
                StreamStateType::TrackPreset,
            );
            self._host_app.register_memory_stream(&stream)?;
            let stream_ptr = stream
                .as_com_ref::<IBStream>()
                .ok_or_else(|| Error::InterfaceError("failed to create IBStream".to_string()))?;
            let result =
                data_interface.setProgramData(program_list_id, program_index, stream_ptr.as_ptr());
            if result == kResultOk {
                *self
                    .unit_cache
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = None;
                self.refresh_program_change_cache();
                Ok(())
            } else {
                Err(Error::Other(format!(
                    "IProgramListData::setProgramData failed: {result:#x}"
                )))
            }
        }
    }

    fn get_unit_data(&self, unit_id: i32) -> Result<Option<Vec<u8>>> {
        self._host_app
            .require_plugin_interface(crate::PluginInterface::UnitData)?;
        self.ensure_control_thread("unit data read")?;
        let Some(data_interface) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IUnitData>())
        else {
            return Ok(None);
        };
        unsafe {
            if data_interface.unitDataSupported(unit_id) != kResultTrue {
                return Ok(None);
            }
            let stream = create_memory_stream_with_metadata(None, StreamStateType::TrackPreset);
            self._host_app.register_memory_stream(&stream)?;
            let stream_ptr = stream
                .as_com_ref::<IBStream>()
                .ok_or_else(|| Error::InterfaceError("failed to create IBStream".to_string()))?;
            let result = data_interface.getUnitData(unit_id, stream_ptr.as_ptr());
            if result == kResultOk {
                Ok(Some(stream.to_vec()))
            } else {
                Err(Error::Other(format!(
                    "IUnitData::getUnitData failed: {result:#x}"
                )))
            }
        }
    }

    fn set_unit_data(&mut self, unit_id: i32, data: &[u8]) -> Result<()> {
        self._host_app
            .require_plugin_interface(crate::PluginInterface::UnitData)?;
        self.ensure_stream_size(data)?;
        self.ensure_control_thread("unit data restore")?;
        let data_interface = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IUnitData>())
            .ok_or_else(|| Error::Other("plugin does not implement IUnitData".to_string()))?;
        unsafe {
            if data_interface.unitDataSupported(unit_id) != kResultTrue {
                return Err(Error::Other(format!(
                    "plugin does not support data for unit {unit_id}"
                )));
            }
            let stream = create_memory_stream_from_with_metadata(
                data.to_vec(),
                None,
                StreamStateType::TrackPreset,
            );
            self._host_app.register_memory_stream(&stream)?;
            let stream_ptr = stream
                .as_com_ref::<IBStream>()
                .ok_or_else(|| Error::InterfaceError("failed to create IBStream".to_string()))?;
            let result = data_interface.setUnitData(unit_id, stream_ptr.as_ptr());
            if result == kResultOk {
                *self
                    .unit_cache
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = None;
                self.refresh_program_change_cache();
                Ok(())
            } else {
                Err(Error::Other(format!(
                    "IUnitData::setUnitData failed: {result:#x}"
                )))
            }
        }
    }

    fn begin_host_edit(&mut self, parameter_id: u32) -> Result<()> {
        self.ensure_control_thread("host edit begin")?;
        let editing = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IEditControllerHostEditing>())
            .ok_or_else(|| {
                Error::Other("plugin does not implement IEditControllerHostEditing".to_string())
            })?;
        Self::check_controller_result(
            unsafe { editing.beginEditFromHost(parameter_id) },
            "IEditControllerHostEditing::beginEditFromHost",
        )
    }

    fn end_host_edit(&mut self, parameter_id: u32) -> Result<()> {
        self.ensure_control_thread("host edit end")?;
        let editing = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IEditControllerHostEditing>())
            .ok_or_else(|| {
                Error::Other("plugin does not implement IEditControllerHostEditing".to_string())
            })?;
        Self::check_controller_result(
            unsafe { editing.endEditFromHost(parameter_id) },
            "IEditControllerHostEditing::endEditFromHost",
        )
    }

    fn send_midi_learn(&mut self, bus: i32, channel: i16, controller: u16) -> Result<()> {
        if bus < 0 || !(0..16).contains(&i32::from(channel)) || controller > 129 {
            return Err(Error::InvalidParameter(format!(
                "invalid live MIDI controller address ({bus}, {channel}, {controller})"
            )));
        }
        self.ensure_control_thread("MIDI learn notification")?;
        let midi_learn = self
            .controller
            .as_ref()
            .and_then(|edit_controller| edit_controller.cast::<IMidiLearn>())
            .ok_or_else(|| Error::Other("plugin does not implement IMidiLearn".to_string()))?;
        Self::check_controller_result(
            unsafe { midi_learn.onLiveMIDIControllerInput(bus, channel, controller as i16) },
            "IMidiLearn::onLiveMIDIControllerInput",
        )
    }

    fn set_automation_state(&mut self, state: crate::plugin::AutomationState) -> Result<()> {
        use crate::plugin::AutomationState;
        self.ensure_control_thread("automation state update")?;
        let automation = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IAutomationState>())
            .ok_or_else(|| {
                Error::Other("plugin does not implement IAutomationState".to_string())
            })?;
        let state = match state {
            AutomationState::Off => IAutomationState_::AutomationStates_::kNoAutomation,
            AutomationState::Read => IAutomationState_::AutomationStates_::kReadState,
            AutomationState::Write => IAutomationState_::AutomationStates_::kWriteState,
            AutomationState::ReadWrite => IAutomationState_::AutomationStates_::kReadWriteState,
        };
        Self::check_controller_result(
            unsafe { automation.setAutomationState(state) },
            "IAutomationState::setAutomationState",
        )
    }

    fn remap_parameter_id(&self, old_plugin_uid: &str, old_param_id: u32) -> Result<Option<u32>> {
        let old_uid = crate::internal::utils::parse_class_uid(old_plugin_uid).ok_or_else(|| {
            Error::InvalidParameter(
                "plugin UID must contain exactly 32 hexadecimal characters".to_string(),
            )
        })?;
        self.ensure_control_thread("parameter id remapping")?;
        let Some(remapper) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IRemapParamID>())
        else {
            return Ok(None);
        };

        let mut new_param_id = 0;
        let result = unsafe {
            remapper.getCompatibleParamID(&old_uid as *const TUID, old_param_id, &mut new_param_id)
        };
        if result == kResultOk || result == kResultTrue {
            Ok(Some(new_param_id))
        } else if result == kResultFalse || result == kNotImplemented || result == kNoInterface {
            Ok(None)
        } else {
            Err(Error::InterfaceError(format!(
                "IRemapParamID::getCompatibleParamID failed: {result:#x}"
            )))
        }
    }

    fn midi_panic(&mut self) -> Result<()> {
        // Release per-voice notes with their exact ids first.
        for &(note_id, channel, pitch) in self.active_notes.iter() {
            unsafe {
                let mut event: Event = std::mem::zeroed();
                event.busIndex = 0;
                event.sampleOffset = 0;
                event.flags = Event_::EventFlags_::kIsLive as u16;
                event.r#type = kNoteOffEvent as u16;
                event.__field0.noteOff.noteId = note_id;
                event.__field0.noteOff.channel = channel;
                event.__field0.noteOff.pitch = pitch;
                self.input_events.add_raw_event(&event);
            }
        }
        self.active_notes.clear();

        // Ordinary MIDI events use noteId -1, so release every channel/pitch that is active.
        for index in 0..self.ordinary_note_counts.len() {
            if self.ordinary_note_counts[index] == 0 {
                continue;
            }
            let channel = (index / 128) as i16;
            let pitch = (index % 128) as i16;
            unsafe {
                let mut event: Event = std::mem::zeroed();
                event.busIndex = 0;
                event.sampleOffset = 0;
                event.flags = Event_::EventFlags_::kIsLive as u16;
                event.r#type = kNoteOffEvent as u16;
                event.__field0.noteOff.noteId = -1;
                event.__field0.noteOff.channel = channel;
                event.__field0.noteOff.pitch = pitch;
                self.input_events.add_raw_event(&event);
            }
            self.ordinary_note_counts[index] = 0;
        }

        // Core tracked NoteOff does not depend on optional mapping. Only the
        // best-effort panic CC extension is omitted when it is undeclared.
        if !self
            ._host_app
            .consumes_plugin_interface(crate::PluginInterface::MidiMapping)
        {
            return Ok(());
        }
        // Also route the standard panic controllers through IMidiMapping for plugins that
        // explicitly expose them as parameters. Never place legacy-MIDI-out events on input.
        for channel in 0..MIDI_CHANNEL_COUNT {
            let Some(channel) = MidiChannel::from_index(channel as u8) else {
                continue;
            };
            for controller in [123u16, 120, 121] {
                self.queue_mapped_midi_controller(channel, controller, 0.0, 0)?;
            }
        }
        Ok(())
    }

    fn output_channel_count(&self) -> usize {
        unsafe {
            let bus_count = self.component.getBusCount(kAudio as i32, kOutput as i32);
            let mut total = 0usize;
            for i in 0..bus_count {
                if !self
                    .bus_activation
                    .audio_outputs
                    .get(i as usize)
                    .copied()
                    .unwrap_or(false)
                {
                    continue;
                }
                let mut info: BusInfo = std::mem::zeroed();
                if self
                    .component
                    .getBusInfo(kAudio as i32, kOutput as i32, i, &mut info)
                    == kResultOk
                {
                    total += info.channelCount.max(0) as usize;
                } else {
                    let mut arrangement = 0u64;
                    if self
                        .processor
                        .getBusArrangement(kOutput as i32, i, &mut arrangement)
                        == kResultOk
                    {
                        total += arrangement.count_ones() as usize;
                    }
                }
            }
            total
        }
    }

    fn reactivate_configuration_stopped(
        &mut self,
    ) -> std::result::Result<(), crate::configuration_capture::ConfigurationCaptureError> {
        use crate::configuration_capture::{
            ConfigurationCaptureError as CaptureError, ConfigurationLifecyclePhase as Phase,
        };
        if thread::current().id() != self.control_thread {
            return Err(CaptureError::WrongThread);
        }
        if self
            .pending_processor_parameter_input()
            .map_err(|_| CaptureError::PoisonedBuffer)?
        {
            return Err(CaptureError::PendingParameterDelivery);
        }
        // This slice only admits a latency request at the already applied setup.
        // setupProcessing while active is forbidden, and device/mode changes
        // require a separately admitted complete configuration transaction.
        if self.applied_setup != Some(self.current_setup()) {
            return Err(CaptureError::ChangedSetup);
        }
        if self.is_processing {
            let result = unsafe { self.processor.setProcessing(0) };
            if result != kResultOk && result != kNotImplemented {
                return Err(CaptureError::LifecycleFailure {
                    phase: Phase::StopProcessing,
                    result,
                });
            }
            self.is_processing = false;
        }
        if self.is_active {
            let result = self.set_component_active(false);
            if result != kResultOk {
                return Err(CaptureError::LifecycleFailure {
                    phase: Phase::Deactivate,
                    result,
                });
            }
            self.is_active = false;
        }
        let result = self.set_component_active(true);
        if result != kResultOk {
            return Err(CaptureError::LifecycleFailure {
                phase: Phase::Activate,
                result,
            });
        }
        self.is_active = true;
        // Old note state is no longer an admitted continuation of this component.
        self.active_notes.clear();
        Ok(())
    }

    fn save_state_bounded(
        &mut self,
        state_limit: usize,
        buffer_grant: usize,
    ) -> std::result::Result<
        crate::configuration_capture::BoundedConfigurationState,
        crate::configuration_capture::ConfigurationCaptureError,
    > {
        use crate::configuration_capture::ConfigurationCaptureError as CaptureError;
        if thread::current().id() != self.control_thread {
            return Err(CaptureError::WrongThread);
        }
        if self.is_processing {
            return Err(CaptureError::StillProcessing);
        }
        self.capture_native_state_bounded(state_limit, buffer_grant)
    }

    fn capture_author_state_bounded(
        &mut self,
        state_limit: usize,
        buffer_grant: usize,
    ) -> std::result::Result<
        crate::configuration_capture::BoundedConfigurationState,
        crate::configuration_capture::ConfigurationCaptureError,
    > {
        // Full &mut PluginImpl is unavailable while scoped DSP fields are borrowed.
        // getState is a main-thread authored configuration read, not a checkpoint.
        self.capture_native_state_bounded(state_limit, buffer_grant)
    }

    fn save_state(&self) -> Result<Vec<u8>> {
        if self.pending_processor_parameter_input()? {
            return Err(Error::PendingParameterDelivery);
        }

        self.drain_deferred_controller_sync();
        unsafe {
            let component_stream =
                create_memory_stream_with_metadata(None, StreamStateType::Project);
            self._host_app.register_memory_stream(&component_stream)?;
            let component_ptr = component_stream.to_com_ptr::<IBStream>().ok_or_else(|| {
                Error::InterfaceError("Failed to create component state stream".into())
            })?;
            let result = self.component.getState(component_ptr.as_ptr());
            if result != kResultOk {
                return Err(Error::Other(format!(
                    "Plugin does not provide state (getState: {result:#x})"
                )));
            }

            // A single-component plugin's "controller" is the component itself, so asking it for
            // its state returns the blob above a second time: the envelope would carry the same
            // bytes twice, the combined size cap would effectively halve, and `load_state` would
            // apply `setState` twice. `load_state` has the matching guard.
            let controller = if let Some(controller) =
                self.controller.as_ref().filter(|_| !self.single_component)
            {
                let stream = create_memory_stream_with_metadata(None, StreamStateType::Project);
                self._host_app.register_memory_stream(&stream)?;
                let stream_ptr = stream.to_com_ptr::<IBStream>().ok_or_else(|| {
                    Error::InterfaceError("Failed to create controller state stream".into())
                })?;
                let result = controller.getState(stream_ptr.as_ptr());
                if result == kResultOk {
                    Some(stream.to_vec())
                } else if result == kNotImplemented || result == kResultFalse {
                    None
                } else {
                    return Err(Error::Other(format!(
                        "Failed to save controller state (getState: {result:#x})"
                    )));
                }
            } else {
                None
            };

            encode_state_snapshot(&StateSnapshot {
                component: component_stream.to_vec(),
                controller,
            })
        }
    }

    fn load_state_with_context(&mut self, data: &[u8], context: &StateContext) -> Result<()> {
        if self.pending_processor_parameter_input()? {
            return Err(Error::PendingParameterDelivery);
        }

        let snapshot = decode_state_snapshot(data)?;
        self.bus_activation.event_readiness = None;
        let was_processing = self.is_processing;
        let was_active = self.is_active;

        unsafe {
            if was_processing {
                let result = self.processor.setProcessing(0);
                if result != kResultOk && result != kNotImplemented {
                    return Err(Error::Other(format!(
                        "Failed to stop processing before state restore: {result:#x}"
                    )));
                }
                self.is_processing = false;
            }
            if was_active {
                let result = self.set_component_active(false);
                if result != kResultOk {
                    if was_processing {
                        let resumed = self.processor.setProcessing(1);
                        self.is_processing = resumed == kResultOk || resumed == kNotImplemented;
                    }
                    return Err(Error::Other(format!(
                        "Failed to deactivate component before state restore: {result:#x}"
                    )));
                }
                self.is_active = false;
            }

            let apply_result = (|| -> Result<()> {
                let comp_stream = create_state_restore_stream(snapshot.component.clone(), context);
                self._host_app.register_memory_stream(&comp_stream)?;
                let comp_ptr = comp_stream.to_com_ptr::<IBStream>().ok_or_else(|| {
                    Error::InterfaceError("Failed to create component state stream".into())
                })?;
                let result = self.component.setState(comp_ptr.as_ptr());
                if result != kResultOk && result != kNotImplemented {
                    return Err(Error::Other(format!(
                        "Failed to restore component state (setState: {result:#x})"
                    )));
                }

                if !self.single_component {
                    if let Some(controller) = self.controller.as_ref() {
                        let stream =
                            create_state_restore_stream(snapshot.component.clone(), context);
                        self._host_app.register_memory_stream(&stream)?;
                        let stream_ptr = stream.to_com_ptr::<IBStream>().ok_or_else(|| {
                            Error::InterfaceError(
                                "Failed to create controller component-state stream".into(),
                            )
                        })?;
                        let result = controller.setComponentState(stream_ptr.as_ptr());
                        if result != kResultOk && result != kNotImplemented {
                            return Err(Error::Other(format!(
                                "Failed to sync component state to controller \
                                 (setComponentState: {result:#x})"
                            )));
                        }
                    }
                }

                if let (Some(controller), Some(controller_state)) =
                    (self.controller.as_ref(), snapshot.controller.as_ref())
                {
                    let stream = create_state_restore_stream(controller_state.clone(), context);
                    self._host_app.register_memory_stream(&stream)?;
                    let stream_ptr = stream.to_com_ptr::<IBStream>().ok_or_else(|| {
                        Error::InterfaceError("Failed to create controller state stream".into())
                    })?;
                    let result = controller.setState(stream_ptr.as_ptr());
                    if result != kResultOk && result != kNotImplemented {
                        return Err(Error::Other(format!(
                            "Failed to restore controller state (setState: {result:#x})"
                        )));
                    }
                }

                self.pending_param_changes.clear();
                if let Some(process_data) = self.process_data.as_ref() {
                    process_data.input_param_changes.clear_all();
                }
                *self
                    .unit_cache
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = None;
                self.refresh_midi_mapping_cache();
                self.refresh_program_change_cache();
                Ok(())
            })();

            let restore_result = (|| -> Result<()> {
                if was_active {
                    self.activate()?;
                }
                if was_processing {
                    let event_readiness = if apply_result.is_ok() {
                        Some(self.checked_event_readiness())
                    } else {
                        None
                    };
                    let result = self.processor.setProcessing(1);
                    if result != kResultOk && result != kNotImplemented {
                        return Err(Error::Other(format!(
                            "Failed to resume processing after state restore: {result:#x}"
                        )));
                    }
                    self.is_processing = true;
                    self.bus_activation.event_readiness = event_readiness;
                }
                Ok(())
            })();

            match (apply_result, restore_result) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(apply), Ok(())) => Err(apply),
                (Ok(()), Err(restore)) => Err(restore),
                (Err(apply), Err(restore)) => Err(Error::Other(format!(
                    "{apply}; additionally failed to restore plugin lifecycle: {restore}"
                ))),
            }
        }
    }
}

/// Convert a raw VST3 `Event` (as a plugin emits into its output event list) into a safe
/// [`MidiEvent`]. Returns `None` for event types this library doesn't model.
#[cfg(test)]
#[allow(non_upper_case_globals)]
// kNoteOnEvent etc. are VST3 SDK constants
// SDK enum constants (event types, controller numbers) are generated as `i32` on some
// targets (Windows) and `u32` on others (macOS); the `u8` field casts are likewise needed
// where `c_char` is `i8`. We match the `u32` scrutinee against `<const> as u32` and allow
// the cast clippy flags as redundant on the targets where it already matches.
#[allow(clippy::unnecessary_cast)]
pub(crate) fn event_to_midi(e: &Event) -> Option<MidiEvent> {
    unsafe {
        match e.r#type as u32 {
            t if t == kNoteOnEvent as u32 => {
                let n = &e.__field0.noteOn;
                Some(MidiEvent::NoteOn {
                    channel: MidiChannel::from_index(n.channel as u8)?,
                    note: (n.pitch.clamp(0, 127)) as u8,
                    velocity: (n.velocity * 127.0).round().clamp(0.0, 127.0) as u8,
                })
            }
            t if t == kNoteOffEvent as u32 => {
                let n = &e.__field0.noteOff;
                Some(MidiEvent::NoteOff {
                    channel: MidiChannel::from_index(n.channel as u8)?,
                    note: (n.pitch.clamp(0, 127)) as u8,
                    velocity: (n.velocity * 127.0).round().clamp(0.0, 127.0) as u8,
                })
            }
            t if t == kPolyPressureEvent as u32 => {
                let p = &e.__field0.polyPressure;
                Some(MidiEvent::PolyAftertouch {
                    channel: MidiChannel::from_index(p.channel as u8)?,
                    note: (p.pitch.clamp(0, 127)) as u8,
                    pressure: (p.pressure * 127.0).round().clamp(0.0, 127.0) as u8,
                })
            }
            t if t == kLegacyMIDICCOutEvent as u32 => {
                let c = &e.__field0.midiCCOut;
                let channel = MidiChannel::from_index(c.channel as u8)?;
                let value = (c.value as u8) & 0x7F;
                match c.controlNumber as u32 {
                    n if n == ControllerNumbers_::kPitchBend as u32 => Some(MidiEvent::PitchBend {
                        channel,
                        value: (((c.value2 as u16) & 0x7F) << 7) | value as u16,
                    }),
                    n if n == ControllerNumbers_::kAfterTouch as u32 => {
                        Some(MidiEvent::ChannelAftertouch {
                            channel,
                            pressure: value,
                        })
                    }
                    cc if cc < 128 => Some(MidiEvent::ControlChange {
                        channel,
                        controller: cc as u8,
                        value,
                    }),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

impl PluginImpl {
    /// Translate a MIDI controller through the prebuilt IMidiMapping table and queue the
    /// resulting processor automation (which also mirrors the value to the controller, so a
    /// plugin's editor follows CC-driven changes). Unmapped controllers are intentionally
    /// ignored: VST3 does not permit legacy controller-out events on the component's input
    /// event list.
    fn queue_mapped_midi_controller(
        &mut self,
        channel: MidiChannel,
        controller: u16,
        normalized: f64,
        sample_offset: i32,
    ) -> Result<()> {
        // Rebuild the table here if a restart invalidated it off-thread; a no-op on the audio
        // thread, which must never make controller calls.
        self._host_app
            .require_plugin_interface(crate::PluginInterface::MidiMapping)?;
        self.service_control_thread_caches();
        if let Some(id) =
            self.checked_midi_cc_to_parameter(0, channel.as_index() as i16, controller)?
        {
            self.queue_processor_parameter_at(id, normalized, sample_offset)?;
        }
        Ok(())
    }

    #[allow(clippy::unnecessary_cast)]
    unsafe fn activate_default_buses(component: &ComPtr<IComponent>) -> Result<BusActivationState> {
        let read_and_activate = |media: i32, direction: i32| -> Result<Vec<bool>> {
            let count = component.getBusCount(media, direction);
            let mut states = Vec::with_capacity(count.max(0) as usize);
            for index in 0..count {
                let mut info: BusInfo = std::mem::zeroed();
                let active = component.getBusInfo(media, direction, index, &mut info) == kResultOk
                    && (info.flags & BusInfo_::BusFlags_::kDefaultActive as u32) != 0;
                if active {
                    let result = component.activateBus(media, direction, index, 1);
                    if result != kResultOk {
                        return Err(Error::InterfaceError(format!(
                            "activateBus failed for default-active bus \
                             ({media}, {direction}, {index}): {result:#x}"
                        )));
                    }
                }
                states.push(active);
            }
            Ok(states)
        };

        let read_events =
            |direction: crate::audio::BusDirection| -> Result<Vec<EventBusActivation>> {
                let native_direction = event_direction(direction);
                let count = bounded_event_count(
                    direction,
                    component.getBusCount(kEvent as i32, native_direction),
                )?;
                let mut states = Vec::new();
                states
                    .try_reserve_exact(count)
                    .map_err(|_| Error::NativeEvent(NativeEventError::Allocation))?;
                for index in 0..count {
                    let mut info: BusInfo = std::mem::zeroed();
                    let status = component.getBusInfo(
                        kEvent as i32,
                        native_direction,
                        index as i32,
                        &mut info,
                    );
                    let (role, channel_count) =
                        checked_event_bus_info(direction, index as i32, status, &info)?;
                    let active = info.flags & BusInfo_::BusFlags_::kDefaultActive as u32 != 0;
                    if active {
                        let status =
                            component.activateBus(kEvent as i32, native_direction, index as i32, 1);
                        if status != kResultOk {
                            return Err(Error::NativeEvent(NativeEventError::BusMetadata {
                                direction,
                                index: index as i32,
                                result: status,
                            }));
                        }
                    }
                    states.push(EventBusActivation {
                        role,
                        channel_count,
                        active,
                    });
                }
                Ok(states)
            };
        Ok(BusActivationState {
            audio_inputs: read_and_activate(kAudio as i32, kInput as i32)?,
            audio_outputs: read_and_activate(kAudio as i32, kOutput as i32)?,
            event_inputs: read_events(crate::audio::BusDirection::Input)?,
            event_outputs: read_events(crate::audio::BusDirection::Output)?,
            event_readiness: None,
        })
    }

    /// Get or create controller (handles both single-component and separate controller)
    unsafe fn get_or_create_controller(
        component: &ComPtr<IComponent>,
        factory: &ComPtr<IPluginFactory>,
        context: *mut FUnknown,
    ) -> Result<Option<ComPtr<IEditController>>> {
        // First, try to cast component to IEditController (single component)
        if let Some(controller) = component.cast::<IEditController>() {
            log::debug!("Component implements IEditController (single component)");
            return Ok(Some(controller));
        }

        // If not single component, try to get separate controller
        log::debug!("Component is separate from controller, getting controller class ID...");
        let mut controller_cid: [std::os::raw::c_char; 16] = [0; 16];
        let result = component.getControllerClassId(&mut controller_cid);

        if result != kResultOk {
            log::warn!("Failed to get controller class ID: {:#x}", result);
            return Ok(None);
        }

        log::debug!("Got controller class ID, creating controller...");
        let mut controller_ptr: *mut IEditController = ptr::null_mut();
        let create_result = factory.createInstance(
            controller_cid.as_ptr(),
            IEditController::IID.as_ptr() as *const std::os::raw::c_char,
            &mut controller_ptr as *mut _ as *mut _,
        );

        if create_result != kResultOk || controller_ptr.is_null() {
            log::warn!(
                "Failed to create controller: {:#x}, ptr is null: {}",
                create_result,
                controller_ptr.is_null()
            );
            return Ok(None);
        }

        let controller = ComPtr::<IEditController>::from_raw(controller_ptr)
            .ok_or_else(|| Error::InterfaceError("Failed to wrap controller".to_string()))?;

        // Initialize controller with the same host context as the component.
        log::debug!("Initializing controller...");
        let init_result = controller.initialize(context);
        if init_result != kResultOk {
            log::warn!("Failed to initialize controller: {:#x}", init_result);
            return Ok(None);
        }

        log::debug!("Controller created and initialized successfully");
        Ok(Some(controller))
    }

    /// Connect component and controller via IConnectionPoint
    unsafe fn connect_component_and_controller(
        component: &ComPtr<IComponent>,
        controller: &ComPtr<IEditController>,
    ) -> Result<Option<ConnectionPair>> {
        // Try to get connection points
        let comp_cp = component.cast::<IConnectionPoint>();
        let ctrl_cp = controller.cast::<IConnectionPoint>();

        if let (Some(comp_cp), Some(ctrl_cp)) = (comp_cp, ctrl_cp) {
            let connection = ConnectionPair::connect(comp_cp, ctrl_cp);
            if connection.is_some() {
                log::debug!("Components connected successfully");
            } else {
                log::warn!("Component connection proxy not established (continuing)");
            }
            Ok(connection)
        } else {
            log::debug!("Components do not support IConnectionPoint - might be single component");
            Ok(None) // Not an error - single components don't need connection
        }
    }
}

// Both consumers traverse the same live component and retained activation rows.
// `begin` runs only after the native count and retained extent are validated:
// the allocating collector therefore keeps its original reserve-before-get order.
fn walk_native_event_bus_direction<T>(
    component: &ComPtr<IComponent>,
    direction: crate::audio::BusDirection,
    retained: &[EventBusActivation],
    begin: impl FnOnce(usize) -> Result<T>,
    mut row: impl FnMut(
        &mut T,
        usize,
        &BusInfo,
        crate::audio::NativeEventBusRole,
        usize,
        bool,
    ) -> Result<()>,
) -> Result<T> {
    let native_direction = event_direction(direction);
    // SAFETY: same live component; the original caller owns its thread/lifecycle boundary.
    let count = unsafe { component.getBusCount(kEvent as i32, native_direction) };
    let count = bounded_event_count(direction, count)?;
    if retained.len() != count {
        return Err(Error::NativeEvent(NativeEventError::TopologyChanged {
            direction,
        }));
    }
    let mut context = begin(count)?;
    for (index, retained) in retained.iter().enumerate() {
        let mut info: BusInfo = unsafe { std::mem::zeroed() };
        // SAFETY: the validated count bounds this direction-local index; BusInfo
        // is initialized SDK output storage, borrowed only during this callback.
        let status = unsafe {
            component.getBusInfo(kEvent as i32, native_direction, index as i32, &mut info)
        };
        let (role, channel_count) = checked_event_bus_info(direction, index as i32, status, &info)?;
        if role != retained.role || channel_count != retained.channel_count {
            return Err(Error::NativeEvent(NativeEventError::TopologyChanged {
                direction,
            }));
        }
        row(
            &mut context,
            index,
            &info,
            role,
            channel_count,
            retained.active,
        )?;
    }
    Ok(context)
}

fn walk_native_event_bus_constraints(
    component: &ComPtr<IComponent>,
    activation: &BusActivationState,
    visitor: &mut dyn FnMut(
        crate::audio::BusDirection,
        usize,
        crate::audio::NativeEventBusRole,
        usize,
        bool,
        bool,
    ) -> Result<()>,
) -> Result<()> {
    for (direction, retained) in [
        (
            crate::audio::BusDirection::Input,
            activation.event_inputs.as_slice(),
        ),
        (
            crate::audio::BusDirection::Output,
            activation.event_outputs.as_slice(),
        ),
    ] {
        walk_native_event_bus_direction(
            component,
            direction,
            retained,
            |_| Ok(()),
            |_, index, info, role, channel_count, host_active| {
                visitor(
                    direction,
                    index,
                    role,
                    channel_count,
                    info.flags & BusInfo_::BusFlags_::kDefaultActive as u32 != 0,
                    host_active,
                )
            },
        )?;
    }
    Ok(())
}

fn census_native_event_buses(
    component: &ComPtr<IComponent>,
    activation: &BusActivationState,
) -> Result<crate::audio::NativeEventBusInfoSet> {
    let side = |direction, retained: &[EventBusActivation]| {
        walk_native_event_bus_direction(
            component,
            direction,
            retained,
            |count| {
                let mut result = Vec::<crate::audio::NativeEventBusInfo>::new();
                result
                    .try_reserve_exact(count)
                    .map_err(|_| Error::NativeEvent(NativeEventError::Allocation))?;
                Ok(result)
            },
            |result, _, info, role, channel_count, host_active| {
                result.push(crate::audio::NativeEventBusInfo {
                    role,
                    name: crate::internal::utils::vst_string_to_string(&info.name),
                    channel_count,
                    default_active: info.flags & BusInfo_::BusFlags_::kDefaultActive as u32 != 0,
                    host_active,
                });
                Ok(())
            },
        )
    };
    Ok(crate::audio::NativeEventBusInfoSet {
        inputs: side(crate::audio::BusDirection::Input, &activation.event_inputs)?,
        outputs: side(
            crate::audio::BusDirection::Output,
            &activation.event_outputs,
        )?,
    })
}

fn set_native_component_active(
    component: &ComPtr<IComponent>,
    host: &HostApplication,
    active: bool,
) -> tresult {
    if !active {
        host.flush_data_exchange();
        host.set_data_exchange_active(false);
    }
    // SAFETY: same owner's live component; caller enforces the cold thread/lifecycle boundary.
    let result = unsafe { component.setActive(u8::from(active)) };
    if result == kResultOk || result == kResultTrue {
        host.set_data_exchange_active(active);
    } else if !active {
        host.set_data_exchange_active(true);
    }
    result
}

fn activate_checked_event_bus(
    component: &ComPtr<IComponent>,
    host: &HostApplication,
    activation: &mut BusActivationState,
    is_active: &mut bool,
    direction: crate::audio::BusDirection,
    index: i32,
    active: bool,
) -> Result<()> {
    // A setter attempt invalidates this capability; the census API itself is pure.
    activation.event_readiness = None;
    walk_native_event_bus_constraints(component, activation, &mut |_, _, _, _, _, _| Ok(()))?;
    let states = match direction {
        crate::audio::BusDirection::Input => &mut activation.event_inputs,
        crate::audio::BusDirection::Output => &mut activation.event_outputs,
    };
    // This is the SAME count matched by the complete census, not a second
    // untrusted getBusCount which could widen the accepted index afterwards.
    let count = states.len();
    let state = usize::try_from(index)
        .ok()
        .and_then(|index| states.get_mut(index))
        .ok_or(Error::NativeEvent(NativeEventError::BusIndex {
            direction,
            index,
            count,
        }))?;
    let was_active = *is_active;
    if was_active {
        let result = set_native_component_active(component, host, false);
        if result != kResultOk {
            return Err(Error::NativeEvent(NativeEventError::ComponentActivation {
                active: false,
                result,
            }));
        }
        *is_active = false;
    }
    // SAFETY: index is bounded by the validated same-owner topology; component is inactive.
    let result = unsafe {
        component.activateBus(
            kEvent as i32,
            event_direction(direction),
            index,
            u8::from(active),
        )
    };
    let operation = if result == kResultOk {
        state.active = active;
        Ok(())
    } else {
        Err(Error::NativeEvent(NativeEventError::BusActivation {
            direction,
            index,
            result,
        }))
    };
    let resume = if was_active {
        let result = set_native_component_active(component, host, true);
        if result == kResultOk {
            *is_active = true;
            Ok(())
        } else {
            Err(Error::NativeEvent(NativeEventError::ComponentActivation {
                active: true,
                result,
            }))
        }
    } else {
        Ok(())
    };
    // Preserve the first operation failure even if lifecycle restoration also failed.
    operation.and(resume)
}

fn event_direction(direction: crate::audio::BusDirection) -> i32 {
    match direction {
        crate::audio::BusDirection::Input => kInput as i32,
        crate::audio::BusDirection::Output => kOutput as i32,
    }
}

fn bounded_event_count(direction: crate::audio::BusDirection, count: i32) -> Result<usize> {
    if !(0..=crate::audio::MAX_NATIVE_EVENT_BUSES as i32).contains(&count) {
        return Err(Error::NativeEvent(NativeEventError::BusCount {
            direction,
            count,
        }));
    }
    Ok(count as usize)
}

fn checked_event_bus_info(
    direction: crate::audio::BusDirection,
    index: i32,
    result: i32,
    info: &BusInfo,
) -> Result<(crate::audio::NativeEventBusRole, usize)> {
    use crate::audio::NativeEventBusRole;
    let role = match info.busType {
        value if value == BusTypes_::kMain as i32 => Some(NativeEventBusRole::Main),
        value if value == BusTypes_::kAux as i32 => Some(NativeEventBusRole::Auxiliary),
        _ => None,
    };
    if result != kResultOk
        || info.mediaType != kEvent as i32
        || info.direction != event_direction(direction)
        || info.channelCount < 0
        || role.is_none()
    {
        return Err(Error::NativeEvent(NativeEventError::BusMetadata {
            direction,
            index,
            result,
        }));
    }
    Ok((role.expect("validated role"), info.channelCount as usize))
}

/// Map a sample offset within the caller's block onto one chunk of it.
///
/// Returns the offset rebased to the chunk (`0..frames`), or `None` when the offset belongs to
/// a different chunk. Anything scheduled past the end of the caller's block lands at the end of
/// the final chunk rather than being dropped — a note scheduled near `block_size` still sounds
/// when the device hands over a shorter block.
fn chunk_offset(
    sample_offset: i32,
    chunk_start: usize,
    frames: usize,
    is_last: bool,
) -> Option<i32> {
    let start = chunk_start as i64;
    let end = start + frames as i64;
    // A negative offset is undefined in VST3; treat it as the start of the block.
    let offset = i64::from(sample_offset.max(0));
    if offset < start {
        None
    } else if offset < end {
        Some((offset - start) as i32)
    } else if is_last {
        Some(frames.saturating_sub(1) as i32)
    } else {
        None
    }
}

/// Stage the events belonging to one chunk into the plugin-facing event list, rebasing each
/// offset to the chunk start (see [`chunk_offset`]). Replaces whatever the list held, so each
/// chunk of a split block sees exactly its own events, in order and with their spacing intact.
fn stage_chunk_events(
    queued: &mut [Option<PluginEvent>],
    list: &HostEventList,
    chunk_start: usize,
    frames: usize,
    is_last: bool,
) {
    list.reset_with(queued.iter_mut().filter_map(|slot| {
        let offset = chunk_offset(slot.as_ref()?.sample_offset, chunk_start, frames, is_last)?;
        slot.take().map(|mut event| {
            event.sample_offset = offset;
            event
        })
    }));
}

/// Ordered teardown for a component that has been initialized but whose `PluginImpl` doesn't
/// exist yet.
///
/// Between `IComponent::initialize` returning success and the finished `PluginImpl`, `load` can
/// still fail — and by then the plugin may have spawned threads and registered callbacks.
/// Releasing its interfaces and unloading the module without `terminate()` leaves that code
/// running inside memory the unload is about to unmap. This guard runs the same sequence
/// `Drop for PluginImpl` does, unless [`Self::disarm`] hands the job to the built plugin.
///
/// It holds its own references (COM refcounts), so `load` keeps using the originals as it
/// builds; the extra references are released when the guard drops either way.
struct InitializedComponent {
    component: ComPtr<IComponent>,
    controller: Option<ComPtr<IEditController>>,
    single_component: bool,
    connection: Option<ConnectionPair>,
    armed: bool,
}

impl InitializedComponent {
    fn new(component: ComPtr<IComponent>) -> Self {
        Self {
            component,
            controller: None,
            single_component: false,
            connection: None,
            armed: true,
        }
    }

    /// Record the controller so a later failure disconnects and terminates it too.
    fn attach_controller(
        &mut self,
        controller: Option<ComPtr<IEditController>>,
        single_component: bool,
    ) {
        self.controller = controller;
        self.single_component = single_component;
    }

    fn attach_connection(&mut self, connection: Option<ConnectionPair>) {
        self.connection = connection;
    }

    fn take_connection(&mut self) -> Option<ConnectionPair> {
        self.connection.take()
    }

    /// Cancel the teardown: the finished `PluginImpl` owns the lifecycle from here.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for InitializedComponent {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        log::debug!("Plugin load failed after initialize; terminating the component");
        unsafe {
            terminate_component(
                &self.component,
                self.controller.as_ref(),
                self.single_component,
                self.connection.as_ref(),
            );
        }
    }
}

/// Disconnect and terminate a live component/controller pair, in VST3 order.
///
/// The mirror of the load sequence: break the component↔controller connection, terminate the
/// controller, then terminate the component. Many plugins (dual-component ones especially) rely
/// on `terminate()` to break that link before release and crash without it. A single-component
/// plugin is one object exposed as both halves, so it has no connection pair and is terminated
/// once, as the component.
unsafe fn terminate_component(
    component: &ComPtr<IComponent>,
    controller: Option<&ComPtr<IEditController>>,
    single_component: bool,
    connection: Option<&ConnectionPair>,
) {
    if let Some(connection) = connection {
        connection.disconnect();
    }
    if let Some(controller) = controller {
        if !single_component {
            controller.terminate();
        }
    }
    component.terminate();
}

impl Drop for PluginImpl {
    fn drop(&mut self) {
        // VST3 teardown order: detach the editor, stop processing, deactivate the component,
        // disconnect the component/controller connection points, terminate the controller and the
        // component, then drop the COM references. Many plugins (dual-component ones especially)
        // rely on `terminate()` to break the controller↔component link before release and crash
        // without it.
        //
        // The editor goes first. A view that is still `attached()` when the controller is
        // terminated — and then merely Released as a field, without `removed()` — leaves the
        // plugin's platform frame and its idle timer alive, pointing at a host window the caller
        // is about to destroy. `open_editor` and `close_editor` pair attach/removed correctly, but
        // a host that drops the plugin with the editor open (or is unwound past its own close)
        // never reaches `close_editor`, so do it here too. An isolated product
        // must explicitly close before dropping this owner and terminate its child
        // without native destruction if removal remains unresolved. Drop cannot
        // supply successful removal evidence for a failed native callback.
        if let Err(error) = PluginInternal::close_editor(self) {
            log::error!("native owner dropped after unresolved editor removal: {error}");
        }

        let _ = self.stop_processing();
        self._host_app.shutdown_data_exchange();

        unsafe {
            if self.is_active {
                self.set_component_active(false);
                self.is_active = false;
            }

            terminate_component(
                &self.component,
                self.controller.as_ref(),
                self.single_component,
                self.connection.as_ref(),
            );
        }
    }
}

/// The `ProcessContext.state` flags the host advertises each block: transport playing, with
/// a valid tempo, time signature, continuous sample time, and musical (quarter-note)
/// playhead. The last two are essential — without `kContTimeValid`/`kProjectTimeMusicValid`
/// a spec-conformant plugin treats the advancing `continousTimeSamples`/`projectTimeMusic`
/// (see [`advance_process_context`]) as invalid and ignores it. The `as u32` cast is needed
/// because the `StatesAndFlags_` constants are generated as `i32` on some targets (Windows)
/// and `u32` on others (macOS).
#[allow(clippy::unnecessary_cast)] // the `as u32` is needed where the constants are i32 (Windows)
const PROCESS_CONTEXT_STATE: u32 = (ProcessContext_::StatesAndFlags_::kPlaying
    | ProcessContext_::StatesAndFlags_::kTempoValid
    | ProcessContext_::StatesAndFlags_::kTimeSigValid
    | ProcessContext_::StatesAndFlags_::kContTimeValid
    | ProcessContext_::StatesAndFlags_::kProjectTimeMusicValid)
    as u32;

/// The `kPlaying` bit on its own, factored out of [`PROCESS_CONTEXT_STATE`] so the playing
/// state can be toggled at runtime without disturbing the validity flags.
#[allow(clippy::unnecessary_cast)] // the `as u32` is needed where the constant is i32 (Windows)
const PROCESS_CONTEXT_PLAYING: u32 = ProcessContext_::StatesAndFlags_::kPlaying as u32;

#[allow(clippy::unnecessary_cast)]
fn process_context_needs(requirements: Option<u32>, flag: u32) -> bool {
    match requirements {
        Some(requirements) => requirements & flag != 0,
        None => true,
    }
}

/// Advertise only requested fields with supplied values. Processors without
/// IProcessContextRequirements receive all available fields, never invented facts.
#[allow(clippy::unnecessary_cast)]
fn process_context_state(requirements: Option<u32>, playing: bool, available: u32) -> u32 {
    let Some(requirements) = requirements else {
        return if playing {
            available | PROCESS_CONTEXT_PLAYING
        } else {
            available & !PROCESS_CONTEXT_PLAYING
        };
    };

    use IProcessContextRequirements_::Flags_ as R;
    use ProcessContext_::StatesAndFlags_ as S;
    let mappings = [
        (R::kNeedSystemTime as u32, S::kSystemTimeValid as u32),
        (
            R::kNeedContinousTimeSamples as u32,
            S::kContTimeValid as u32,
        ),
        (
            R::kNeedProjectTimeMusic as u32,
            S::kProjectTimeMusicValid as u32,
        ),
        (R::kNeedBarPositionMusic as u32, S::kBarPositionValid as u32),
        (R::kNeedCycleMusic as u32, S::kCycleValid as u32),
        (R::kNeedSamplesToNextClock as u32, S::kClockValid as u32),
        (R::kNeedTempo as u32, S::kTempoValid as u32),
        (R::kNeedTimeSignature as u32, S::kTimeSigValid as u32),
        (R::kNeedChord as u32, S::kChordValid as u32),
        (R::kNeedFrameRate as u32, S::kSmpteValid as u32),
    ];
    let mut state = mappings
        .iter()
        .filter(|(required, valid)| requirements & required != 0 && available & valid != 0)
        .fold(0, |state, (_, valid)| state | valid);
    if playing && requirements & R::kNeedTransportState as u32 != 0 {
        state |= S::kPlaying as u32;
    }
    state
}

fn current_system_time_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

/// Prove the complete half-open sample window before consuming any process input.
/// Continuous time is meaningful only when its optional validity bit is present.
fn checked_process_sample_end(ctx: &ProcessContext, frames: usize) -> Result<(i64, Option<i64>)> {
    let invalid = NativeEventError::InvalidFrames;
    let frames = i64::try_from(frames).map_err(|_| Error::NativeEvent(invalid))?;
    let project = ctx
        .projectTimeSamples
        .checked_add(frames)
        .ok_or(Error::NativeEvent(invalid))?;
    let continuous = if ctx.state & ProcessContext_::StatesAndFlags_::kContTimeValid as u32 != 0 {
        Some(
            ctx.continousTimeSamples
                .checked_add(frames)
                .ok_or(Error::NativeEvent(invalid))?,
        )
    } else {
        None
    };
    Ok((project, continuous))
}

fn preflight_musical_window(
    context: &ProcessContext,
    reference: Option<crate::audio::ProcessMusicalReference>,
    prepared_rate: f64,
    frames: usize,
    prepared_block: usize,
) -> Result<()> {
    let Some(reference) = reference else {
        return Ok(());
    };
    if !prepared_rate.is_finite()
        || !(1.0..=f64::from(u32::MAX)).contains(&prepared_rate)
        || prepared_rate.fract() != 0.0
        || context.sampleRate != prepared_rate
    {
        return Err(Error::MusicalTime(MusicalTimeError::SampleRate));
    }
    let rate = prepared_rate as u32;
    let step = prepared_block.max(1);
    let mut offset = 0;
    loop {
        let start = context
            .projectTimeSamples
            .checked_add(
                i64::try_from(offset)
                    .map_err(|_| Error::NativeEvent(NativeEventError::InvalidFrames))?,
            )
            .ok_or(Error::NativeEvent(NativeEventError::InvalidFrames))?;
        reference
            .quarter_notes_at(start, rate)
            .map_err(Error::MusicalTime)?;
        let chunk = (frames - offset).min(step);
        let end = start
            .checked_add(
                i64::try_from(chunk)
                    .map_err(|_| Error::NativeEvent(NativeEventError::InvalidFrames))?,
            )
            .ok_or(Error::NativeEvent(NativeEventError::InvalidFrames))?;
        reference
            .quarter_notes_at(end, rate)
            .map_err(Error::MusicalTime)?;
        offset += chunk;
        if offset == frames {
            return Ok(());
        }
    }
}

/// Advance the transport in a `ProcessContext` by `frames` samples after a processed block.
/// Keeps `continousTimeSamples`/`projectTimeSamples` (and the musical playhead derived from
/// the current tempo) moving so tempo-synced plugins don't see a frozen time-0.
#[allow(clippy::unnecessary_cast)]
fn advance_process_context(
    ctx: &mut ProcessContext,
    transport_tempo: f64,
    anchored: Option<(crate::audio::ProcessMusicalReference, u32)>,
    frames: usize,
) -> Result<()> {
    use ProcessContext_::StatesAndFlags_ as S;
    let (project, continuous) = checked_process_sample_end(ctx, frames)?;
    let musical_position = if let Some((reference, rate)) = anchored {
        if ctx.sampleRate != f64::from(rate) {
            return Err(Error::MusicalTime(MusicalTimeError::SampleRate));
        }
        Some(
            reference
                .quarter_notes_at(project, rate)
                .map_err(Error::MusicalTime)?,
        )
    } else {
        None
    };
    ctx.projectTimeSamples = project;
    if let Some(continuous) = continuous {
        ctx.continousTimeSamples = continuous;
    }
    if ctx.state & S::kSystemTimeValid as u32 != 0 && ctx.sampleRate > 0.0 {
        let nanos = (frames as f64 * 1_000_000_000.0 / ctx.sampleRate).round() as i64;
        ctx.systemTime = ctx.systemTime.saturating_add(nanos);
    }
    if let Some(position) = musical_position {
        ctx.projectTimeMusic = position;
    } else if ctx.state & S::kProjectTimeMusicValid as u32 != 0 && ctx.sampleRate > 0.0 {
        // Quarter notes elapsed = seconds * (BPM / 60).
        let secs = ctx.projectTimeSamples as f64 / ctx.sampleRate;
        ctx.projectTimeMusic = secs * (transport_tempo / 60.0);
    }
    Ok(())
}

#[cfg(test)]
mod process_buffer_tests {
    use super::*;

    fn buses(channels: &[i32]) -> Vec<AudioBusBuffers> {
        channels
            .iter()
            .map(|&num_channels| {
                let mut bus: AudioBusBuffers = unsafe { std::mem::zeroed() };
                bus.numChannels = num_channels;
                bus
            })
            .collect()
    }

    /// An inactive bus still gets a full channel-pointer array — the spec requires the array
    /// itself for every bus and only permits the *addresses* in it to be null. Sample storage is
    /// allocated for active channels only.
    #[test]
    fn typed_buffers_give_inactive_buses_an_array_of_null_channel_pointers() {
        let inputs = buses(&[2, 1, 4]);
        let outputs = buses(&[2]);
        let samples =
            build_typed_sample_buffers::<f64>(32, &inputs, &[true, false, true], &outputs, &[true]);

        assert_eq!(samples.inputs.len(), 6);
        assert_eq!(
            samples
                .input_channel_ptrs
                .0
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            [2, 1, 4]
        );
        assert!(samples.input_channel_ptrs.0[1].iter().all(|p| p.is_null()));
        assert!(samples.input_channel_ptrs.0[0]
            .iter()
            .chain(&samples.input_channel_ptrs.0[2])
            .all(|p| !p.is_null()));
        assert_eq!(samples.outputs.len(), 2);
    }

    #[test]
    fn sample64_storage_converts_to_and_from_the_public_f32_buffers() {
        let mut storage = HostSampleBuffers::F64(build_typed_sample_buffers(
            4,
            &buses(&[1]),
            &[true],
            &buses(&[1]),
            &[true],
        ));
        storage.copy_inputs_from(&[vec![0.25, -0.5, 1.0, 0.0]], 0, 4);
        let HostSampleBuffers::F64(samples) = &mut storage else {
            unreachable!()
        };
        assert_eq!(samples.inputs[0], [0.25, -0.5, 1.0, 0.0]);
        samples.outputs[0].copy_from_slice(&[0.5, -0.25, 2.0, 0.0]);
        let mut outputs = vec![vec![0.0; 4]];
        let output_buses = buses(&[1]);
        storage.copy_outputs_to(&mut outputs, 0, 4, &output_buses, &[true]);
        assert_eq!(outputs[0], [0.5, -0.25, 2.0, 0.0]);
    }

    #[test]
    fn bus_buffers_route_active_buses_without_flattening_inactive_slots() {
        let input_buses = buses(&[2, 1, 1]);
        let output_buses = buses(&[1, 2, 1]);
        let active = [true, false, true];
        let mut storage = HostSampleBuffers::F32(build_typed_sample_buffers(
            4,
            &input_buses,
            &active,
            &output_buses,
            &active,
        ));
        let inputs = vec![
            AudioBusBuffer {
                active: true,
                channels: vec![vec![1.0; 4], vec![2.0; 4]],
            },
            AudioBusBuffer {
                active: false,
                channels: vec![vec![99.0; 4]],
            },
            AudioBusBuffer {
                active: true,
                channels: vec![vec![3.0; 4]],
            },
        ];
        storage.copy_inputs_from_buses(&inputs, 0, 4, &input_buses, &active);
        let HostSampleBuffers::F32(samples) = &mut storage else {
            unreachable!()
        };
        assert_eq!(samples.inputs[0], [1.0; 4]);
        assert_eq!(samples.inputs[1], [2.0; 4]);
        assert_eq!(samples.inputs[2], [3.0; 4]);

        samples.outputs[0].fill(4.0);
        samples.outputs[1].fill(5.0);
        let mut outputs = vec![
            AudioBusBuffer::new(1, 4, true),
            AudioBusBuffer::new(2, 4, false),
            AudioBusBuffer::new(1, 4, true),
        ];
        outputs[1].channels[0].fill(99.0);
        outputs[1].channels[1].fill(99.0);
        storage.copy_outputs_to_buses(&mut outputs, 0, 4, &output_buses, &active);
        assert_eq!(outputs[0].channels[0], [4.0; 4]);
        assert_eq!(outputs[1].channels[0], [0.0; 4]);
        assert_eq!(outputs[1].channels[1], [0.0; 4]);
        assert_eq!(outputs[2].channels[0], [5.0; 4]);
    }

    #[test]
    fn silence_flags_are_computed_per_bus_and_inactive_slots_are_silent() {
        let mut storage = HostSampleBuffers::F32(build_typed_sample_buffers(
            4,
            &buses(&[2, 1]),
            &[true, false],
            &buses(&[2]),
            &[true],
        ));
        storage.copy_inputs_from(&[vec![0.0, 0.0, 0.0, 0.0], vec![0.0, 0.5, 0.0, 0.0]], 0, 4);
        let mut input_buses = buses(&[2, 1]);
        storage.update_input_silence_flags(&mut input_buses, &[true, false], 4);
        assert_eq!(input_buses[0].silenceFlags, 0b01);
        assert_eq!(input_buses[1].silenceFlags, 0b1);
    }

    #[test]
    fn plugin_output_silence_flags_zero_only_the_declared_channels() {
        let mut storage = HostSampleBuffers::F32(build_typed_sample_buffers(
            4,
            &[],
            &[],
            &buses(&[2]),
            &[true],
        ));
        let HostSampleBuffers::F32(samples) = &mut storage else {
            unreachable!()
        };
        samples.outputs[0].copy_from_slice(&[0.5, 0.5, 0.5, 0.5]);
        samples.outputs[1].copy_from_slice(&[0.25, 0.25, 0.25, 0.25]);
        let mut output_buses = buses(&[2]);
        output_buses[0].silenceFlags = 0b01;
        storage.update_output_silence_flags(&mut output_buses, &[true], 4);
        assert_eq!(output_buses[0].silenceFlags, 0b01);

        let mut outputs = vec![vec![9.0; 4], vec![9.0; 4]];
        storage.copy_outputs_to(&mut outputs, 0, 4, &output_buses, &[true]);
        assert_eq!(outputs[0], [0.0; 4]);
        assert_eq!(outputs[1], [0.25; 4]);
    }

    #[test]
    fn actually_zero_output_is_marked_silent_even_when_plugin_omits_the_flag() {
        let mut storage = HostSampleBuffers::F64(build_typed_sample_buffers(
            4,
            &[],
            &[],
            &buses(&[2]),
            &[true],
        ));
        let HostSampleBuffers::F64(samples) = &mut storage else {
            unreachable!()
        };
        samples.outputs[0].fill(0.0);
        samples.outputs[1].fill(1.0);
        let mut output_buses = buses(&[2]);
        prepare_output_silence_flags(&mut output_buses, &[true]);
        storage.update_output_silence_flags(&mut output_buses, &[true], 4);
        assert_eq!(output_buses[0].silenceFlags, 0b01);
    }

    #[test]
    fn host_silence_hints_preserve_signed_zero_and_processing_prefix() {
        let layout = buses(&[5, 1]);
        let active = [true, false];
        for use_f64 in [false, true] {
            let mut storage = if use_f64 {
                HostSampleBuffers::F64(build_typed_sample_buffers(
                    3, &layout, &active, &layout, &active,
                ))
            } else {
                HostSampleBuffers::F32(build_typed_sample_buffers(
                    3, &layout, &active, &layout, &active,
                ))
            };
            let input = [
                vec![0.0, 0.0, 1.0],
                vec![-0.0, -0.0, 1.0],
                vec![0.0, -0.0, 1.0],
                vec![f32::from_bits(1), 0.0, 0.0],
                vec![0.25, 0.0, 0.0],
            ];
            storage.copy_inputs_from(&input, 0, 3);
            if let HostSampleBuffers::F64(samples) = &mut storage {
                samples.inputs[3][0] = f64::from_bits(1);
            }
            let mut input_buses = buses(&[5, 1]);
            storage.update_input_silence_flags(&mut input_buses, &active, 2);
            assert_eq!(input_buses[0].silenceFlags, 0b00001, "f64={use_f64}");
            assert_eq!(input_buses[1].silenceFlags, 1);

            match &mut storage {
                HostSampleBuffers::F32(samples) => {
                    for (destination, source) in samples.outputs.iter_mut().zip(&input) {
                        destination.copy_from_slice(source);
                    }
                }
                HostSampleBuffers::F64(samples) => {
                    for (destination, source) in samples.outputs.iter_mut().zip(&input) {
                        for (to, from) in destination.iter_mut().zip(source) {
                            *to = *from as f64;
                        }
                    }
                    samples.outputs[3][0] = f64::from_bits(1);
                }
            }
            let mut output_buses = buses(&[5, 1]);
            prepare_output_silence_flags(&mut output_buses, &active);
            storage.update_output_silence_flags(&mut output_buses, &active, 2);
            assert_eq!(output_buses[0].silenceFlags, 0b00001, "f64={use_f64}");
            assert_eq!(output_buses[1].silenceFlags, 1);

            let mut flat = vec![vec![9.0; 4]; 5];
            storage.copy_outputs_to(&mut flat, 1, 2, &output_buses, &active);
            let mut routed = vec![
                AudioBusBuffer::new(5, 4, true),
                AudioBusBuffer::new(1, 4, false),
            ];
            for bus in &mut routed {
                for channel in &mut bus.channels {
                    channel.fill(9.0);
                }
            }
            storage.copy_outputs_to_buses(&mut routed, 1, 2, &output_buses, &active);
            for (channel, expected) in input.iter().enumerate().take(3) {
                for (sample, expected) in expected.iter().take(2).enumerate() {
                    assert_eq!(flat[channel][sample + 1].to_bits(), expected.to_bits());
                    assert_eq!(
                        routed[0].channels[channel][sample + 1].to_bits(),
                        expected.to_bits()
                    );
                }
                assert_eq!(flat[channel][0], 9.0);
                assert_eq!(flat[channel][3], 9.0);
            }
            assert_eq!(routed[1].channels[0], [9.0, 0.0, 0.0, 9.0]);

            // Native-declared silence remains authoritative, including for -0.
            output_buses[0].silenceFlags |= 0b00010;
            storage.update_output_silence_flags(&mut output_buses, &active, 2);
            storage.copy_outputs_to(&mut flat, 1, 2, &output_buses, &active);
            assert_eq!(flat[1][1].to_bits(), 0);
            assert_eq!(flat[1][2].to_bits(), 0);
        }
    }

    #[test]
    fn absent_sample_storage_keeps_silent_channel_hints() {
        for storage in [
            HostSampleBuffers::F32(build_typed_sample_buffers(2, &[], &[], &[], &[])),
            HostSampleBuffers::F64(build_typed_sample_buffers(2, &[], &[], &[], &[])),
        ] {
            let mut input = buses(&[1]);
            storage.update_input_silence_flags(&mut input, &[true], 2);
            assert_eq!(input[0].silenceFlags, 1);
            let mut output = buses(&[1]);
            storage.update_output_silence_flags(&mut output, &[true], 2);
            assert_eq!(output[0].silenceFlags, 1);
        }
    }

    #[test]
    fn zero_sample_flush_hides_then_restores_all_audio_buses() {
        let mut input_bus: AudioBusBuffers = unsafe { std::mem::zeroed() };
        let mut output_bus: AudioBusBuffers = unsafe { std::mem::zeroed() };
        let mut process_data: ProcessData = unsafe { std::mem::zeroed() };
        process_data.numInputs = 1;
        process_data.numOutputs = 1;
        process_data.inputs = &mut input_bus;
        process_data.outputs = &mut output_bus;
        let original_inputs = process_data.inputs;
        let original_outputs = process_data.outputs;

        let saved = hide_audio_io_for_zero_sample(&mut process_data, 0);
        assert_eq!(process_data.numInputs, 0);
        assert_eq!(process_data.numOutputs, 0);
        assert!(process_data.inputs.is_null());
        assert!(process_data.outputs.is_null());

        restore_process_audio_io(&mut process_data, saved);
        assert_eq!(process_data.numInputs, 1);
        assert_eq!(process_data.numOutputs, 1);
        assert_eq!(process_data.inputs, original_inputs);
        assert_eq!(process_data.outputs, original_outputs);
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[test]
    #[allow(clippy::unnecessary_cast)] // `as u32` needed where the constants are i32 (Windows)
    fn process_context_state_advertises_playhead_validity() {
        // The advancing continous/musical playhead is only honored by conformant plugins if
        // its validity flags are set.
        use ProcessContext_::StatesAndFlags_ as F;
        assert_ne!(PROCESS_CONTEXT_STATE & F::kPlaying as u32, 0);
        assert_ne!(PROCESS_CONTEXT_STATE & F::kTempoValid as u32, 0);
        assert_ne!(PROCESS_CONTEXT_STATE & F::kTimeSigValid as u32, 0);
        assert_ne!(PROCESS_CONTEXT_STATE & F::kContTimeValid as u32, 0);
        assert_ne!(PROCESS_CONTEXT_STATE & F::kProjectTimeMusicValid as u32, 0);
    }

    #[test]
    #[allow(clippy::unnecessary_cast)] // `as u32` needed where the constants are i32 (Windows)
    fn process_context_state_toggles_playing_without_disturbing_validity() {
        use ProcessContext_::StatesAndFlags_ as F;
        let playing = process_context_state(None, true, PROCESS_CONTEXT_STATE);
        let stopped = process_context_state(None, false, PROCESS_CONTEXT_STATE);
        // kPlaying tracks the playing flag.
        assert_ne!(playing & F::kPlaying as u32, 0);
        assert_eq!(stopped & F::kPlaying as u32, 0);
        // The validity flags survive in both states.
        for flag in [
            F::kTempoValid as u32,
            F::kTimeSigValid as u32,
            F::kContTimeValid as u32,
            F::kProjectTimeMusicValid as u32,
        ] {
            assert_ne!(playing & flag, 0);
            assert_ne!(stopped & flag, 0);
        }
    }

    #[test]
    fn advance_moves_playhead_and_musical_time() {
        let mut ctx: ProcessContext = unsafe { std::mem::zeroed() };
        ctx.sampleRate = 48_000.0;
        ctx.tempo = 120.0;
        ctx.state = PROCESS_CONTEXT_STATE;
        // One second of audio at 48 kHz in 512-sample blocks.
        let blocks = 48_000 / 512;
        for _ in 0..blocks {
            advance_process_context(&mut ctx, 120.0, None, 512).unwrap();
        }
        let advanced = (blocks * 512) as i64;
        assert_eq!(ctx.projectTimeSamples, advanced);
        assert_eq!(ctx.continousTimeSamples, advanced);
        // ~0.992 s elapsed at 120 BPM → ~1.98 quarter notes; just assert it moved forward.
        assert!(ctx.projectTimeMusic > 1.9 && ctx.projectTimeMusic < 2.1);
    }

    #[test]
    #[allow(clippy::unnecessary_cast)]
    fn unavailable_context_fields_remain_invalid_after_advance() {
        use IProcessContextRequirements_::Flags_ as R;
        let requirements = R::kNeedTempo as u32 | R::kNeedContinousTimeSamples as u32;
        assert_eq!(process_context_state(Some(requirements), false, 0), 0);
        let mut ctx: ProcessContext = unsafe { std::mem::zeroed() };
        ctx.sampleRate = 48_000.0;
        ctx.projectTimeSamples = -17;
        advance_process_context(&mut ctx, 120.0, None, 4).unwrap();
        assert_eq!(ctx.projectTimeSamples, -13);
        assert_eq!(ctx.state, 0);
        assert_eq!(ctx.continousTimeSamples, 0);
        assert_eq!(ctx.projectTimeMusic, 0.0);
        assert_eq!(ctx.tempo, 0.0);
    }

    #[test]
    #[allow(clippy::unnecessary_cast)]
    fn explicit_requirements_advertise_only_requested_fields() {
        use IProcessContextRequirements_::Flags_ as R;
        use ProcessContext_::StatesAndFlags_ as S;
        let requirements = R::kNeedTempo as u32 | R::kNeedContinousTimeSamples as u32;
        let state = process_context_state(Some(requirements), true, PROCESS_CONTEXT_STATE);
        assert_ne!(state & S::kTempoValid as u32, 0);
        assert_ne!(state & S::kContTimeValid as u32, 0);
        assert_eq!(state & S::kPlaying as u32, 0);
        assert_eq!(state & S::kTimeSigValid as u32, 0);
        assert_eq!(state & S::kProjectTimeMusicValid as u32, 0);
    }
}

#[cfg(test)]
mod chunked_block_tests {
    use super::*;
    use crate::internal::com_implementations::create_event_list;

    fn note_on_at(offset: i32, pitch: i16) -> PluginEvent {
        PluginEvent {
            bus_index: 0,
            sample_offset: offset,
            ppq_position: 0.0,
            flags: 0,
            data: crate::midi::PluginEventData::NoteOn {
                channel: 0,
                pitch,
                tuning: 0.0,
                velocity: 1.0,
                length: 0,
                note_id: -1,
            },
        }
    }

    /// Read back what the plugin would see: `(sampleOffset, pitch)` per staged event.
    fn staged(list: &HostEventList) -> Vec<(i32, i16)> {
        let mut out = Vec::new();
        list.drain_each(|e| {
            let crate::midi::PluginEventData::NoteOn { pitch, .. } = e.data else {
                panic!("expected note on");
            };
            out.push((e.sample_offset, pitch))
        });
        out
    }

    /// A device block larger than the configured block size is split into chunks, and each
    /// queued event has to land in the chunk that actually contains its offset — rebased to
    /// that chunk. Routing everything to chunk 0 (clamped to its end) fires late events ~20 ms
    /// early and collapses the spacing between them.
    #[test]
    fn events_are_routed_to_the_chunk_that_contains_them() {
        // A 2048-frame device block processed in 512-frame chunks.
        let mut queued = [
            Some(note_on_at(0, 60)),    // chunk 0, offset 0
            Some(note_on_at(100, 61)),  // chunk 0, offset 100
            Some(note_on_at(512, 62)),  // chunk 1, offset 0 (first sample of the chunk)
            Some(note_on_at(1500, 63)), // chunk 2, offset 476
        ];
        let list = create_event_list();

        stage_chunk_events(&mut queued, &list, 0, 512, false);
        assert_eq!(staged(&list), vec![(0, 60), (100, 61)]);

        stage_chunk_events(&mut queued, &list, 512, 512, false);
        assert_eq!(staged(&list), vec![(0, 62)]);

        stage_chunk_events(&mut queued, &list, 1024, 512, false);
        assert_eq!(staged(&list), vec![(476, 63)]);

        // The final chunk holds nothing: every event was delivered exactly once, earlier.
        stage_chunk_events(&mut queued, &list, 1536, 512, true);
        assert_eq!(staged(&list), Vec::new());
    }

    /// Offsets past the end of the caller's block still have to sound; they collapse into the
    /// last chunk rather than being dropped (the pre-split behaviour, preserved).
    #[test]
    fn offsets_past_the_block_land_in_the_final_chunk() {
        let mut queued = [Some(note_on_at(9_000, 64)), Some(note_on_at(-5, 65))];
        let list = create_event_list();

        // A non-final chunk ignores the overshoot; the negative offset is treated as 0.
        stage_chunk_events(&mut queued, &list, 0, 512, false);
        assert_eq!(staged(&list), vec![(0, 65)]);

        // The final chunk absorbs it at its last sample.
        stage_chunk_events(&mut queued, &list, 512, 512, true);
        assert_eq!(staged(&list), vec![(511, 64)]);
    }

    /// A block that fits in one chunk keeps the old semantics: exact offsets inside the block,
    /// anything past the end clamped to its last sample.
    #[test]
    fn single_chunk_block_clamps_to_its_own_length() {
        let mut queued = [
            Some(note_on_at(0, 60)),
            Some(note_on_at(200, 61)),
            Some(note_on_at(999, 62)),
        ];
        let list = create_event_list();

        stage_chunk_events(&mut queued, &list, 0, 256, true);
        assert_eq!(staged(&list), vec![(0, 60), (200, 61), (255, 62)]);
    }

    #[test]
    fn chunk_offset_maps_each_offset_once() {
        // Every offset in a 3-chunk block belongs to exactly one chunk.
        let chunks = [
            (0usize, 512usize, false),
            (512, 512, false),
            (1024, 512, true),
        ];
        for offset in [0, 1, 511, 512, 1023, 1024, 1535] {
            let hits: Vec<_> = chunks
                .iter()
                .filter_map(|&(start, frames, last)| {
                    chunk_offset(offset, start, frames, last).map(|o| (start, o))
                })
                .collect();
            assert_eq!(hits.len(), 1, "offset {offset} routed to {hits:?}");
            let (start, rebased) = hits[0];
            assert_eq!(start as i32 + rebased, offset);
        }
    }
}

#[cfg(test)]
mod output_midi_tests {
    use super::*;

    fn blank_event() -> Event {
        unsafe { std::mem::zeroed() }
    }

    #[test]
    fn converts_note_on() {
        let mut e = blank_event();
        e.r#type = kNoteOnEvent as u16;
        e.__field0.noteOn.channel = 0;
        e.__field0.noteOn.pitch = 60;
        e.__field0.noteOn.velocity = 1.0;
        assert_eq!(
            event_to_midi(&e),
            Some(MidiEvent::NoteOn {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 127
            })
        );
    }

    #[test]
    fn converts_note_off() {
        let mut e = blank_event();
        e.r#type = kNoteOffEvent as u16;
        e.__field0.noteOff.channel = 1;
        e.__field0.noteOff.pitch = 64;
        e.__field0.noteOff.velocity = 0.0;
        assert_eq!(
            event_to_midi(&e),
            Some(MidiEvent::NoteOff {
                channel: MidiChannel::Ch2,
                note: 64,
                velocity: 0
            })
        );
    }

    #[test]
    fn converts_legacy_cc_and_pitchbend() {
        // A plain CC.
        let mut cc = blank_event();
        cc.r#type = kLegacyMIDICCOutEvent as u16;
        cc.__field0.midiCCOut.controlNumber = 1; // mod wheel
        cc.__field0.midiCCOut.channel = 0;
        cc.__field0.midiCCOut.value = 64;
        assert_eq!(
            event_to_midi(&cc),
            Some(MidiEvent::ControlChange {
                channel: MidiChannel::Ch1,
                controller: 1,
                value: 64
            })
        );

        // Pitch bend round-trips the 14-bit value (LSB in value, MSB in value2).
        let mut pb = blank_event();
        pb.r#type = kLegacyMIDICCOutEvent as u16;
        pb.__field0.midiCCOut.controlNumber = ControllerNumbers_::kPitchBend as u8;
        pb.__field0.midiCCOut.channel = 0;
        pb.__field0.midiCCOut.value = (10000 & 0x7F) as std::os::raw::c_char;
        pb.__field0.midiCCOut.value2 = ((10000 >> 7) & 0x7F) as std::os::raw::c_char;
        assert_eq!(
            event_to_midi(&pb),
            Some(MidiEvent::PitchBend {
                channel: MidiChannel::Ch1,
                value: 10000
            })
        );
    }

    #[test]
    fn ignores_unknown_event_types() {
        let mut e = blank_event();
        e.r#type = 9999;
        assert_eq!(event_to_midi(&e), None);
    }

    /// MIDI's velocity-0 note-on is a note-off alias, and VST3 has no running status to express
    /// it. Emitting a zero-velocity `kNoteOnEvent` leaves the plugin sounding the voice while
    /// the host's note tracker has already counted the release, so `midi_panic` cannot clear it.
    #[test]
    fn velocity_zero_note_on_is_written_as_a_note_off() {
        let mut e = blank_event();
        let released = write_midi_note_on(&mut e, MidiChannel::Ch1, 60, 0);
        assert!(released);
        assert_eq!(
            event_to_midi(&e),
            Some(MidiEvent::NoteOff {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 0
            })
        );
    }

    #[test]
    fn ordinary_note_on_is_written_as_a_note_on() {
        let mut e = blank_event();
        let released = write_midi_note_on(&mut e, MidiChannel::Ch2, 64, 127);
        assert!(!released);
        assert_eq!(
            event_to_midi(&e),
            Some(MidiEvent::NoteOn {
                channel: MidiChannel::Ch2,
                note: 64,
                velocity: 127
            })
        );
    }

    #[test]
    fn note_off_keeps_its_release_velocity() {
        let mut e = blank_event();
        write_note_off_event(&mut e, MidiChannel::Ch1, 60, 64);
        assert_eq!(
            event_to_midi(&e),
            Some(MidiEvent::NoteOff {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 64
            })
        );
    }
}

#[cfg(test)]
mod midi_mapping_cache_tests {
    use super::*;

    /// The mapping table is `buses × 16 × 131` entries, so the bus count a plugin reports sizes
    /// a host allocation. An implausible (or hostile) count must not be taken at face value.
    #[test]
    fn bus_count_is_clamped_to_a_sane_range() {
        assert_eq!(midi_mapping_bus_count(-1), 0);
        assert_eq!(midi_mapping_bus_count(0), 0);
        assert_eq!(midi_mapping_bus_count(1), 1);
        assert_eq!(midi_mapping_bus_count(i32::MAX), MAX_MIDI_MAPPING_BUSES);
    }

    #[test]
    fn mapping_table_includes_vst3_program_change_controller() {
        let mut cache = MidiMappingCache {
            buses: 1,
            assignments: vec![None; MIDI_CHANNEL_COUNT * MIDI_CONTROLLER_COUNT],
        };
        let controller = ControllerNumbers_::kCtrlProgramChange as u16;
        let index = cache
            .index(0, MidiChannel::Ch16.as_index() as i16, controller)
            .expect("program change must fit in the MIDI mapping table");
        cache.assignments[index] = Some(42);

        assert_eq!(cache.get(0, 15, controller), Some(42));
    }
}

#[cfg(test)]
mod editor_scale_tests {
    use super::*;
    use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
    use vst3::Class;

    /// A view that implements `IPlugViewContentScaleSupport` and declines the factor, which is
    /// what JUCE editors do on macOS.
    struct DecliningScaleView {
        calls: Arc<AtomicUsize>,
        result: Arc<AtomicI32>,
    }

    impl Class for DecliningScaleView {
        type Interfaces = (IPlugView, IPlugViewContentScaleSupport);
    }

    impl IPlugViewTrait for DecliningScaleView {
        unsafe fn isPlatformTypeSupported(&self, _type: FIDString) -> tresult {
            kResultOk
        }
        unsafe fn attached(&self, _parent: *mut std::ffi::c_void, _type: FIDString) -> tresult {
            kResultOk
        }
        unsafe fn removed(&self) -> tresult {
            kResultOk
        }
        unsafe fn onWheel(&self, _distance: f32) -> tresult {
            kResultOk
        }
        unsafe fn onKeyDown(&self, _key: char16, _code: int16, _modifiers: int16) -> tresult {
            kResultOk
        }
        unsafe fn onKeyUp(&self, _key: char16, _code: int16, _modifiers: int16) -> tresult {
            kResultOk
        }
        unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
            *size = ViewRect {
                left: 0,
                top: 0,
                right: 640,
                bottom: 480,
            };
            kResultOk
        }
        unsafe fn onSize(&self, _new_size: *mut ViewRect) -> tresult {
            kResultOk
        }
        unsafe fn onFocus(&self, _state: TBool) -> tresult {
            kResultOk
        }
        unsafe fn setFrame(&self, _frame: *mut IPlugFrame) -> tresult {
            kResultOk
        }
        unsafe fn canResize(&self) -> tresult {
            kResultTrue
        }
        unsafe fn checkSizeConstraint(&self, _rect: *mut ViewRect) -> tresult {
            kResultOk
        }
    }

    impl IPlugViewContentScaleSupportTrait for DecliningScaleView {
        unsafe fn setContentScaleFactor(&self, _factor: f32) -> tresult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.load(Ordering::SeqCst)
        }
    }

    fn view(result: i32) -> (ComPtr<IPlugView>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let wrapper = ComWrapper::new(DecliningScaleView {
            calls: Arc::clone(&calls),
            result: Arc::new(AtomicI32::new(result)),
        });
        (wrapper.to_com_ptr::<IPlugView>().expect("view"), calls)
    }

    /// The open path asks every view for its scale factor, and a view that answers
    /// `kResultFalse` has *declined* — which is what JUCE editors do on macOS. Treating a
    /// decline as an error takes "Open GUI" away from every JUCE plugin on the platform, so the
    /// open path must never see one.
    #[test]
    fn a_declined_scale_factor_is_not_an_error() {
        let (view, calls) = view(kResultFalse);
        let accepted = unsafe { set_view_scale_factor(&view, 1.0) };
        assert!(!accepted.expect("a decline is not an error"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_accepted_scale_factor_reports_support() {
        let (view, calls) = view(kResultOk);
        assert!(unsafe { set_view_scale_factor(&view, 2.0) }.expect("accepted"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A nonsensical *request* is still the caller's bug, and stays an error.
    #[test]
    fn a_nonsensical_scale_factor_is_rejected_without_reaching_the_view() {
        let (view, calls) = view(kResultOk);
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(
                unsafe { set_view_scale_factor(&view, bad) }.is_err(),
                "{bad}"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

fn apply_controller_value(controller: &Option<ComPtr<IEditController>>, id: u32, value: f64) {
    let Some(controller) = controller.as_ref() else {
        return;
    };
    let result = unsafe { controller.setParamNormalized(id, value) };
    if result != kResultOk && result != kResultTrue {
        log::debug!(
            "setParamNormalized({id}) returned {result:#x}; applying anyway \
                 (many plugins report kResultFalse on success)"
        );
    }
}

fn process_audio_bus_layout(
    process_data: &Option<Box<HostProcessData>>,
    activation: &BusActivationState,
) -> Result<AudioBusLayout> {
    let data = process_data
        .as_ref()
        .ok_or_else(|| Error::Other("Process data not initialized".to_string()))?;
    let side = |buses: &[AudioBusBuffers], active: &[bool]| {
        buses
            .iter()
            .enumerate()
            .map(|(index, bus)| AudioBusConfig {
                channel_count: bus.numChannels.max(0) as usize,
                active: active.get(index).copied().unwrap_or(false),
            })
            .collect()
    };
    Ok(AudioBusLayout {
        inputs: side(&data.input_bus_buffers, &activation.audio_inputs),
        outputs: side(&data.output_bus_buffers, &activation.audio_outputs),
    })
}

fn validate_process_bus_buffers(
    process_data: &Option<Box<HostProcessData>>,
    activation: &BusActivationState,
    buffers: &BusAudioBuffers,
) -> Result<()> {
    let data = process_data
        .as_ref()
        .ok_or_else(|| Error::Other("Process data not initialized".to_string()))?;
    let validate = |label: &str,
                    supplied: &[AudioBusBuffer],
                    expected: &[AudioBusBuffers],
                    active: &[bool]|
     -> Result<()> {
        if supplied.len() != expected.len() {
            return Err(Error::Other(format!(
                "{label} bus count mismatch: expected {}, got {}",
                expected.len(),
                supplied.len()
            )));
        }
        for (index, (supplied, expected)) in supplied.iter().zip(expected).enumerate() {
            let expected_active = active.get(index).copied().unwrap_or(false);
            if supplied.active != expected_active {
                return Err(Error::Other(format!(
                    "{label} bus {index} activation is stale: expected {expected_active}, got {}",
                    supplied.active
                )));
            }
            let expected_channels = expected.numChannels.max(0) as usize;
            if supplied.channels.len() != expected_channels {
                return Err(Error::Other(format!(
                        "{label} bus {index} channel count mismatch: expected {expected_channels}, got {}",
                        supplied.channels.len()
                    )));
            }
        }
        Ok(())
    };
    validate(
        "input",
        &buffers.inputs,
        &data.input_bus_buffers,
        &activation.audio_inputs,
    )?;
    validate(
        "output",
        &buffers.outputs,
        &data.output_bus_buffers,
        &activation.audio_outputs,
    )
}

fn validate_checked_note_buffers(
    data: &Option<Box<HostProcessData>>,
    activation: &BusActivationState,
    sample_rate: f64,
    prepared_block: usize,
    buffers: &BusAudioBuffers,
) -> Result<()> {
    let frames = buffers.block_size;
    if frames == 0 || frames > i32::MAX as usize || prepared_block == 0 {
        return Err(Error::NativeEvent(NativeEventError::InvalidFrames));
    }
    if !sample_rate.is_finite() || sample_rate <= 0.0 || buffers.sample_rate != sample_rate {
        return Err(Error::NativeEvent(NativeEventError::SampleRate));
    }
    let data = data
        .as_ref()
        .ok_or(Error::NativeEvent(NativeEventError::PreparationRequired))?;
    for (supplied, expected, active) in [
        (
            &buffers.inputs,
            &data.input_bus_buffers,
            &activation.audio_inputs,
        ),
        (
            &buffers.outputs,
            &data.output_bus_buffers,
            &activation.audio_outputs,
        ),
    ] {
        if supplied.len() != expected.len() || active.len() != expected.len() {
            return Err(Error::NativeEvent(NativeEventError::AudioLayout));
        }
        for ((bus, native), active) in supplied.iter().zip(expected).zip(active) {
            if native.numChannels < 0
                || bus.active != *active
                || bus.channels.len() != native.numChannels as usize
            {
                return Err(Error::NativeEvent(NativeEventError::AudioLayout));
            }
            if bus.channels.iter().any(|channel| channel.len() != frames) {
                return Err(Error::NativeEvent(NativeEventError::InvalidFrames));
            }
        }
    }
    Ok(())
}

fn validate_checked_note_events(
    events: &[PluginEvent],
    frames: usize,
    inputs: &[EventBusActivation],
) -> Result<()> {
    use crate::midi::PluginEventData;
    if events.len() > MAX_QUEUED_EVENTS {
        return Err(Error::NativeEvent(NativeEventError::Capacity));
    }
    let allowed_flags = (Event_::EventFlags_::kIsLive
        | Event_::EventFlags_::kUserReserved1
        | Event_::EventFlags_::kUserReserved2) as u16;
    let mut previous = None;
    for (index, event) in events.iter().enumerate() {
        let invalid = || Error::NativeEvent(NativeEventError::InvalidEvent { index });
        if event.bus_index < 0
            || event.sample_offset < 0
            || event.sample_offset as usize >= frames
            || !event.ppq_position.is_finite()
            || event.flags & !allowed_flags != 0
        {
            return Err(invalid());
        }
        let bus = inputs.get(event.bus_index as usize).ok_or_else(invalid)?;
        if !bus.active {
            return Err(invalid());
        }
        let (channel, pitch, tuning, velocity, id) = match &event.data {
            PluginEventData::NoteOn {
                channel,
                pitch,
                tuning,
                velocity,
                length,
                note_id,
            } => {
                if *length < 0 {
                    return Err(invalid());
                }
                (*channel, *pitch, *tuning, *velocity, *note_id)
            }
            PluginEventData::NoteOff {
                channel,
                pitch,
                tuning,
                velocity,
                note_id,
            } => (*channel, *pitch, *tuning, *velocity, *note_id),
            _ => return Err(invalid()),
        };
        // -1 IDs are standard-defined, but intentionally outside this capability.
        if !(0..=15).contains(&channel)
            || channel as usize >= bus.channel_count
            || !(0..=127).contains(&pitch)
            || id < 0
            || !velocity.is_finite()
            || !(0.0..=1.0).contains(&velocity)
            || !tuning.is_finite()
        {
            return Err(invalid());
        }
        if previous.is_some_and(|offset| offset > event.sample_offset) {
            return Err(Error::NativeEvent(NativeEventError::EventOrder { index }));
        }
        previous = Some(event.sample_offset);
    }
    Ok(())
}

/// Disjoint mutable DSP fields, borrowed from the sole loaded PluginImpl.
/// No controller/component lifecycle or IPlugView capability reaches this role.
struct PluginProcessFields<'a> {
    processor: &'a ComPtr<IAudioProcessor>,
    is_active: bool,
    is_processing: bool,
    sample_rate: f64,
    block_size: usize,
    process_data: &'a mut Option<Box<HostProcessData>>,
    bus_activation: &'a BusActivationState,
    component_handler: &'a Option<ComWrapper<ComponentHandler>>,
    pending_param_changes: &'a mut Vec<ParameterChange>,
    input_events: &'a ComWrapper<HostEventList>,
    output_events: &'a ComWrapper<HostEventList>,
    chunk_events: &'a mut Vec<Option<PluginEvent>>,
    gui_param_changes_for_host: &'a Arc<Mutex<Vec<(u32, f64)>>>,
    output_param_feedback: &'a Arc<ArrayQueue<(u32, f64)>>,
    output_events_owned: &'a Arc<ArrayQueue<PluginEvent>>,
    deferred_controller_sync: &'a ArrayQueue<(u32, f64)>,
    host_app: &'a HostApplication,
}

/// Only native editor and controller fields; it never borrows DSP storage.
struct PluginEditorFields<'a> {
    controller: &'a Option<ComPtr<IEditController>>,
    component_handler: &'a Option<ComWrapper<ComponentHandler>>,
    native_editor_owner: crate::plugin::NativeEditorAttachmentOwner,
    plugin_view: &'a mut EditorAttachment,
    editor_scale_factor: &'a mut f32,
    plug_frame: &'a ComWrapper<HostPlugFrame>,
    editor_resize: &'a Arc<Mutex<Option<(i32, i32)>>>,
    deferred_controller_sync: &'a ArrayQueue<(u32, f64)>,
    host_app: &'a HostApplication,
    control_thread: ThreadId,
    #[cfg(target_os = "linux")]
    run_loop: &'a Arc<Mutex<RunLoopRegistry>>,
}

impl PluginImpl {
    fn process_fields(&mut self) -> PluginProcessFields<'_> {
        PluginProcessFields {
            processor: &self.processor,
            is_active: self.is_active,
            is_processing: self.is_processing,
            sample_rate: self.sample_rate,
            block_size: self.block_size,
            process_data: &mut self.process_data,
            bus_activation: &self.bus_activation,
            component_handler: &self.component_handler,
            pending_param_changes: &mut self.pending_param_changes,
            input_events: &self.input_events,
            output_events: &self.output_events,
            chunk_events: &mut self.chunk_events,
            gui_param_changes_for_host: &self.gui_param_changes_for_host,
            output_param_feedback: &self.output_param_feedback,
            output_events_owned: &self.output_events_owned,
            deferred_controller_sync: &self.deferred_controller_sync,
            host_app: &self._host_app,
        }
    }

    fn editor_fields(&mut self) -> PluginEditorFields<'_> {
        PluginEditorFields {
            controller: &self.controller,
            component_handler: &self.component_handler,
            native_editor_owner: self.native_editor_owner,
            plugin_view: &mut self.plugin_view,
            editor_scale_factor: &mut self.editor_scale_factor,
            plug_frame: &self.plug_frame,
            editor_resize: &self.editor_resize,
            deferred_controller_sync: &self.deferred_controller_sync,
            host_app: &self._host_app,
            control_thread: self.control_thread,
            #[cfg(target_os = "linux")]
            run_loop: &self.run_loop,
        }
    }
}

impl PluginProcessFields<'_> {
    fn process_chunk(
        &mut self,
        buffers: &mut CallerAudioBuffers<'_>,
        frame_offset: usize,
        frames: usize,
        is_last: bool,
        pure_audio: bool,
    ) -> Result<()> {
        let result = if let Some(data) = self.process_data.as_mut() {
            unsafe {
                // Clear output events only - input events should be preserved for processing
                self.output_events.clear();

                // Clear the output parameter queue too. VST3's ProcessData::outputParameterChanges
                // describes changes for the *current* processing block only (see
                // Steinberg::Vst::ProcessData / IParameterChanges docs); the reference
                // ParameterChanges host helper exposes clearQueue() for exactly this reset.
                // Without this, addParameterData()/addPoint() would keep appending to queues
                // from prior blocks, mixing stale points into new ones, growing point storage
                // unbounded, and risking a reallocation on the audio thread long after warm-up.
                data.output_param_changes.clear_all();

                // VST3 allows numSamples to vary up to the maximum given to setupProcessing; the
                // caller's block may be shorter (BufferSize::Default gives variable sizes) or, if
                // it was longer, `process` has already split it into chunks of at most that
                // maximum.
                let frames = frames.min(self.block_size);
                data.process_data.numSamples = frames as i32;

                // Feed the queued parameter changes that belong to this chunk into the
                // processor's input queue, rebased to the chunk — the same routing the events
                // got in `stage_chunk_events`, so automation and MIDI stay aligned across a
                // split block.
                for pc in self.pending_param_changes.iter() {
                    if let Some(off) = chunk_offset(pc.sample_offset, frame_offset, frames, is_last)
                    {
                        data.input_param_changes.enqueue(pc.id, off, pc.value);
                    }
                }

                // Route parameter edits the plugin's *own editor* reported via performEdit into
                // the processor too, so turning a knob in the plugin GUI affects the audio — not
                // just the host's display. (Some plugins relay editor→processor internally over
                // the component/controller connection; others rely on the host to do this. We do
                // it unconditionally; a plugin that also self-relays just gets the same value
                // twice in the same block, which is idempotent.) Drained here at offset 0 and
                // stashed for the host's display poll (get_parameter_changes).
                if let Some(ref handler) = self.component_handler {
                    if let Ok(mut gui_changes) = handler.parameter_changes.lock() {
                        if !gui_changes.is_empty() {
                            for &(id, value) in gui_changes.iter() {
                                data.input_param_changes.enqueue(id, 0, value);
                            }
                            if let Ok(mut stash) = self.gui_param_changes_for_host.lock() {
                                // Bounded: nothing drains the stash unless the host polls
                                // `get_parameter_changes`, and the realtime runner never does, so
                                // an unbounded append here would grow forever and reallocate on
                                // the audio thread. Both buffers are pre-reserved to the cap, so
                                // the steady-state append allocates nothing.
                                let room = MAX_EDITOR_FEEDBACK.saturating_sub(stash.len());
                                if room >= gui_changes.len() {
                                    stash.append(&mut gui_changes);
                                } else {
                                    stash.extend(gui_changes.drain(..room));
                                    gui_changes.clear();
                                }
                            } else {
                                gui_changes.clear();
                            }
                        }
                    }
                }

                match buffers {
                    CallerAudioBuffers::Flat(buffers) => {
                        data.sample_buffers
                            .copy_inputs_from(&buffers.inputs, frame_offset, frames)
                    }
                    CallerAudioBuffers::Buses(buffers) => {
                        data.sample_buffers.copy_inputs_from_buses(
                            &buffers.inputs,
                            frame_offset,
                            frames,
                            &data.input_bus_buffers,
                            &self.bus_activation.audio_inputs,
                        )
                    }
                }
                data.sample_buffers.clear_outputs();
                data.sample_buffers.update_input_silence_flags(
                    &mut data.input_bus_buffers,
                    &self.bus_activation.audio_inputs,
                    frames,
                );
                prepare_output_silence_flags(
                    &mut data.output_bus_buffers,
                    &self.bus_activation.audio_outputs,
                );

                // Channel pointers and process-data input/output pointers were wired once in
                // prepare_buffers (buffer addresses are stable), so there's nothing to rebuild
                // here — keeping the steady-state path allocation-free.

                // A zero-sample flush carries events/parameter queues only. The VST3 process
                // contract requires no audio buses or pointers for that call.
                let saved_audio_io = hide_audio_io_for_zero_sample(&mut data.process_data, frames);
                // ProcessData.inputEvents is optional. Some admits pure audio
                // only: do not expose late ingress through the original list.
                // No Rust owner or queue is replaced; this pointer is restored
                // immediately after native returns, before any fallible advance.
                let saved_input_events = pure_audio.then(|| {
                    std::mem::replace(&mut data.process_data.inputEvents, ptr::null_mut())
                });
                self.host_app.enter_data_exchange_process();
                let process_result = self.processor.process(&mut data.process_data);
                if let Some(input_events) = saved_input_events {
                    data.process_data.inputEvents = input_events;
                }
                self.host_app.leave_data_exchange_process();
                restore_process_audio_io(&mut data.process_data, saved_audio_io);

                // Everything from here to the output copy is per-block cleanup and MUST run even
                // when the plugin reported failure. Returning early instead would leave this
                // block's events and parameter changes queued, so the next block would re-deliver
                // every one of them — re-triggering held notes and growing the queues without
                // bound for as long as the plugin keeps failing.

                // Advance the transport so tempo-synced DSP (LFOs, sync'd delays/arps) sees
                // a moving playhead instead of a frozen time-0. The context describes the
                // block that was just processed; advancing here means the next block starts
                // at the new sample position.
                let advance_result = advance_process_context(
                    &mut data.process_context,
                    data.transport_tempo,
                    // Successful anchored installation and the whole-window
                    // preflight prove this prepared f64 is an exact nonzero
                    // u32 rate. Never derive the rate from plugin-writable ctx.
                    data.musical_reference
                        .map(|reference| (reference, self.sample_rate as u32)),
                    frames,
                );

                // Clear the staged input events AFTER processing, so the plugin got to see them
                // and the next chunk starts from an empty list.
                if !pure_audio {
                    self.input_events.clear();
                }
                // Clear the input parameter queue too, so this block's values don't
                // re-stick on the next block.
                data.input_param_changes.clear_all();

                // Processor-originated automation belongs to this block. Copy it into a
                // bounded lock-free feedback queue, then clear it on both success and failure
                // so stale points can never be re-reported.
                let feedback = &self.output_param_feedback;
                data.output_param_changes
                    .for_each_active_point(|id, _offset, value| {
                        feedback.force_push((id, value));
                    });
                data.output_param_changes.clear_all();

                // Capture any MIDI the plugin emitted this block (arpeggiators, MPE, etc.).
                // Drain the event list in place (no `mem::take`) and push each converted event
                // into the lock-free output queue with `force_push`, which drops the oldest event
                // when full. No lock and no allocation on the audio thread even while MIDI flows.
                if !self.output_events.is_empty() {
                    let out = &self.output_events_owned;
                    self.output_events.drain_each(|event| {
                        out.force_push(event);
                    });
                }

                if process_result != kResultOk {
                    // Leave the caller's buffers as they were (the playback bridges pre-fill them
                    // with silence) rather than copying out whatever the failed call left behind.
                    Err(Error::ProcessFailed(process_result))
                } else {
                    // The entire window was checked before consumption. Still
                    // fail cleanly if native code changed a context counter;
                    // all input/feedback cleanup above must run first, and the
                    // original native failure retains priority when both fail.
                    advance_result?;
                    data.sample_buffers.update_output_silence_flags(
                        &mut data.output_bus_buffers,
                        &self.bus_activation.audio_outputs,
                        frames,
                    );
                    match buffers {
                        CallerAudioBuffers::Flat(buffers) => data.sample_buffers.copy_outputs_to(
                            &mut buffers.outputs,
                            frame_offset,
                            frames,
                            &data.output_bus_buffers,
                            &self.bus_activation.audio_outputs,
                        ),
                        CallerAudioBuffers::Buses(buffers) => {
                            data.sample_buffers.copy_outputs_to_buses(
                                &mut buffers.outputs,
                                frame_offset,
                                frames,
                                &data.output_bus_buffers,
                                &self.bus_activation.audio_outputs,
                            )
                        }
                    }
                    Ok(())
                }
            }
        } else {
            Err(Error::Other("Process data not initialized".to_string()))
        };

        result
    }

    fn process_chunks(
        &mut self,
        buffers: &mut CallerAudioBuffers<'_>,
        total: usize,
        checked_notes: bool,
        pure_audio: bool,
    ) -> Result<()> {
        // `.max(1)`: a zero block size would make every chunk empty and never advance `offset`,
        // spinning forever on the audio thread. `Vst3HostBuilder::build` rejects 0, but
        // `block_size` is plain state and this loop must terminate regardless.
        let step = self.block_size.max(1);
        let mut offset = 0;
        while offset < total {
            let frames = (total - offset).min(step);
            let is_last = offset + frames >= total;
            if !pure_audio {
                if checked_notes {
                    self.input_events
                        .stage_checked_note_chunk(self.chunk_events, offset, frames)
                        .map_err(Error::NativeEvent)?;
                } else {
                    stage_chunk_events(
                        self.chunk_events,
                        self.input_events,
                        offset,
                        frames,
                        is_last,
                    );
                }
            }
            self.process_chunk(buffers, offset, frames, is_last, pure_audio)?;
            offset += frames;
        }
        Ok(())
    }

    fn process_checked_note_buffers(
        &mut self,
        buffers: &mut BusAudioBuffers,
        events: &[PluginEvent],
    ) -> Result<()> {
        if !self.is_active || !self.is_processing {
            return Err(Error::NotProcessing);
        }
        self.bus_activation
            .event_readiness
            .unwrap_or(Err(NativeEventError::PreparationRequired))
            .map_err(Error::NativeEvent)?;
        if self.component_handler.as_ref().is_some_and(|handler| {
            let flags = handler.pending_restart_flags();
            flags.io_changed() || flags.reload_component()
        }) {
            return Err(Error::NativeEvent(NativeEventError::PreparationRequired));
        }
        validate_checked_note_buffers(
            self.process_data,
            self.bus_activation,
            self.sample_rate,
            self.block_size,
            buffers,
        )?;
        validate_checked_note_events(
            events,
            buffers.block_size,
            &self.bus_activation.event_inputs,
        )?;
        self.process_buffer_view_inner(&mut CallerAudioBuffers::Buses(buffers), Some(events))
    }

    fn process_buffer_view(&mut self, buffers: &mut CallerAudioBuffers<'_>) -> Result<()> {
        self.process_buffer_view_inner(buffers, None)
    }

    fn process_buffer_view_inner(
        &mut self,
        buffers: &mut CallerAudioBuffers<'_>,
        notes: Option<&[PluginEvent]>,
    ) -> Result<()> {
        if !self.is_active || !self.is_processing {
            return Err(Error::NotProcessing);
        }

        let total = buffers.frame_count(self.block_size);
        let data = self
            .process_data
            .as_ref()
            .ok_or_else(|| Error::Other("Process data not initialized".to_owned()))?;
        checked_process_sample_end(&data.process_context, total)?;
        preflight_musical_window(
            &data.process_context,
            data.musical_reference,
            self.sample_rate,
            total,
            self.block_size,
        )?;
        let pure_audio = data.musical_reference.is_some();
        if pure_audio {
            if notes.is_some_and(|events| !events.is_empty()) {
                return Err(Error::MusicalTime(MusicalTimeError::EventInputUnsupported));
            }
            self.input_events
                .require_empty_anchored_ingress(self.chunk_events)
                .map_err(Error::NativeEvent)?;
        }
        let _denormal = crate::internal::denormal::DenormalGuard::new();
        if !pure_audio {
            if let Some(notes) = notes {
                // All fallible scalar/layout checks finished. This admission checks
                // both original capacities and absence of legacy input before mutation.
                self.input_events
                    .begin_checked_note_batch(self.chunk_events, notes)
                    .map_err(Error::NativeEvent)?;
            } else {
                self.input_events.take_into_slots(self.chunk_events);
            }
        }

        let result = if total == 0 {
            if !pure_audio {
                stage_chunk_events(self.chunk_events, self.input_events, 0, 0, true);
            }
            self.process_chunk(buffers, 0, 0, true, pure_audio)
        } else {
            self.process_chunks(buffers, total, notes.is_some(), pure_audio)
        };

        if !pure_audio {
            if notes.is_some() {
                // Consumption has begun; no future call may replay remaining chunks,
                // including when the second native call or input-list staging failed.
                self.input_events.clear_consumed_note_events();
            }
            self.chunk_events.clear();
        }
        self.pending_param_changes.clear();
        result
    }

    fn set_signal_timeline_context(
        &mut self,
        context: crate::audio::ProcessTimelineContext,
    ) -> Result<()> {
        if !self.is_processing {
            return Err(Error::NotProcessing);
        }
        if context.sample_rate == 0 || f64::from(context.sample_rate) != self.sample_rate {
            return Err(Error::InvalidParameter(
                "signal timeline rate differs from prepared processing".to_string(),
            ));
        }
        let data = self.process_data.as_mut().ok_or(Error::NotProcessing)?;
        // Assignment keeps the SDK pointer to this field stable. This adapter
        // owns no second timeline: optional author facts are unavailable here.
        data.process_context = unsafe { std::mem::zeroed() };
        data.process_context.sampleRate = f64::from(context.sample_rate);
        data.process_context.projectTimeSamples = context.sample_position;
        data.process_context.state =
            process_context_state(data.process_context_requirements, false, 0);
        data.musical_reference = None;
        Ok(())
    }

    #[allow(clippy::unnecessary_cast)]
    fn install_musical_timeline_context(
        &mut self,
        context: crate::audio::ProcessTimelineContext,
        reference: crate::audio::ProcessMusicalReference,
    ) -> Result<()> {
        if !self.is_processing {
            return Err(Error::NotProcessing);
        }
        if context.sample_rate == 0 || f64::from(context.sample_rate) != self.sample_rate {
            return Err(Error::MusicalTime(MusicalTimeError::SampleRate));
        }
        let position = reference
            .quarter_notes_at(context.sample_position, context.sample_rate)
            .map_err(Error::MusicalTime)?;
        let tempo = reference.tempo_bpm();
        let data = self.process_data.as_mut().ok_or(Error::NotProcessing)?;
        // All validation/projection precedes publication. The assignment keeps
        // the ProcessData pointer to this same retained field stable.
        let mut next: ProcessContext = unsafe { std::mem::zeroed() };
        next.sampleRate = f64::from(context.sample_rate);
        next.projectTimeSamples = context.sample_position;
        next.projectTimeMusic = position;
        next.tempo = tempo;
        next.state = process_context_state(
            data.process_context_requirements,
            false,
            (ProcessContext_::StatesAndFlags_::kProjectTimeMusicValid
                | ProcessContext_::StatesAndFlags_::kTempoValid) as u32,
        );
        data.process_context = next;
        data.musical_reference = Some(reference);
        Ok(())
    }
}

impl PluginEditorFields<'_> {
    fn open_editor(&mut self, parent: *mut std::ffi::c_void) -> Result<()> {
        if self.plugin_view.is_present() {
            return Err(Error::Other("Editor already open".to_string()));
        }
        if parent.is_null() {
            return Err(Error::Other(
                "editor parent window handle is null".to_string(),
            ));
        }

        if let Some(ref controller) = self.controller {
            unsafe {
                // Create editor view
                let view_type = c"editor".as_ptr();
                let view_ptr = controller.createView(view_type);
                if view_ptr.is_null() {
                    return Err(Error::EditorUnavailable);
                }

                let view = ComPtr::<IPlugView>::from_raw(view_ptr)
                    .ok_or_else(|| Error::Other("Failed to wrap view".to_string()))?;

                // Get view size
                let mut view_rect = ViewRect {
                    left: 0,
                    top: 0,
                    right: 400,
                    bottom: 300,
                };

                if view.getSize(&mut view_rect) != kResultOk {
                    return Err(Error::Other("Failed to get view size".to_string()));
                }
                view_rect_size(&view_rect)?;

                // Hand the plugin an IPlugFrame (before attach, per the SDK) so it can
                // request host-side resizes; requests land in `editor_resize`.
                let frame = self.plug_frame.to_com_ptr::<IPlugFrame>().ok_or_else(|| {
                    Error::Other("Failed to create editor plug frame".to_string())
                })?;
                let frame_result = view.setFrame(frame.as_ptr());
                if frame_result != kResultOk && frame_result != kResultTrue {
                    return Err(Error::Other(format!(
                        "Plugin rejected editor plug frame: {frame_result:#x}"
                    )));
                }

                // Offer the current scale factor, but never let the answer decide whether the
                // editor opens: a view that declines simply renders at its own scale.
                let _ = set_view_scale_factor(&view, *self.editor_scale_factor);

                // Platform-specific attachment
                #[cfg(target_os = "macos")]
                let platform_type = kPlatformTypeNSView;
                #[cfg(target_os = "windows")]
                let platform_type = kPlatformTypeHWND;
                #[cfg(target_os = "linux")]
                let platform_type = kPlatformTypeX11EmbedWindowID;
                #[cfg(target_os = "android")]
                {
                    self.plugin_view.clear_unattached_frame(view)?;
                    return Err(Error::Other(
                        "embedded VST3 editors are not supported on Android".to_string(),
                    ));
                }

                // Check platform support
                #[cfg(not(target_os = "android"))]
                if view.isPlatformTypeSupported(platform_type) != kResultOk {
                    self.plugin_view.clear_unattached_frame(view)?;
                    return Err(Error::EditorPlatformUnsupported);
                }

                // Attach to parent window
                #[cfg(not(target_os = "android"))]
                let attach_result = view.attached(parent, platform_type);
                #[cfg(not(target_os = "android"))]
                if attach_result != kResultOk {
                    self.plugin_view.clear_unattached_frame(view)?;
                    return Err(Error::Other(format!(
                        "Failed to attach view: {:#x}",
                        attach_result
                    )));
                }

                #[cfg(not(target_os = "android"))]
                {
                    *self.plugin_view = EditorAttachment::Attached(view);
                    Ok(())
                }
            }
        } else {
            Err(Error::EditorUnavailable)
        }
    }

    fn close_editor(&mut self) -> Result<()> {
        let result = self.plugin_view.detach();
        // Remove callbacks only after the view has actually accepted removed.
        // On unknown removal it still owns its host frame and registrations.
        #[cfg(target_os = "linux")]
        if self.plugin_view.removal_complete() {
            let mut registry = self.run_loop.lock().map_err(|_| {
                Error::Other("editor run-loop state is poisoned during removal".to_string())
            })?;
            registry.handlers.clear();
            registry.timers.clear();
        }
        result
    }

    fn resize_editor(&mut self, width: i32, height: i32) -> Result<(i32, i32)> {
        if width <= 0 || height <= 0 {
            return Err(Error::Other(
                "editor dimensions must be greater than zero".to_string(),
            ));
        }
        let view = self
            .plugin_view
            .live_view()?
            .ok_or_else(|| Error::Other("Plugin editor is not open".to_string()))?;

        unsafe {
            if view.canResize() != kResultTrue {
                let mut current = ViewRect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                if view.getSize(&mut current) != kResultOk {
                    return Err(Error::Other(
                        "Plugin editor is fixed-size and its size could not be queried".to_string(),
                    ));
                }
                return view_rect_size(&current);
            }

            let mut requested = ViewRect {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            // The view constrains `requested` in place. `kResultFalse` means it left the rect
            // alone (nothing to constrain, or it wants a different size than asked for) — a
            // refusal to adapt, not a broken call — so take whatever rect it ended up with and
            // only reject result codes that mean the call itself failed.
            let constraint_result = view.checkSizeConstraint(&mut requested);
            let constrained = constraint_result == kResultOk
                || constraint_result == kResultTrue
                || constraint_result == kResultFalse
                || constraint_result == kNotImplemented;
            if !constrained {
                return Err(Error::Other(format!(
                    "Plugin failed to check the editor size constraint: {constraint_result:#x}"
                )));
            }
            let accepted = view_rect_size(&requested)?;

            // The SDK calls onSize only when the size actually changes; re-sending the current
            // one makes VSTGUI-based editors rebuild their frame for nothing.
            let mut current = ViewRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if view.getSize(&mut current) == kResultOk
                && view_rect_size(&current).ok() == Some(accepted)
            {
                return Ok(accepted);
            }

            let resize_result = view.onSize(&mut requested);
            if resize_result != kResultOk && resize_result != kResultTrue {
                return Err(Error::Other(format!(
                    "Plugin rejected editor resize: {resize_result:#x}"
                )));
            }
            Ok(accepted)
        }
    }

    fn set_editor_scale_factor(&mut self, factor: f32) -> Result<bool> {
        if !factor.is_finite() || factor <= 0.0 {
            return Err(Error::Other(
                "editor scale factor must be finite and greater than zero".to_string(),
            ));
        }
        let supported = match self.plugin_view.live_view()? {
            Some(view) => unsafe { set_view_scale_factor(view, factor)? },
            None => false,
        };
        *self.editor_scale_factor = factor;
        Ok(supported)
    }

    #[cfg(target_os = "linux")]
    fn service_run_loop(&mut self) {
        use vst3::Steinberg::Linux::{IEventHandlerTrait, ITimerHandlerTrait};

        // Fire due timers. Snapshot the handlers, then invoke with the lock
        // RELEASED: a callback may re-enter registerTimer/unregisterTimer
        // (VSTGUI does), which takes the same lock.
        let now = std::time::Instant::now();
        let mut due = Vec::new();
        if let Ok(mut reg) = self.run_loop.lock() {
            for timer in reg.timers.iter_mut() {
                if now >= timer.due {
                    timer.due = now + std::time::Duration::from_millis(timer.interval_ms);
                    due.push(timer.handler.clone());
                }
            }
        }
        for handler in due {
            unsafe { handler.onTimer() };
        }

        // Poll registered fds (zero timeout - never blocks the UI thread)
        // and notify ready ones. Same snapshot-then-invoke pattern.
        let handlers: Vec<_> = match self.run_loop.lock() {
            Ok(reg) => reg.handlers.clone(),
            Err(_) => return,
        };
        if handlers.is_empty() {
            return;
        }
        let mut fds: Vec<libc::pollfd> = handlers
            .iter()
            .map(|&(_, fd)| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, 0) };
        if ready > 0 {
            for (pfd, (handler, fd)) in fds.iter().zip(handlers.iter()) {
                if pfd.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
                    unsafe { handler.onFDIsSet(*fd) };
                }
            }
        }
    }
}

fn has_editor_view(
    info_has_gui: bool,
    plugin_view: &EditorAttachment,
    controller: &Option<ComPtr<IEditController>>,
) -> bool {
    // First check our cached value
    if info_has_gui {
        return true;
    }

    // An open editor is proof enough, and probing for a second view while one is attached
    // upsets plugins that assume a single live view.
    if plugin_view.is_present() {
        return true;
    }

    // Otherwise do a runtime check
    if let Some(ref controller) = controller {
        unsafe {
            // Check if controller can create an editor view
            let view_type = c"editor".as_ptr();
            let view_ptr = controller.createView(view_type);
            if !view_ptr.is_null() {
                // Release the probe view; never call `removed()` on it — that pairs with
                // `attached()`, and an unmatched `removed()` crashes some plugins that
                // initialize their close state only on attach.
                let _ = ComPtr::<IPlugView>::from_raw(view_ptr);
                true
            } else {
                false
            }
        }
    } else {
        false
    }
}

fn query_editor_size(
    plugin_view: &EditorAttachment,
    controller: &Option<ComPtr<IEditController>>,
) -> Result<(i32, i32)> {
    if let Some(view) = plugin_view.live_view()? {
        unsafe {
            let mut view_rect = ViewRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if view.getSize(&mut view_rect) != kResultOk {
                return Err(Error::Other("Failed to query open editor size".to_string()));
            }
            return view_rect_size(&view_rect);
        }
    }

    if let Some(ref controller) = controller {
        unsafe {
            // Create a temporary view to get size
            let view_type = c"editor".as_ptr();
            let view_ptr = controller.createView(view_type);
            if view_ptr.is_null() {
                return Err(Error::EditorUnavailable);
            }

            let view = ComPtr::<IPlugView>::from_raw(view_ptr)
                .ok_or_else(|| Error::Other("Failed to wrap plug view".to_string()))?;

            // Get view size
            let mut view_rect = ViewRect {
                left: 0,
                top: 0,
                right: 400,
                bottom: 300,
            };

            let result = view.getSize(&mut view_rect);

            // Released when `view` drops; do not call `removed()` — it pairs with
            // `attached()`, which this probe never calls.

            if result == kResultOk {
                view_rect_size(&view_rect)
            } else {
                Ok((800, 600)) // Default size
            }
        }
    } else {
        Err(Error::EditorUnavailable)
    }
}

fn can_resize_editor(
    plugin_view: &EditorAttachment,
    controller: &Option<ComPtr<IEditController>>,
) -> bool {
    unsafe {
        let Ok(view) = plugin_view.live_view() else {
            return false;
        };
        if let Some(view) = view {
            return view.canResize() == kResultTrue;
        }

        let Some(controller) = controller.as_ref() else {
            return false;
        };
        let view_ptr = controller.createView(c"editor".as_ptr());
        let Some(view) = ComPtr::<IPlugView>::from_raw(view_ptr) else {
            return false;
        };
        view.canResize() == kResultTrue
    }
}

impl PluginEditorFields<'_> {
    fn ensure_control_thread(&self) -> Result<()> {
        if thread::current().id() == self.control_thread {
            Ok(())
        } else {
            Err(Error::Other(
                "native editor control must run on its initializing thread".to_string(),
            ))
        }
    }
}

impl crate::plugin::PluginEditorRole for PluginEditorFields<'_> {
    fn attachment_owner(&self) -> Result<crate::plugin::NativeEditorAttachmentOwner> {
        self.ensure_control_thread()?;
        Ok(self.native_editor_owner)
    }
    fn open_editor(&mut self, parent: *mut std::ffi::c_void) -> Result<()> {
        self.ensure_control_thread()?;
        PluginEditorFields::open_editor(self, parent)
    }
    fn close_editor(&mut self) -> Result<()> {
        self.ensure_control_thread()?;
        PluginEditorFields::close_editor(self)
    }
    fn editor_size(&self) -> Result<(i32, i32)> {
        self.ensure_control_thread()?;
        query_editor_size(self.plugin_view, self.controller)
    }
    fn editor_can_resize(&self) -> Result<bool> {
        self.ensure_control_thread()?;
        Ok(can_resize_editor(self.plugin_view, self.controller))
    }
    fn resize_editor(&mut self, width: i32, height: i32) -> Result<(i32, i32)> {
        self.ensure_control_thread()?;
        PluginEditorFields::resize_editor(self, width, height)
    }
    fn set_editor_scale_factor(&mut self, factor: f32) -> Result<bool> {
        self.ensure_control_thread()?;
        PluginEditorFields::set_editor_scale_factor(self, factor)
    }
    fn editor_event_source_status(&self) -> Result<crate::plugin::NativeEditorEventSourceStatus> {
        self.ensure_control_thread()?;
        let handler = self
            .component_handler
            .as_ref()
            .ok_or_else(|| Error::Other("component handler is unavailable".to_string()))?;
        Ok(handler.editor_event_source_status())
    }

    fn take_editor_events(&mut self) -> Result<crate::plugin::NativeEditorEventDrain> {
        self.ensure_control_thread()?;
        let handler = self
            .component_handler
            .as_ref()
            .ok_or_else(|| Error::Other("component handler is unavailable".to_string()))?;
        Ok(handler.take_editor_events())
    }
    fn pending_restart_cohort(&self) -> Result<Option<crate::plugin::RestartCohort>> {
        self.ensure_control_thread()?;
        let handler = self
            .component_handler
            .as_ref()
            .ok_or_else(|| Error::Other("component handler is unavailable".to_string()))?;
        Ok(Some(handler.pending_restart_cohort()))
    }
    fn take_restart_flags(&mut self) -> Result<crate::plugin::RestartFlags> {
        self.ensure_control_thread()?;
        let handler = self
            .component_handler
            .as_ref()
            .ok_or_else(|| Error::Other("component handler is unavailable".to_string()))?;
        Ok(handler.take_restart_flags())
    }
    fn service_controller(&mut self) -> Result<()> {
        self.ensure_control_thread()?;
        while let Some((id, value)) = self.deferred_controller_sync.pop() {
            apply_controller_value(self.controller, id, value);
        }
        self.host_app.service_main_thread_data_exchange();
        Ok(())
    }
    fn parameter(&self, id: u32) -> Result<f64> {
        self.ensure_control_thread()?;
        let controller = self
            .controller
            .as_ref()
            .ok_or_else(|| Error::InterfaceError("No controller available".to_string()))?;
        Ok(unsafe { controller.getParamNormalized(id) })
    }
    fn take_editor_resize_request(&self) -> Result<Option<(i32, i32)>> {
        self.ensure_control_thread()?;
        self.editor_resize
            .lock()
            .map(|mut requested| requested.take())
            .map_err(|_| Error::Other("editor resize slot is poisoned".to_string()))
    }
    fn service_run_loop(&mut self) -> Result<()> {
        self.ensure_control_thread()?;
        #[cfg(target_os = "linux")]
        PluginEditorFields::service_run_loop(self);
        Ok(())
    }
}

impl crate::plugin::PluginProcessingRole for PluginProcessFields<'_> {
    fn set_process_timeline_context(
        &mut self,
        context: crate::audio::ProcessTimelineContext,
    ) -> Result<()> {
        self.set_signal_timeline_context(context)
    }
    fn set_process_musical_timeline_context(
        &mut self,
        context: crate::audio::ProcessTimelineContext,
        reference: crate::audio::ProcessMusicalReference,
    ) -> Result<()> {
        self.install_musical_timeline_context(context, reference)
    }
    fn queue_parameter_at(&mut self, id: u32, value: f64, sample_offset: i32) -> Result<()> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) || sample_offset < 0 {
            return Err(Error::InvalidParameter(
                "normalized processor parameter or sample offset is invalid".to_string(),
            ));
        }
        if self.pending_param_changes.len() >= MAX_PENDING_PARAM_CHANGES {
            return Err(Error::Other(
                "processor parameter queue is full".to_string(),
            ));
        }
        self.pending_param_changes.push(ParameterChange {
            id,
            value,
            sample_offset,
        });
        self.deferred_controller_sync.force_push((id, value));
        Ok(())
    }
    fn queue_unconsumed_editor_values(&mut self) -> Result<bool> {
        if !self.pending_param_changes.is_empty() {
            return Err(Error::Other(
                "unconsumed editor replay cannot replace an already staged parameter queue"
                    .to_string(),
            ));
        }
        let handler = self
            .component_handler
            .as_ref()
            .ok_or_else(|| Error::Other("component handler is unavailable".to_string()))?;
        let values = &mut self.pending_param_changes;
        let accepted = handler.visit_unconsumed_editor_values(|id, value| {
            if values.len() >= MAX_PENDING_PARAM_CHANGES {
                return false;
            }
            values.push(ParameterChange {
                id,
                value,
                sample_offset: 0,
            });
            true
        });
        if !accepted {
            // No native process occurred. Refuse the whole incomplete source,
            // never process a silently truncated value prefix.
            values.clear();
            return Ok(false);
        }
        Ok(true)
    }
    fn process_bus_audio(&mut self, buffers: &mut BusAudioBuffers) -> Result<()> {
        validate_process_bus_buffers(self.process_data, self.bus_activation, buffers)?;
        self.process_buffer_view(&mut CallerAudioBuffers::Buses(buffers))
    }
    fn process_bus_audio_with_note_events(
        &mut self,
        buffers: &mut BusAudioBuffers,
        events: &[PluginEvent],
    ) -> Result<()> {
        self.process_checked_note_buffers(buffers, events)
    }
}

#[cfg(test)]
mod editor_detach_failure_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
    use vst3::Class;

    struct DetachProbe {
        attached: AtomicBool,
        removal_result: Arc<AtomicI32>,
        frame_result: Arc<AtomicI32>,
        removals: Arc<AtomicUsize>,
        frame_clears: Arc<AtomicUsize>,
    }

    impl Class for DetachProbe {
        type Interfaces = (IPlugView,);
    }
    impl IPlugViewTrait for DetachProbe {
        unsafe fn isPlatformTypeSupported(&self, _kind: FIDString) -> tresult {
            kResultOk
        }
        unsafe fn attached(&self, _parent: *mut std::ffi::c_void, _kind: FIDString) -> tresult {
            self.attached.store(true, Ordering::SeqCst);
            kResultOk
        }
        unsafe fn removed(&self) -> tresult {
            assert!(
                self.attached.load(Ordering::SeqCst),
                "removed requires a still attached view"
            );
            self.removals.fetch_add(1, Ordering::SeqCst);
            let result = self.removal_result.load(Ordering::SeqCst);
            if result == kResultOk || result == kResultTrue {
                self.attached.store(false, Ordering::SeqCst);
            }
            result
        }
        unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
            if frame.is_null() {
                self.frame_clears.fetch_add(1, Ordering::SeqCst);
                self.frame_result.load(Ordering::SeqCst)
            } else {
                kResultOk
            }
        }
        unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
            unsafe {
                *size = ViewRect {
                    left: 0,
                    top: 0,
                    right: 640,
                    bottom: 480,
                };
            }
            kResultOk
        }
        unsafe fn onWheel(&self, _distance: f32) -> tresult {
            kResultOk
        }
        unsafe fn onKeyDown(&self, _key: char16, _code: int16, _modifiers: int16) -> tresult {
            kResultOk
        }
        unsafe fn onKeyUp(&self, _key: char16, _code: int16, _modifiers: int16) -> tresult {
            kResultOk
        }
        unsafe fn onSize(&self, _size: *mut ViewRect) -> tresult {
            kResultOk
        }
        unsafe fn onFocus(&self, _state: TBool) -> tresult {
            kResultOk
        }
        unsafe fn canResize(&self) -> tresult {
            kResultTrue
        }
        unsafe fn checkSizeConstraint(&self, _size: *mut ViewRect) -> tresult {
            kResultOk
        }
    }

    /// This is the real private close implementation with a native ABI view
    /// probe, not a physical HWND/module-load test. Seed attachment is paired
    /// through the actual IPlugView callback before closing it.
    struct Fixture {
        controller: Option<ComPtr<IEditController>>,
        handler: Option<ComWrapper<ComponentHandler>>,
        view: EditorAttachment,
        native_editor_owner: crate::plugin::NativeEditorAttachmentOwner,
        scale: f32,
        frame: ComWrapper<HostPlugFrame>,
        resize: Arc<Mutex<Option<(i32, i32)>>>,
        deferred: ArrayQueue<(u32, f64)>,
        host: HostApplication,
        #[cfg(target_os = "linux")]
        run_loop: Arc<Mutex<RunLoopRegistry>>,
        removal_result: Arc<AtomicI32>,
        frame_result: Arc<AtomicI32>,
        removals: Arc<AtomicUsize>,
        frame_clears: Arc<AtomicUsize>,
    }

    impl Fixture {
        fn new(removal: tresult, clear: tresult) -> Self {
            let removal_result = Arc::new(AtomicI32::new(removal));
            let frame_result = Arc::new(AtomicI32::new(clear));
            let removals = Arc::new(AtomicUsize::new(0));
            let frame_clears = Arc::new(AtomicUsize::new(0));
            let resize = Arc::new(Mutex::new(None));
            #[cfg(target_os = "linux")]
            let run_loop = Arc::new(Mutex::new(RunLoopRegistry::new()));
            #[cfg(target_os = "linux")]
            let frame = create_host_plug_frame(Arc::clone(&resize), Arc::clone(&run_loop));
            #[cfg(not(target_os = "linux"))]
            let frame = create_host_plug_frame(Arc::clone(&resize));
            let probe = ComWrapper::new(DetachProbe {
                attached: AtomicBool::new(false),
                removal_result: Arc::clone(&removal_result),
                frame_result: Arc::clone(&frame_result),
                removals: Arc::clone(&removals),
                frame_clears: Arc::clone(&frame_clears),
            });
            let view = probe.to_com_ptr::<IPlugView>().expect("view interface");
            let frame_ptr = frame.to_com_ptr::<IPlugFrame>().expect("frame interface");
            let mut mock_parent = 0_u8;
            unsafe {
                assert_eq!(view.setFrame(frame_ptr.as_ptr()), kResultOk);
                assert_eq!(
                    view.attached((&mut mock_parent as *mut u8).cast(), c"probe".as_ptr()),
                    kResultOk
                );
            }
            Self {
                controller: None,
                handler: None,
                view: EditorAttachment::Attached(view),
                native_editor_owner: crate::plugin::NativeEditorAttachmentOwner::issue()
                    .expect("native fixture owner"),
                scale: 1.0,
                frame,
                resize,
                deferred: ArrayQueue::new(4),
                host: HostApplication::default(),
                #[cfg(target_os = "linux")]
                run_loop,
                removal_result,
                frame_result,
                removals,
                frame_clears,
            }
        }
        fn fields(&mut self) -> PluginEditorFields<'_> {
            PluginEditorFields {
                controller: &self.controller,
                component_handler: &self.handler,
                native_editor_owner: self.native_editor_owner,
                plugin_view: &mut self.view,
                editor_scale_factor: &mut self.scale,
                plug_frame: &self.frame,
                editor_resize: &self.resize,
                deferred_controller_sync: &self.deferred,
                host_app: &self.host,
                control_thread: thread::current().id(),
                #[cfg(target_os = "linux")]
                run_loop: &self.run_loop,
            }
        }
        fn close(&mut self) -> Result<()> {
            self.fields().close_editor()
        }
    }

    #[test]
    fn failed_editor_removal_cannot_become_a_successful_second_close() {
        let mut fixture = Fixture::new(kResultFalse, kResultOk);
        assert!(
            fixture.close().is_err(),
            "native remove failure must be explicit"
        );
        fixture.removal_result.store(kResultOk, Ordering::SeqCst);
        assert!(
            fixture.close().is_err(),
            "a failed removal cannot lose the live view and pretend a later close succeeded"
        );
        assert_eq!(
            fixture.removals.load(Ordering::SeqCst),
            1,
            "unknown attachment must not be retried as a fresh remove"
        );
        assert_eq!(
            fixture.frame_clears.load(Ordering::SeqCst),
            0,
            "a possibly attached native view must retain its live host frame"
        );
    }

    #[test]
    fn failed_editor_frame_clear_retries_only_frame_clear_not_removed() {
        let mut fixture = Fixture::new(kResultOk, kResultFalse);
        assert!(
            fixture.close().is_err(),
            "native frame clear failure must be explicit"
        );
        fixture.frame_result.store(kResultOk, Ordering::SeqCst);
        fixture
            .close()
            .expect("only a successful frame clear completes closure");
        assert_eq!(
            fixture.removals.load(Ordering::SeqCst),
            1,
            "removed callback is paired once with attached"
        );
        assert_eq!(
            fixture.frame_clears.load(Ordering::SeqCst),
            2,
            "the still-owned view must receive the actual clear retry"
        );
    }

    #[test]
    fn successful_editor_close_is_paired_once_and_repeated_close_is_noop() {
        let mut fixture = Fixture::new(kResultOk, kResultOk);
        fixture.close().expect("removed and cleared");
        fixture.close().expect("already closed");
        assert_eq!(fixture.removals.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.frame_clears.load(Ordering::SeqCst), 1);
    }
    struct UnusedProcessingRole;
    impl crate::plugin::PluginProcessingRole for UnusedProcessingRole {
        fn set_process_timeline_context(
            &mut self,
            _context: crate::audio::ProcessTimelineContext,
        ) -> Result<()> {
            Ok(())
        }
        fn queue_parameter_at(&mut self, _id: u32, _value: f64, _offset: i32) -> Result<()> {
            Ok(())
        }
        fn queue_unconsumed_editor_values(&mut self) -> Result<bool> {
            Ok(true)
        }
        fn process_bus_audio(&mut self, _buffers: &mut BusAudioBuffers) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn temporary_editor_role_drop_preserves_owner_view_until_explicit_close() {
        let mut fixture = Fixture::new(kResultOk, kResultOk);
        {
            let roles = crate::plugin::PluginEditorProcessingSplit::from_roles(
                Box::new(fixture.fields()),
                Box::new(UnusedProcessingRole),
            );
            drop(roles);
        }
        assert_eq!(
            fixture.removals.load(Ordering::SeqCst),
            0,
            "a temporary role is not the native view lifetime owner"
        );
        assert_eq!(fixture.frame_clears.load(Ordering::SeqCst), 0);
        fixture
            .close()
            .expect("full native owner explicitly removes its view");
        assert_eq!(fixture.removals.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.frame_clears.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn temporary_control_only_role_preserves_owner_view_until_explicit_close() {
        let mut fixture = Fixture::new(kResultOk, kResultOk);
        let owner = fixture.native_editor_owner;
        {
            let control = crate::plugin::PluginEditorControl::from_role(Box::new(fixture.fields()));
            assert_eq!(control.attachment_owner().unwrap(), owner);
            drop(control);
        }
        assert_eq!(fixture.removals.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.frame_clears.load(Ordering::SeqCst), 0);
        fixture
            .close()
            .expect("the original owner explicitly closes the view");
        assert_eq!(fixture.removals.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.frame_clears.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_remove_reports_exact_native_stage_and_refuses_live_ui() {
        let mut fixture = Fixture::new(kResultFalse, kResultOk);
        assert!(matches!(
            fixture.close(),
            Err(Error::EditorDetachFailed {
                stage: crate::error::EditorDetachStage::Removal,
                result
            }) if result == kResultFalse
        ));
        assert!(
            matches!(
                fixture.fields().resize_editor(800, 600),
                Err(Error::EditorLifecycleBlocked)
            ),
            "unknown attachment cannot perform live UI actions"
        );
        fixture.removal_result.store(kResultOk, Ordering::SeqCst);
        assert!(
            matches!(
                fixture.close(),
                Err(Error::EditorDetachFailed {
                    stage: crate::error::EditorDetachStage::Removal,
                    result
                }) if result == kResultFalse
            ),
            "a later source change does not prove the first failed remove completed"
        );
        assert_eq!(fixture.removals.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.frame_clears.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failed_frame_clear_retains_exact_native_step_until_real_cleanup() {
        let mut fixture = Fixture::new(kResultOk, kResultFalse);
        for _ in 0..2 {
            assert!(matches!(
                fixture.close(),
                Err(Error::EditorDetachFailed {
                    stage: crate::error::EditorDetachStage::FrameClear,
                    result
                }) if result == kResultFalse
            ));
        }
        fixture.frame_result.store(kResultOk, Ordering::SeqCst);
        fixture
            .close()
            .expect("actual native frame cleanup accepted");
        assert_eq!(
            fixture.removals.load(Ordering::SeqCst),
            1,
            "successful removed cannot be called twice"
        );
        assert_eq!(fixture.frame_clears.load(Ordering::SeqCst), 3);
    }
}

pub(crate) fn native_editor_processing_layouts() -> [std::alloc::Layout; 2] {
    [
        std::alloc::Layout::new::<PluginEditorFields<'static>>(),
        std::alloc::Layout::new::<PluginProcessFields<'static>>(),
    ]
}

#[test]
#[ignore = "requires the existing owned Note-oracle SDK DLL; no GUI or compatibility claim"]
fn native_stop_deactivates_loaded_and_retained_active_but_stopped_owners() {
    let path = std::path::PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_NOTE_ORACLE_PLUGIN").expect("existing owned SDK DLL"),
    );
    assert!(path.is_absolute() && path.is_file());
    let mut native = PluginImpl::load_class(
        &path,
        "6E4F05A12D8B4C639F017B42A4C2E611",
        crate::PluginInterfaceSupport::default(),
    )
    .unwrap();
    assert!(native.is_active && !native.processing_state());
    native.stop_processing().unwrap();
    assert!(!native.is_active && !native.processing_state());
    native.stop_processing().unwrap();
    assert!(!native.is_active && !native.processing_state());
    native.start_processing().unwrap();
    assert!(native.is_active && native.processing_state());
    native.reactivate_configuration_stopped().unwrap();
    assert!(native.is_active && !native.processing_state());
    native.stop_processing().unwrap();
    assert!(!native.is_active && !native.processing_state());
}

#[test]
#[ignore = "requires the existing owned Note-oracle DLL; SDK anchored/legacy mutator ownership only"]
fn native_anchored_musical_context_rejects_independent_tempo_and_keeps_explicit_meter_playing() {
    use crate::audio::{ProcessMusicalReference, ProcessTimelineContext};
    use IProcessContextRequirements_::Flags_ as R;
    use ProcessContext_::StatesAndFlags_ as S;
    let path = std::path::PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_NOTE_ORACLE_PLUGIN").expect("existing owned SDK DLL"),
    );
    assert!(path.is_absolute() && path.is_file());
    let mut native = PluginImpl::load_class(
        &path,
        "6E4F05A12D8B4C639F017B42A4C2E611",
        crate::PluginInterfaceSupport::default(),
    )
    .unwrap();
    native.set_audio_config(48_000.0, 4);
    native.start_processing().unwrap();
    assert_eq!(
        native.process_data.as_ref().unwrap().musical_reference,
        None
    );
    let reference = ProcessMusicalReference::new(-1, 7, 3, 4).unwrap();
    native
        .set_process_musical_timeline_context(
            ProcessTimelineContext {
                sample_position: -17,
                sample_rate: 48_000,
            },
            reference,
        )
        .unwrap();
    let before = {
        let data = native.process_data.as_ref().unwrap();
        (
            native.tempo,
            data.transport_tempo,
            data.process_context.tempo,
            data.process_context.projectTimeMusic,
            data.process_context.state,
        )
    };
    assert!(matches!(
        native.set_tempo(140.0),
        Err(Error::MusicalTime(MusicalTimeError::AnchoredTempo))
    ));
    let data = native.process_data.as_ref().unwrap();
    assert_eq!(
        (
            native.tempo,
            data.transport_tempo,
            data.process_context.tempo,
            data.process_context.projectTimeMusic,
            data.process_context.state
        ),
        before
    );
    // These independent explicit SDK caller facts remain legal. They are not
    // inferred from the reference and do not establish product meter/transport.
    native.set_time_signature(7, 8).unwrap();
    native.set_playing(true).unwrap();
    let data = native.process_data.as_ref().unwrap();
    assert_eq!(data.musical_reference, Some(reference));
    assert_eq!(data.process_context.tempo, before.2);
    assert_eq!(data.process_context.projectTimeMusic, before.3);
    assert_eq!(
        (
            native.time_sig_numerator,
            native.time_sig_denominator,
            native.playing
        ),
        (7, 8, true)
    );
    if process_context_needs(
        data.process_context_requirements,
        R::kNeedTimeSignature as u32,
    ) {
        assert_eq!(
            (
                data.process_context.timeSigNumerator,
                data.process_context.timeSigDenominator
            ),
            (7, 8)
        );
        assert_ne!(data.process_context.state & S::kTimeSigValid as u32, 0);
    }
    assert_eq!(
        data.process_context.state & S::kPlaying as u32 != 0,
        process_context_needs(
            data.process_context_requirements,
            R::kNeedTransportState as u32
        )
    );
    assert_eq!(data.process_context.barPositionMusic, 0.0);
    assert_eq!(data.process_context.state & S::kBarPositionValid as u32, 0);
    native
        .set_process_timeline_context(ProcessTimelineContext {
            sample_position: -5,
            sample_rate: 48_000,
        })
        .unwrap();
    assert_eq!(
        native.process_data.as_ref().unwrap().musical_reference,
        None
    );
    native.set_tempo(140.0).unwrap();
    assert_eq!(native.tempo, 140.0);
    native.stop_processing().unwrap();
    native.start_processing().unwrap();
    assert_eq!(
        native.process_data.as_ref().unwrap().musical_reference,
        None
    );
}

#[test]
#[ignore = "requires rebuilt MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN mode7; actual DLL DSP literal music/PCM, not Audio VST transport admission"]
fn native_anchored_musical_context_reaches_sidechain_dsp_with_literal_bits() {
    use crate::audio::{BusDirection, MediaType, ProcessMusicalReference, ProcessTimelineContext};
    use crate::{Plugin, Vst3Host};

    // Only the existing state envelope is decoded here. Q/tempo expectations
    // live as independent literal bits inside the actual DLL's process callback.
    fn observe(plugin: &Plugin) -> (Vec<u8>, f64, u64) {
        let bytes = plugin
            .save_state()
            .expect("same component after scoped DSP join");
        let state = decode_state_snapshot(&bytes).unwrap();
        assert_eq!(state.component.len(), 32);
        assert_eq!(&state.component[..4], b"MSS1");
        assert_eq!(&state.component[4..8], &1_u32.to_le_bytes());
        assert_eq!(&state.component[16..20], &7_u32.to_le_bytes());
        assert_eq!(&state.component[20..24], b"MSA7");
        let controller = state.controller.as_ref().expect("separate controller");
        assert_eq!(controller.len(), 20);
        assert_eq!(&controller[..8], &state.component[..8]);
        assert_eq!(&controller[16..20], &state.component[16..20]);
        let gain = f64::from_le_bytes(state.component[8..16].try_into().unwrap());
        let attempts = u64::from_le_bytes(state.component[24..32].try_into().unwrap());
        (bytes, gain, attempts)
    }
    fn invalid_rate(result: Result<()>, anchored: bool) {
        if anchored {
            assert!(matches!(
                result,
                Err(Error::MusicalTime(MusicalTimeError::SampleRate))
            ));
        } else {
            assert!(matches!(result, Err(Error::InvalidParameter(_))));
        }
    }

    let path = std::path::PathBuf::from(
        std::env::var_os("MONDRIAN_VST3_SIDECHAIN_TEST_PLUGIN")
            .expect("rebuilt existing sidechain DLL with independent mode7"),
    );
    assert!(path.is_absolute() && path.is_file());
    for rate in [44_100_u32, 48_000] {
        let mut host = Vst3Host::builder()
            .sample_rate(f64::from(rate))
            .block_size(4)
            .input_channels(2)
            .output_channels(2)
            .build()
            .unwrap();
        for scoped in [false, true] {
            for anchored in [false, true] {
                for start in [-17, i64::MAX - 32] {
                    let mut plugin = host
                        .load_plugin_class(&path, "7486AF22234A480B9DF13173F12EB620")
                        .expect("actual existing sidechain class");
                    let mut configuration = b"MSS1".to_vec();
                    configuration.extend_from_slice(&1_u32.to_le_bytes());
                    configuration.extend_from_slice(&0.5_f64.to_le_bytes());
                    configuration.extend_from_slice(&7_u32.to_le_bytes());
                    let state = encode_state_snapshot(&StateSnapshot {
                        component: configuration.clone(),
                        controller: Some(configuration),
                    })
                    .unwrap();
                    plugin.load_state(&state).unwrap();
                    for index in 0..3 {
                        plugin
                            .set_bus_active(MediaType::Audio, BusDirection::Input, index, true)
                            .unwrap();
                    }
                    plugin
                        .set_bus_active(MediaType::Audio, BusDirection::Output, 0, true)
                        .unwrap();
                    assert_eq!(plugin.latency_samples(), 3);
                    plugin.start_processing().unwrap();
                    let (initial_state, initial_gain, initial_attempts) = observe(&plugin);
                    assert_eq!((initial_gain, initial_attempts), (0.5, 0));
                    let context = ProcessTimelineContext {
                        sample_position: start,
                        sample_rate: rate,
                    };
                    let reference =
                        ProcessMusicalReference::new(start - 1, i64::from(rate), 3, 4).unwrap();
                    let overflow_context = ProcessTimelineContext {
                        sample_position: i64::MAX - 6,
                        sample_rate: rate,
                    };
                    let overflow_reference =
                        ProcessMusicalReference::new(i64::MAX - 7, i64::from(rate), 3, 4).unwrap();
                    let bad_context = ProcessTimelineContext {
                        sample_position: start,
                        sample_rate: if rate == 44_100 { 48_000 } else { 44_100 },
                    };
                    // Two complete native windows use the same object. Window0
                    // observes the retained context after a refused setter;
                    // window1 proves a refused overflow consumed no queued point
                    // or DSP call. A successful position-only setter then clears
                    // the same object's prior anchor before actual None DSP.
                    for pass in 0..2 {
                        let mut buffers = plugin.create_bus_audio_buffers(9).unwrap();
                        assert_eq!((buffers.inputs.len(), buffers.outputs.len()), (3, 1));
                        for (channel, samples) in buffers.inputs[0].channels.iter_mut().enumerate()
                        {
                            samples.fill(if channel == 0 { 1.0 } else { -1.0 });
                        }
                        for samples in &mut buffers.outputs[0].channels {
                            samples.fill(-99.0);
                        }
                        let unmodified = buffers.clone();
                        plugin
                            .set_parameter_at(0, if pass == 0 { 0.75 } else { 0.25 }, 0)
                            .unwrap();
                        if scoped {
                            let split = plugin.split_editor_processing().unwrap();
                            let mut processor = split.processor;
                            std::thread::scope(|scope| {
                                scope
                                    .spawn(|| {
                                        if anchored && pass == 0 {
                                            processor
                                                .set_process_musical_timeline_context(
                                                    context, reference,
                                                )
                                                .unwrap();
                                        } else {
                                            processor
                                                .set_process_timeline_context(context)
                                                .unwrap();
                                        }
                                        if pass == 0 {
                                            invalid_rate(
                                                if anchored {
                                                    processor.set_process_musical_timeline_context(
                                                        bad_context,
                                                        reference,
                                                    )
                                                } else {
                                                    processor
                                                        .set_process_timeline_context(bad_context)
                                                },
                                                anchored,
                                            );
                                            assert_eq!(buffers, unmodified);
                                        } else {
                                            if anchored {
                                                processor
                                                    .set_process_musical_timeline_context(
                                                        overflow_context,
                                                        overflow_reference,
                                                    )
                                                    .unwrap();
                                            } else {
                                                processor
                                                    .set_process_timeline_context(overflow_context)
                                                    .unwrap();
                                            }
                                            assert!(matches!(
                                                processor.process_bus_audio(&mut buffers),
                                                Err(Error::NativeEvent(
                                                    NativeEventError::InvalidFrames
                                                ))
                                            ));
                                            assert_eq!(buffers, unmodified);
                                            processor
                                                .set_process_timeline_context(context)
                                                .unwrap();
                                        }
                                        processor.process_bus_audio(&mut buffers).unwrap();
                                    })
                                    .join()
                                    .expect("joined real scoped DSP thread");
                            });
                            drop(processor);
                            drop(split.editor);
                        } else {
                            if anchored && pass == 0 {
                                plugin
                                    .set_process_musical_timeline_context(context, reference)
                                    .unwrap();
                            } else {
                                plugin.set_process_timeline_context(context).unwrap();
                            }
                            if pass == 0 {
                                invalid_rate(
                                    if anchored {
                                        plugin.set_process_musical_timeline_context(
                                            bad_context,
                                            reference,
                                        )
                                    } else {
                                        plugin.set_process_timeline_context(bad_context)
                                    },
                                    anchored,
                                );
                                assert_eq!(buffers, unmodified);
                            } else {
                                if anchored {
                                    plugin
                                        .set_process_musical_timeline_context(
                                            overflow_context,
                                            overflow_reference,
                                        )
                                        .unwrap();
                                } else {
                                    plugin
                                        .set_process_timeline_context(overflow_context)
                                        .unwrap();
                                }
                                assert!(matches!(
                                    plugin.process_bus_audio(&mut buffers),
                                    Err(Error::NativeEvent(NativeEventError::InvalidFrames))
                                ));
                                assert_eq!(buffers, unmodified);
                                plugin.set_process_timeline_context(context).unwrap();
                            }
                            plugin.process_bus_audio(&mut buffers).unwrap();
                        }
                        let (_, gain, attempts) = observe(&plugin);
                        assert_eq!(
                            attempts,
                            if pass == 0 { 3 } else { 6 },
                            "refused call cannot reach DLL process"
                        );
                        assert_eq!(
                            gain.to_bits(),
                            if pass == 0 { 0.75_f64 } else { 0.25_f64 }.to_bits()
                        );
                        for (channel, samples) in buffers.outputs[0].channels.iter().enumerate() {
                            for (frame, actual) in samples.iter().enumerate() {
                                // Independent literal PCM: three-frame delay of
                                // main-input * 2 * the retained parameter gain,
                                // plus a per-native-chunk mode/rate tag.
                                let row = frame / 4;
                                let tag =
                                    (1 + row + if rate == 44_100 { 0 } else { 4 }) as f32 / 16.0;
                                let tag = if anchored && pass == 0 { tag } else { -tag };
                                let delayed = match (pass, frame < 3) {
                                    (0, true) => 0.0,
                                    (0, false) | (1, true) => 1.5,
                                    (1, false) => 0.5,
                                    _ => unreachable!(),
                                };
                                let expected = delayed + tag;
                                let expected = if channel == 0 { expected } else { -expected };
                                assert_eq!(actual.to_bits(), expected.to_bits(), "rate={rate} start={start} scoped={scoped} anchored={anchored} pass={pass} channel={channel} frame={frame}");
                            }
                        }
                    }
                    if anchored {
                        let mut guarded = plugin.create_bus_audio_buffers(9).unwrap();
                        for (channel, samples) in guarded.inputs[0].channels.iter_mut().enumerate()
                        {
                            samples.fill(if channel == 0 { 1.0 } else { -1.0 });
                        }
                        for samples in &mut guarded.outputs[0].channels {
                            samples.fill(-99.0);
                        }
                        let unmodified = guarded.clone();
                        plugin
                            .set_process_musical_timeline_context(context, reference)
                            .unwrap();
                        plugin
                            .send_plugin_event(PluginEvent {
                                bus_index: 0,
                                sample_offset: 0,
                                ppq_position: 0.0,
                                flags: 0,
                                data: crate::midi::PluginEventData::NoteOn {
                                    channel: 0,
                                    pitch: 60,
                                    tuning: 0.0,
                                    velocity: 0.5,
                                    length: 0,
                                    note_id: 777,
                                },
                            })
                            .unwrap();
                        plugin.set_parameter_at(0, 0.75, 0).unwrap();
                        let result = if scoped {
                            let split = plugin.split_editor_processing().unwrap();
                            let mut processor = split.processor;
                            let result = std::thread::scope(|scope| {
                                scope
                                    .spawn(|| processor.process_bus_audio(&mut guarded))
                                    .join()
                                    .unwrap()
                            });
                            drop(processor);
                            drop(split.editor);
                            result
                        } else {
                            plugin.process_bus_audio(&mut guarded)
                        };
                        assert!(matches!(
                            result,
                            Err(Error::NativeEvent(NativeEventError::PendingEvents))
                        ));
                        assert_eq!(guarded, unmodified);
                        assert!(
                            matches!(plugin.save_state(), Err(Error::PendingParameterDelivery)),
                            "refusal must retain the original queued parameter"
                        );
                        // Explicit SDK caller recovery preserves the original None
                        // event route. This fixture does not implement Note DSP:
                        // only the Some rejection, PCM and actual call count qualify.
                        plugin.set_process_timeline_context(context).unwrap();
                        if scoped {
                            let split = plugin.split_editor_processing().unwrap();
                            let mut processor = split.processor;
                            std::thread::scope(|scope| {
                                scope
                                    .spawn(|| processor.process_bus_audio(&mut guarded))
                                    .join()
                                    .unwrap()
                            })
                            .unwrap();
                            drop(processor);
                            drop(split.editor);
                        } else {
                            plugin.process_bus_audio(&mut guarded).unwrap();
                        }
                        let (_, gain, attempts) = observe(&plugin);
                        assert_eq!(
                            (gain, attempts),
                            (0.75, 9),
                            "only the explicit valid recovery's three calls reached the same DLL"
                        );
                        for (channel, samples) in guarded.outputs[0].channels.iter().enumerate() {
                            for (frame, actual) in samples.iter().enumerate() {
                                let tag = (1 + frame / 4 + if rate == 44_100 { 0 } else { 4 })
                                    as f32
                                    / 16.0;
                                let expected = if frame < 3 { 0.5 } else { 1.5 } - tag;
                                let expected = if channel == 0 { expected } else { -expected };
                                assert_eq!(actual.to_bits(), expected.to_bits());
                            }
                        }
                    }
                    plugin.stop_processing().unwrap();
                    // Restore the earlier real same-object configuration with a
                    // recorded count0. Audit is intentionally not DSP state:
                    // original gain restores; actual attempt history remains.
                    plugin.load_state(&initial_state).unwrap();
                    let (_, gain, attempts) = observe(&plugin);
                    assert_eq!((gain, attempts), (0.5, if anchored { 9 } else { 6 }));
                    plugin.stop_processing().unwrap();
                }
            }
        }
    }
}

#[cfg(test)]
mod cache_policy_tests;

#[cfg(test)]
mod initial_parameter_input_tests;

#[cfg(test)]
#[allow(clippy::unnecessary_cast)]
mod checked_native_note_tests {
    use super::*;
    use crate::audio::{AudioBusConfig, NativeEventBusRole};
    use crate::midi::PluginEventData;
    use crate::plugin::PluginProcessingRole;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use vst3::{Class, ComRef};

    type Received = (i32, i32, i32, i16, i16, u32);
    #[derive(Default)]
    struct Observations {
        calls: usize,
        received: Vec<Vec<Received>>,
        frames: Vec<i32>,
        positions: Vec<(i64, i64, u32)>,
        musical: Vec<[u64; 5]>,
    }
    struct Processor {
        observed: Arc<Mutex<Observations>>,
        fail_call: Arc<AtomicUsize>,
        context_fault: Arc<AtomicUsize>,
        inject_legacy: Arc<AtomicBool>,
        original_ingress: ComPtr<IEventList>,
    }
    impl Class for Processor {
        type Interfaces = (IAudioProcessor,);
    }
    impl IAudioProcessorTrait for Processor {
        unsafe fn setBusArrangements(&self, _: *mut u64, _: i32, _: *mut u64, _: i32) -> tresult {
            kResultOk
        }
        unsafe fn getBusArrangement(&self, _: i32, _: i32, arr: *mut u64) -> tresult {
            unsafe {
                *arr = 1;
            }
            kResultOk
        }
        unsafe fn canProcessSampleSize(&self, _: i32) -> tresult {
            kResultOk
        }
        unsafe fn getLatencySamples(&self) -> u32 {
            0
        }
        unsafe fn setupProcessing(&self, _: *mut ProcessSetup) -> tresult {
            kResultOk
        }
        unsafe fn setProcessing(&self, _: u8) -> tresult {
            kResultOk
        }
        unsafe fn getTailSamples(&self) -> u32 {
            0
        }
        unsafe fn process(&self, data: *mut ProcessData) -> tresult {
            // SAFETY: the actual SDK entry receives the fixture's wired stable
            // ProcessData, event COM owner and mono output storage.
            let data = unsafe { &mut *data };
            if self.inject_legacy.swap(false, Ordering::Relaxed) {
                let mut late: Event = unsafe { std::mem::zeroed() };
                late.r#type = kNoteOnEvent as u16;
                late.ppqPosition = 0.0;
                late.__field0.noteOn = NoteOnEvent {
                    channel: 2,
                    pitch: 60,
                    tuning: 0.0,
                    velocity: 0.5,
                    length: 0,
                    noteId: 999,
                };
                assert_eq!(
                    unsafe { self.original_ingress.addEvent(&mut late) },
                    kResultOk
                );
            }
            let mut received = Vec::new();
            let events = unsafe { ComRef::from_raw(data.inputEvents) };
            for index in 0..events
                .as_ref()
                .map_or(0, |events| unsafe { events.getEventCount() })
            {
                let events = events.as_ref().unwrap();
                let mut event: Event = unsafe { std::mem::zeroed() };
                assert_eq!(unsafe { events.getEvent(index, &mut event) }, kResultOk);
                let (id, channel, pitch, velocity) = match event.r#type as u32 {
                    value if value == kNoteOnEvent as u32 => {
                        let note = unsafe { event.__field0.noteOn };
                        (note.noteId, note.channel, note.pitch, note.velocity)
                    }
                    value if value == kNoteOffEvent as u32 => {
                        let note = unsafe { event.__field0.noteOff };
                        (note.noteId, note.channel, note.pitch, note.velocity)
                    }
                    _ => panic!("only the admitted scalar Note subset"),
                };
                received.push((
                    event.sampleOffset,
                    event.busIndex,
                    id,
                    channel,
                    pitch,
                    velocity.to_bits(),
                ));
            }
            let mut observed = self.observed.lock().unwrap();
            observed.calls += 1;
            let call = observed.calls;
            observed.received.push(received);
            observed.frames.push(data.numSamples);
            let context = unsafe { &*data.processContext };
            observed.positions.push((
                context.projectTimeSamples,
                context.continousTimeSamples,
                context.state,
            ));
            observed.musical.push([
                context.projectTimeMusic.to_bits(),
                context.tempo.to_bits(),
                context.barPositionMusic.to_bits(),
                context.timeSigNumerator as u64,
                context.timeSigDenominator as u64,
            ]);
            // Adversarial native mutation: the host must reject an unrepresentable
            // next sample position without skipping per-call cleanup.
            match self.context_fault.load(Ordering::Relaxed) {
                1 => unsafe { (*data.processContext).projectTimeSamples = i64::MAX },
                2 => unsafe { (*data.processContext).continousTimeSamples = i64::MAX },
                3 => unsafe { (*data.processContext).sampleRate = 44_100.0 },
                _ => {}
            }
            // A failed native block deliberately writes native output too. It
            // must not become caller PCM, unlike the preceding successful chunk.
            if data.numSamples != 0 {
                let output = unsafe { &mut *data.outputs };
                output.silenceFlags = 0;
                let samples = unsafe {
                    std::slice::from_raw_parts_mut(
                        *output.__field0.channelBuffers32,
                        data.numSamples as usize,
                    )
                };
                samples.fill(call as f32 * 0.25);
            }
            if self.fail_call.load(Ordering::Relaxed) == call {
                kResultFalse
            } else {
                kResultOk
            }
        }
    }

    struct Fixture {
        processor: ComPtr<IAudioProcessor>,
        observed: Arc<Mutex<Observations>>,
        fail_call: Arc<AtomicUsize>,
        context_fault: Arc<AtomicUsize>,
        inject_legacy: Arc<AtomicBool>,
        data: Option<Box<HostProcessData>>,
        activation: BusActivationState,
        handler: Option<ComWrapper<ComponentHandler>>,
        parameters: Vec<ParameterChange>,
        input: ComWrapper<HostEventList>,
        output: ComWrapper<HostEventList>,
        chunks: Vec<Option<PluginEvent>>,
        gui: Arc<Mutex<Vec<(u32, f64)>>>,
        feedback: Arc<ArrayQueue<(u32, f64)>>,
        emitted: Arc<ArrayQueue<PluginEvent>>,
        deferred: ArrayQueue<(u32, f64)>,
        host: ComWrapper<HostApplication>,
    }
    impl Fixture {
        fn new() -> Self {
            let observed = Arc::new(Mutex::new(Observations::default()));
            let fail_call = Arc::new(AtomicUsize::new(0));
            let context_fault = Arc::new(AtomicUsize::new(0));
            let inject_legacy = Arc::new(AtomicBool::new(false));
            let input = create_event_list();
            let processor = ComWrapper::new(Processor {
                observed: Arc::clone(&observed),
                fail_call: Arc::clone(&fail_call),
                context_fault: Arc::clone(&context_fault),
                inject_legacy: Arc::clone(&inject_legacy),
                original_ingress: input.to_com_ptr::<IEventList>().unwrap(),
            })
            .to_com_ptr::<IAudioProcessor>()
            .unwrap();
            let output = create_event_list();
            let mut native_output: AudioBusBuffers = unsafe { std::mem::zeroed() };
            native_output.numChannels = 1;
            let mut native_outputs = vec![native_output];
            let mut samples =
                build_typed_sample_buffers::<f32>(4, &[], &[], &native_outputs, &[true]);
            native_outputs[0].__field0.channelBuffers32 =
                samples.output_channel_ptrs.0[0].as_mut_ptr();
            let mut data = Box::new(HostProcessData {
                process_data: unsafe { std::mem::zeroed() },
                sample_buffers: HostSampleBuffers::F32(samples),
                input_bus_buffers: vec![],
                output_bus_buffers: native_outputs,
                process_context: unsafe { std::mem::zeroed() },
                process_context_requirements: None,
                transport_tempo: 120.0,
                musical_reference: None,
                input_param_changes: ComWrapper::new(ParameterChanges::default()),
                output_param_changes: ComWrapper::new(ParameterChanges::default()),
            });
            data.process_context.sampleRate = 48_000.0;
            data.process_data.numOutputs = 1;
            data.process_data.outputs = data.output_bus_buffers.as_mut_ptr();
            data.process_data.symbolicSampleSize = SymbolicSampleSizes_::kSample32 as i32;
            data.process_data.inputEvents = input.as_com_ref::<IEventList>().unwrap().as_ptr();
            data.process_data.outputEvents = output.as_com_ref::<IEventList>().unwrap().as_ptr();
            data.process_data.inputParameterChanges = data
                .input_param_changes
                .as_com_ref::<IParameterChanges>()
                .unwrap()
                .as_ptr();
            data.process_data.outputParameterChanges = data
                .output_param_changes
                .as_com_ref::<IParameterChanges>()
                .unwrap()
                .as_ptr();
            data.process_data.processContext = &mut data.process_context;
            Self {
                processor,
                observed,
                fail_call,
                context_fault,
                inject_legacy,
                data: Some(data),
                activation: BusActivationState {
                    audio_outputs: vec![true],
                    event_inputs: vec![
                        EventBusActivation {
                            role: NativeEventBusRole::Main,
                            channel_count: 16,
                            active: true,
                        },
                        EventBusActivation {
                            role: NativeEventBusRole::Auxiliary,
                            channel_count: 1,
                            active: true,
                        },
                    ],
                    event_readiness: Some(Ok(())),
                    ..BusActivationState::default()
                },
                handler: None,
                parameters: Vec::with_capacity(4),
                input,
                output,
                chunks: Vec::with_capacity(4096),
                gui: Arc::new(Mutex::new(Vec::new())),
                feedback: Arc::new(ArrayQueue::new(16)),
                emitted: Arc::new(ArrayQueue::new(16)),
                deferred: ArrayQueue::new(16),
                host: super::super::com_implementations::create_host_application(),
            }
        }
        fn fields(&mut self) -> PluginProcessFields<'_> {
            PluginProcessFields {
                processor: &self.processor,
                is_active: true,
                is_processing: true,
                sample_rate: 48_000.0,
                block_size: 4,
                process_data: &mut self.data,
                bus_activation: &self.activation,
                component_handler: &self.handler,
                pending_param_changes: &mut self.parameters,
                input_events: &self.input,
                output_events: &self.output,
                chunk_events: &mut self.chunks,
                gui_param_changes_for_host: &self.gui,
                output_param_feedback: &self.feedback,
                output_events_owned: &self.emitted,
                deferred_controller_sync: &self.deferred,
                host_app: &self.host,
            }
        }
        fn run(&mut self, buffers: &mut BusAudioBuffers, notes: &[PluginEvent]) -> Result<()> {
            std::thread::scope(|scope| {
                let mut fields = self.fields();
                scope
                    .spawn(move || fields.process_bus_audio_with_note_events(buffers, notes))
                    .join()
                    .expect("actual scoped processing thread")
            })
        }
    }
    fn buffers(frames: usize) -> BusAudioBuffers {
        BusAudioBuffers::new(
            &AudioBusLayout {
                inputs: vec![],
                outputs: vec![AudioBusConfig {
                    channel_count: 1,
                    active: true,
                }],
            },
            frames,
            48_000.0,
        )
    }
    fn on(offset: i32, id: i32) -> PluginEvent {
        PluginEvent {
            bus_index: 0,
            sample_offset: offset,
            ppq_position: 0.0,
            flags: 0,
            data: PluginEventData::NoteOn {
                channel: 2,
                pitch: 60,
                tuning: 0.25,
                velocity: 0.0,
                length: 0,
                note_id: id,
            },
        }
    }
    fn off(offset: i32, id: i32) -> PluginEvent {
        PluginEvent {
            bus_index: 0,
            sample_offset: offset,
            ppq_position: 0.0,
            flags: 0,
            data: PluginEventData::NoteOff {
                channel: 2,
                pitch: 60,
                tuning: 0.25,
                velocity: 0.5,
                note_id: id,
            },
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum WindowPath {
        Flat,
        Buses,
        ScopedNotes,
    }

    fn run_sample_window(
        fixture: &mut Fixture,
        path: WindowPath,
        frames: usize,
    ) -> (Result<()>, Vec<f32>) {
        match path {
            WindowPath::Flat => {
                let mut pcm = AudioBuffers::new(0, 1, frames, 48_000.0);
                pcm.outputs[0].fill(93.0);
                let result = fixture
                    .fields()
                    .process_buffer_view(&mut CallerAudioBuffers::Flat(&mut pcm));
                (result, pcm.outputs.remove(0))
            }
            WindowPath::Buses | WindowPath::ScopedNotes => {
                let mut pcm = buffers(frames);
                pcm.outputs[0].channels[0].fill(93.0);
                let result = match path {
                    WindowPath::Buses => fixture.fields().process_bus_audio(&mut pcm),
                    WindowPath::ScopedNotes => {
                        fixture.run(&mut pcm, &[on(0, 71), off((frames - 1) as i32, 71)])
                    }
                    WindowPath::Flat => unreachable!(),
                };
                (result, pcm.outputs.remove(0).channels.remove(0))
            }
        }
    }

    // All fields of the pinned SDK POD, without reading uninitialized padding.
    fn context_facts(ctx: &ProcessContext) -> [u64; 19] {
        [
            ctx.state as u64,
            ctx.sampleRate.to_bits(),
            ctx.projectTimeSamples as u64,
            ctx.systemTime as u64,
            ctx.continousTimeSamples as u64,
            ctx.projectTimeMusic.to_bits(),
            ctx.barPositionMusic.to_bits(),
            ctx.cycleStartMusic.to_bits(),
            ctx.cycleEndMusic.to_bits(),
            ctx.tempo.to_bits(),
            ctx.timeSigNumerator as u64,
            ctx.timeSigDenominator as u64,
            ctx.chord.keyNote as u64,
            ctx.chord.rootNote as u64,
            ctx.chord.chordMask as u64,
            ctx.smpteOffsetSubframes as u64,
            ctx.frameRate.framesPerSecond as u64,
            ctx.frameRate.flags as u64,
            ctx.samplesToNextClock as u64,
        ]
    }

    #[test]
    fn sample_window_overflow_preserves_context_queues_pcm_and_calls_no_processor() {
        use ProcessContext_::StatesAndFlags_ as S;
        for path in [WindowPath::Flat, WindowPath::Buses, WindowPath::ScopedNotes] {
            for continuous in [false, true] {
                let mut fixture = Fixture::new();
                let data = fixture.data.as_mut().unwrap();
                // First native chunk (4) would fit exactly; the complete 9-frame
                // window would overflow only in a later chunk. No prefix may run.
                data.process_context.projectTimeSamples =
                    if continuous { -17 } else { i64::MAX - 4 };
                data.process_context.continousTimeSamples = i64::MAX - 4;
                data.process_context.state = if continuous {
                    S::kContTimeValid as u32
                } else {
                    0
                };
                let context = context_facts(&data.process_context);
                fixture.parameters.push(ParameterChange {
                    id: 7,
                    value: 0.5,
                    sample_offset: 8,
                });
                fixture.gui.lock().unwrap().push((17, 0.25));
                fixture.output.events.lock().unwrap().push(off(0, 81));
                if !matches!(path, WindowPath::ScopedNotes) {
                    fixture.input.events.lock().unwrap().push(on(0, 91));
                    fixture.chunks.push(Some(off(0, 92)));
                }
                let input = fixture.input.events.lock().unwrap().clone();
                let output = fixture.output.events.lock().unwrap().clone();
                let chunks = fixture.chunks.clone();
                let input_owner = {
                    let owner = fixture.input.events.lock().unwrap();
                    (owner.as_ptr(), owner.capacity())
                };
                let chunk_owner = (fixture.chunks.as_ptr(), fixture.chunks.capacity());
                let parameter_owner = (fixture.parameters.as_ptr(), fixture.parameters.capacity());
                let (result, pcm) = run_sample_window(&mut fixture, path, 9);
                assert!(
                    matches!(
                        result,
                        Err(Error::NativeEvent(NativeEventError::InvalidFrames))
                    ),
                    "{path:?} continuous={continuous}: {result:?}"
                );
                assert_eq!(pcm, vec![93.0; 9]);
                assert_eq!(fixture.observed.lock().unwrap().calls, 0);
                assert_eq!(
                    context_facts(&fixture.data.as_ref().unwrap().process_context),
                    context
                );
                assert_eq!(*fixture.input.events.lock().unwrap(), input);
                assert_eq!(*fixture.output.events.lock().unwrap(), output);
                assert_eq!(fixture.chunks, chunks);
                assert_eq!(
                    (fixture.chunks.as_ptr(), fixture.chunks.capacity()),
                    chunk_owner
                );
                let owner = fixture.input.events.lock().unwrap();
                assert_eq!((owner.as_ptr(), owner.capacity()), input_owner);
                assert_eq!(
                    (fixture.parameters.as_ptr(), fixture.parameters.capacity()),
                    parameter_owner
                );
                assert_eq!(fixture.parameters.len(), 1);
                let parameter = &fixture.parameters[0];
                assert_eq!(
                    (
                        parameter.id,
                        parameter.value.to_bits(),
                        parameter.sample_offset
                    ),
                    (7, 0.5f64.to_bits(), 8)
                );
                assert_eq!(*fixture.gui.lock().unwrap(), [(17, 0.25)]);
            }
        }
    }

    #[test]
    fn changed_native_sample_context_fails_cleanly_without_replay_and_preserves_primary_error() {
        for fault in [1, 2] {
            for native_failure in [false, true] {
                let mut fixture = Fixture::new();
                fixture.context_fault.store(fault, Ordering::Relaxed);
                if native_failure {
                    fixture.fail_call.store(1, Ordering::Relaxed);
                }
                let context = &mut fixture.data.as_mut().unwrap().process_context;
                context.projectTimeSamples = -17;
                context.continousTimeSamples = -41;
                context.state = ProcessContext_::StatesAndFlags_::kContTimeValid as u32;
                fixture.parameters.push(ParameterChange {
                    id: 7,
                    value: 0.5,
                    sample_offset: 8,
                });
                let mut pcm = buffers(9);
                pcm.outputs[0].channels[0].fill(93.0);
                let result = fixture.run(&mut pcm, &[on(0, 71), off(8, 71)]);
                if native_failure {
                    assert!(
                        matches!(result, Err(Error::ProcessFailed(value)) if value == kResultFalse)
                    );
                } else {
                    assert!(matches!(
                        result,
                        Err(Error::NativeEvent(NativeEventError::InvalidFrames))
                    ));
                }
                assert_eq!(fixture.observed.lock().unwrap().frames, [4]);
                assert_eq!(pcm.outputs[0].channels[0], [93.0; 9]);
                let context = &fixture.data.as_ref().unwrap().process_context;
                assert_eq!(
                    context.projectTimeSamples,
                    if fault == 1 { i64::MAX } else { -17 }
                );
                assert_eq!(
                    context.continousTimeSamples,
                    if fault == 2 { i64::MAX } else { -41 }
                );
                assert!(fixture.input.events.lock().unwrap().is_empty());
                assert!(fixture.chunks.is_empty());
                assert!(fixture.parameters.is_empty());
                fixture.context_fault.store(0, Ordering::Relaxed);
                fixture.fail_call.store(0, Ordering::Relaxed);
                fixture
                    .fields()
                    .set_process_timeline_context(crate::audio::ProcessTimelineContext {
                        sample_position: 100,
                        sample_rate: 48_000,
                    })
                    .unwrap();
                fixture.run(&mut buffers(4), &[]).unwrap();
                let observed = fixture.observed.lock().unwrap();
                assert_eq!(observed.frames, [4, 4]);
                assert!(
                    observed.received[1].is_empty(),
                    "failed window's later Note must not replay"
                );
                assert_eq!(observed.positions[1], (100, 0, 0));
            }
        }
    }

    #[test]
    fn signed_sample_windows_keep_negative_partial_chunks_and_maximum_exclusive_end() {
        use ProcessContext_::StatesAndFlags_ as S;
        for path in [WindowPath::Flat, WindowPath::Buses, WindowPath::ScopedNotes] {
            for start in [-17, i64::MAX - 9] {
                let mut fixture = Fixture::new();
                let context = &mut fixture.data.as_mut().unwrap().process_context;
                context.projectTimeSamples = start;
                context.continousTimeSamples = -41;
                context.state = S::kContTimeValid as u32;
                let (result, pcm) = run_sample_window(&mut fixture, path, 9);
                result.unwrap();
                assert_eq!(pcm, [0.25, 0.25, 0.25, 0.25, 0.5, 0.5, 0.5, 0.5, 0.75]);
                let observed = fixture.observed.lock().unwrap();
                assert_eq!(observed.frames, [4, 4, 1]);
                assert_eq!(
                    observed.positions,
                    [
                        (start, -41, S::kContTimeValid as u32),
                        (start + 4, -37, S::kContTimeValid as u32),
                        (start + 8, -33, S::kContTimeValid as u32),
                    ]
                );
                let context = &fixture.data.as_ref().unwrap().process_context;
                assert_eq!(context.projectTimeSamples, start + 9);
                assert_eq!(context.continousTimeSamples, -32);
            }
        }
    }

    #[test]
    fn unavailable_continuous_time_is_ignored_and_legacy_zero_flush_keeps_maximum_position() {
        for path in [WindowPath::Flat, WindowPath::Buses, WindowPath::ScopedNotes] {
            let mut fixture = Fixture::new();
            let context = &mut fixture.data.as_mut().unwrap().process_context;
            context.projectTimeSamples = -17;
            context.continousTimeSamples = i64::MAX;
            let (result, _) = run_sample_window(&mut fixture, path, 9);
            result.unwrap();
            let context = &fixture.data.as_ref().unwrap().process_context;
            assert_eq!(context.projectTimeSamples, -8);
            assert_eq!(context.continousTimeSamples, i64::MAX);
            assert_eq!(context.state, 0);
        }
        for path in [WindowPath::Flat, WindowPath::Buses] {
            let mut fixture = Fixture::new();
            fixture
                .data
                .as_mut()
                .unwrap()
                .process_context
                .projectTimeSamples = i64::MAX;
            let (result, pcm) = run_sample_window(&mut fixture, path, 0);
            result.unwrap();
            assert!(pcm.is_empty());
            let observed = fixture.observed.lock().unwrap();
            assert_eq!(observed.frames, [0]);
            assert_eq!(observed.positions, [(i64::MAX, 0, 0)]);
            assert_eq!(
                fixture
                    .data
                    .as_ref()
                    .unwrap()
                    .process_context
                    .projectTimeSamples,
                i64::MAX
            );
        }
    }

    #[test]
    fn checked_note_scoped_native_events_span_chunks_keep_original_tuple_and_equal_offsets() {
        let mut fixture = Fixture::new();
        let mut output = buffers(9);
        fixture
            .run(&mut output, &[on(1, 71), on(1, 72), off(4, 71), off(8, 72)])
            .unwrap();
        assert_eq!(fixture.observed.lock().unwrap().frames, [4, 4, 1]);
        assert_eq!(
            fixture.observed.lock().unwrap().received,
            [
                vec![(1, 0, 71, 2, 60, 0), (1, 0, 72, 2, 60, 0)],
                vec![(0, 0, 71, 2, 60, 0.5f32.to_bits())],
                vec![(0, 0, 72, 2, 60, 0.5f32.to_bits())],
            ]
        );
        assert_eq!(
            output.outputs[0].channels[0],
            [0.25, 0.25, 0.25, 0.25, 0.5, 0.5, 0.5, 0.5, 0.75]
        );
        assert!(fixture.input.events.lock().unwrap().is_empty());
        assert!(fixture.chunks.is_empty());
    }

    #[test]
    fn checked_note_second_chunk_failure_clears_remaining_input_and_does_not_replay() {
        let mut fixture = Fixture::new();
        fixture.fail_call.store(2, Ordering::Relaxed);
        fixture.parameters.push(ParameterChange {
            id: 7,
            value: 0.5,
            sample_offset: 8,
        });
        let mut output = buffers(9);
        output.outputs[0].channels[0].fill(93.0);
        assert!(
            matches!(fixture.run(&mut output, &[on(1, 71), off(4, 71), on(8, 72)]),
            Err(Error::ProcessFailed(result)) if result == kResultFalse)
        );
        assert_eq!(fixture.observed.lock().unwrap().frames, [4, 4]);
        assert_eq!(
            output.outputs[0].channels[0],
            [0.25, 0.25, 0.25, 0.25, 93.0, 93.0, 93.0, 93.0, 93.0]
        );
        assert!(fixture.input.events.lock().unwrap().is_empty());
        assert!(fixture.chunks.is_empty());
        assert!(fixture.parameters.is_empty());
        fixture.run(&mut buffers(4), &[]).unwrap();
        assert!(fixture.observed.lock().unwrap().received[2].is_empty());
    }

    #[test]
    fn checked_note_late_bad_event_refuses_before_pcm_parameters_or_native_call_change() {
        let mut fixture = Fixture::new();
        fixture.parameters.push(ParameterChange {
            id: 7,
            value: 0.5,
            sample_offset: 0,
        });
        let mut output = buffers(9);
        output.outputs[0].channels[0].fill(93.0);
        assert!(matches!(
            fixture.run(&mut output, &[on(0, 71), off(9, 71)]),
            Err(Error::NativeEvent(NativeEventError::InvalidEvent {
                index: 1
            }))
        ));
        assert_eq!(output.outputs[0].channels[0], vec![93.0; 9]);
        assert_eq!(fixture.parameters.len(), 1);
        assert!(fixture.input.events.lock().unwrap().is_empty());
        assert!(fixture.chunks.is_empty());
        assert_eq!(fixture.observed.lock().unwrap().calls, 0);
    }

    #[test]
    fn checked_note_bad_audio_window_preserves_all_input_and_parameter_owners() {
        let mut fixture = Fixture::new();
        fixture.parameters.push(ParameterChange {
            id: 7,
            value: 0.5,
            sample_offset: 0,
        });
        for case in 0..4 {
            let mut output = buffers(9);
            output.outputs[0].channels[0].fill(93.0);
            match case {
                0 => output.block_size = 8,
                1 => output.sample_rate = 44_100.0,
                2 => output.block_size = 0,
                _ => output.outputs[0].active = false,
            }
            assert!(matches!(
                fixture.run(&mut output, &[on(0, 71)]),
                Err(Error::NativeEvent(_))
            ));
            assert_eq!(output.outputs[0].channels[0], vec![93.0; 9]);
            assert_eq!(fixture.parameters.len(), 1);
            assert!(fixture.input.events.lock().unwrap().is_empty());
            assert!(fixture.chunks.is_empty());
        }
        assert_eq!(fixture.observed.lock().unwrap().calls, 0);
    }

    #[test]
    fn checked_note_capacity_legacy_and_poison_refusals_preserve_pcm_and_parameters() {
        for case in 0..4 {
            let mut fixture = Fixture::new();
            fixture.parameters.push(ParameterChange {
                id: 7,
                value: 0.5,
                sample_offset: 0,
            });
            let mut notes = vec![on(0, 1)];
            match case {
                0 => notes = vec![on(0, 1); 4097],
                1 => fixture.chunks = Vec::new(),
                2 => fixture.input.add_event(on(2, 77)),
                _ => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let _guard = fixture.input.events.lock().unwrap();
                        panic!("owned preflight poison");
                    }));
                }
            }
            let mut output = buffers(9);
            output.outputs[0].channels[0].fill(93.0);
            let error = fixture.run(&mut output, &notes).unwrap_err();
            let expected = match case {
                0 | 1 => NativeEventError::Capacity,
                2 => NativeEventError::PendingEvents,
                _ => NativeEventError::StoragePoisoned,
            };
            assert!(matches!(error,Error::NativeEvent(actual) if actual==expected));
            assert_eq!(output.outputs[0].channels[0], vec![93.0; 9]);
            assert_eq!(fixture.parameters.len(), 1);
            assert_eq!(fixture.parameters[0].id, 7);
            assert_eq!(fixture.parameters[0].value.to_bits(), 0.5f64.to_bits());
            assert_eq!(fixture.parameters[0].sample_offset, 0);
            assert!(fixture.chunks.is_empty());
            let queue = fixture
                .input
                .events
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if case == 2 {
                assert_eq!(*queue, [on(2, 77)]);
            } else {
                assert!(queue.is_empty());
            }
            assert_eq!(fixture.observed.lock().unwrap().calls, 0);
        }
    }

    #[test]
    fn checked_note_requires_same_owner_prepared_readiness_and_does_not_invent_it() {
        let mut fixture = Fixture::new();
        for readiness in [
            None,
            Some(Err(NativeEventError::TopologyChanged {
                direction: crate::audio::BusDirection::Input,
            })),
        ] {
            fixture.activation.event_readiness = readiness;
            assert!(matches!(
                fixture.run(&mut buffers(4), &[on(0, 1)]),
                Err(Error::NativeEvent(_))
            ));
        }
        assert_eq!(fixture.observed.lock().unwrap().calls, 0);
        fixture.activation.event_readiness = Some(Ok(()));
        fixture.run(&mut buffers(4), &[on(0, 1)]).unwrap();
        assert_eq!(fixture.observed.lock().unwrap().calls, 1);
    }

    #[test]
    fn checked_note_scalar_subset_and_direction_local_channel_bounds_are_explicit() {
        let fixture = Fixture::new();
        for case in 0..13 {
            let mut event = on(0, 71);
            match case {
                0 => event.bus_index = -1,
                1 => event.bus_index = 2,
                2 => event.sample_offset = -1,
                3 => event.sample_offset = 9,
                4 => event.ppq_position = f64::NAN,
                5 => event.flags = 2,
                6 => {
                    if let PluginEventData::NoteOn { note_id, .. } = &mut event.data {
                        *note_id = -1;
                    }
                }
                7 => {
                    if let PluginEventData::NoteOn { velocity, .. } = &mut event.data {
                        *velocity = f32::NAN;
                    }
                }
                8 => {
                    if let PluginEventData::NoteOn { tuning, .. } = &mut event.data {
                        *tuning = f32::INFINITY;
                    }
                }
                9 => {
                    if let PluginEventData::NoteOn { length, .. } = &mut event.data {
                        *length = -1;
                    }
                }
                10 => {
                    if let PluginEventData::NoteOn { pitch, .. } = &mut event.data {
                        *pitch = 128;
                    }
                }
                11 => {
                    if let PluginEventData::NoteOn { channel, .. } = &mut event.data {
                        *channel = 16;
                    }
                }
                _ => event.bus_index = 1, // channel2 does not fit this actual one-channel bus.
            }
            assert!(
                matches!(
                    validate_checked_note_events(&[event], 9, &fixture.activation.event_inputs),
                    Err(Error::NativeEvent(NativeEventError::InvalidEvent {
                        index: 0
                    }))
                ),
                "case {case}"
            );
        }
        assert!(matches!(
            validate_checked_note_events(
                &[on(3, 1), off(2, 1)],
                9,
                &fixture.activation.event_inputs
            ),
            Err(Error::NativeEvent(NativeEventError::EventOrder {
                index: 1
            }))
        ));
        validate_checked_note_events(&[on(0, 1), off(0, 1)], 9, &fixture.activation.event_inputs)
            .unwrap();
    }

    #[derive(Default)]
    struct ComponentObservations {
        input_counts: usize,
        output_counts: usize,
        metadata_queries: usize,
        bus_activations: Vec<(i32, i32, bool)>,
        component_activations: Vec<bool>,
    }
    struct ComponentProbe {
        observed: Mutex<ComponentObservations>,
        deactivate_result: std::sync::atomic::AtomicI32,
        reactivate_result: std::sync::atomic::AtomicI32,
        next_input_count: std::sync::atomic::AtomicI32,
        channels: std::sync::atomic::AtomicI32,
    }
    struct Component(Arc<ComponentProbe>);
    impl Class for Component {
        type Interfaces = (IComponent,);
    }
    impl IPluginBaseTrait for Component {
        unsafe fn initialize(&self, _: *mut FUnknown) -> tresult {
            kResultOk
        }
        unsafe fn terminate(&self) -> tresult {
            kResultOk
        }
    }
    impl IComponentTrait for Component {
        unsafe fn getControllerClassId(&self, _: *mut TUID) -> tresult {
            kNotImplemented
        }
        unsafe fn setIoMode(&self, _: i32) -> tresult {
            kResultOk
        }
        unsafe fn getBusCount(&self, media: i32, direction: i32) -> i32 {
            assert_eq!(media, kEvent as i32);
            let mut observed = self.0.observed.lock().unwrap();
            if direction != kInput as i32 {
                observed.output_counts += 1;
                return 0;
            }
            observed.input_counts += 1;
            if observed.input_counts == 1 {
                2
            } else {
                self.0.next_input_count.load(Ordering::Relaxed)
            }
        }
        unsafe fn getBusInfo(
            &self,
            media: i32,
            direction: i32,
            index: i32,
            info: *mut BusInfo,
        ) -> tresult {
            self.0.observed.lock().unwrap().metadata_queries += 1;
            if media != kEvent as i32 || direction != kInput as i32 || !(0..32).contains(&index) {
                return kResultFalse;
            }
            let info = unsafe { &mut *info };
            *info = unsafe { std::mem::zeroed() };
            info.name[..3].copy_from_slice(&[u16::from(b'R'), u16::from(b'o'), u16::from(b'w')]);
            info.mediaType = media;
            info.direction = direction;
            info.channelCount = self.0.channels.load(Ordering::Relaxed);
            info.busType = if index == 0 {
                BusTypes_::kMain as i32
            } else {
                BusTypes_::kAux as i32
            };
            kResultOk
        }
        unsafe fn getRoutingInfo(&self, _: *mut RoutingInfo, _: *mut RoutingInfo) -> tresult {
            kNotImplemented
        }
        unsafe fn activateBus(&self, media: i32, direction: i32, index: i32, state: u8) -> tresult {
            assert_eq!(media, kEvent as i32);
            self.0
                .observed
                .lock()
                .unwrap()
                .bus_activations
                .push((direction, index, state != 0));
            kResultOk
        }
        unsafe fn setActive(&self, state: u8) -> tresult {
            self.0
                .observed
                .lock()
                .unwrap()
                .component_activations
                .push(state != 0);
            if state == 0 {
                self.0.deactivate_result.load(Ordering::Relaxed)
            } else {
                self.0.reactivate_result.load(Ordering::Relaxed)
            }
        }
        unsafe fn setState(&self, _: *mut IBStream) -> tresult {
            kResultOk
        }
        unsafe fn getState(&self, _: *mut IBStream) -> tresult {
            kResultOk
        }
    }
    struct EventControl {
        component: ComPtr<IComponent>,
        probe: Arc<ComponentProbe>,
        activation: BusActivationState,
        active: bool,
        host: ComWrapper<HostApplication>,
    }
    impl EventControl {
        fn new() -> Self {
            let probe = Arc::new(ComponentProbe {
                observed: Mutex::new(ComponentObservations::default()),
                deactivate_result: std::sync::atomic::AtomicI32::new(kResultOk),
                reactivate_result: std::sync::atomic::AtomicI32::new(kResultOk),
                next_input_count: std::sync::atomic::AtomicI32::new(2),
                channels: std::sync::atomic::AtomicI32::new(16),
            });
            let component = ComWrapper::new(Component(Arc::clone(&probe)))
                .to_com_ptr::<IComponent>()
                .unwrap();
            Self {
                component,
                probe,
                active: true,
                activation: BusActivationState {
                    event_inputs: vec![
                        EventBusActivation {
                            role: NativeEventBusRole::Main,
                            channel_count: 16,
                            active: true,
                        },
                        EventBusActivation {
                            role: NativeEventBusRole::Auxiliary,
                            channel_count: 16,
                            active: false,
                        },
                    ],
                    event_readiness: Some(Ok(())),
                    ..BusActivationState::default()
                },
                host: super::super::com_implementations::create_host_application(),
            }
        }
        fn activate(&mut self, index: i32) -> Result<()> {
            activate_checked_event_bus(
                &self.component,
                &self.host,
                &mut self.activation,
                &mut self.active,
                crate::audio::BusDirection::Input,
                index,
                true,
            )
        }
    }

    #[test]
    fn primitive_event_census_matches_the_name_collector_and_preserves_zero_channels() {
        let mut fixture = EventControl::new();
        fixture.probe.channels.store(0, Ordering::Relaxed);
        for row in &mut fixture.activation.event_inputs {
            row.channel_count = 0;
        }
        let named = census_native_event_buses(&fixture.component, &fixture.activation).unwrap();
        assert!(named.outputs.is_empty());
        assert_eq!(named.inputs.len(), 2);
        assert!(named.inputs.iter().all(|row| row.name == "Row"));
        let mut rows = [None; 2];
        walk_native_event_bus_constraints(
            &fixture.component,
            &fixture.activation,
            &mut |direction, index, role, channels, default, active| {
                assert_eq!(direction, crate::audio::BusDirection::Input);
                rows[index] = Some((role, channels, default, active));
                Ok(())
            },
        )
        .unwrap();
        for (index, row) in named.inputs.iter().enumerate() {
            assert_eq!(
                rows[index],
                Some((
                    row.role,
                    row.channel_count,
                    row.default_active,
                    row.host_active
                ))
            );
        }
        let observed = fixture.probe.observed.lock().unwrap();
        assert_eq!(
            (
                observed.input_counts,
                observed.output_counts,
                observed.metadata_queries
            ),
            (2, 2, 4)
        );
        assert!(observed.bus_activations.is_empty() && observed.component_activations.is_empty());
    }

    #[test]
    fn primitive_event_census_callback_failure_stops_before_later_rows_or_directions() {
        let fixture = EventControl::new();
        let mut visits = 0;
        let result = walk_native_event_bus_constraints(
            &fixture.component,
            &fixture.activation,
            &mut |_, index, _, _, _, _| {
                visits += 1;
                assert_eq!(index, 0);
                Err(Error::NotProcessing)
            },
        );
        assert!(matches!(result, Err(Error::NotProcessing)));
        assert_eq!(visits, 1);
        let observed = fixture.probe.observed.lock().unwrap();
        assert_eq!(
            (
                observed.input_counts,
                observed.output_counts,
                observed.metadata_queries
            ),
            (1, 0, 1)
        );
    }

    #[test]
    fn primitive_event_census_keeps_count_bounds_before_begin_and_late_topology_refusal() {
        for count in [-1, 33] {
            let fixture = EventControl::new();
            fixture.probe.observed.lock().unwrap().input_counts = 1;
            fixture
                .probe
                .next_input_count
                .store(count, Ordering::Relaxed);
            let mut began = false;
            let result = walk_native_event_bus_direction(
                &fixture.component,
                crate::audio::BusDirection::Input,
                &fixture.activation.event_inputs,
                |_| {
                    began = true;
                    Ok(())
                },
                |_, _, _, _, _, _| panic!("invalid count cannot reach a row"),
            );
            assert!(
                matches!(result, Err(Error::NativeEvent(NativeEventError::BusCount { count: actual, .. })) if actual == count)
            );
            assert!(!began);
            assert_eq!(fixture.probe.observed.lock().unwrap().metadata_queries, 0);
        }
        for count in [0, 32] {
            let mut fixture = EventControl::new();
            fixture.probe.observed.lock().unwrap().input_counts = 1;
            fixture
                .probe
                .next_input_count
                .store(count, Ordering::Relaxed);
            fixture
                .activation
                .event_inputs
                .resize_with(count as usize, || EventBusActivation {
                    role: NativeEventBusRole::Auxiliary,
                    channel_count: 16,
                    active: false,
                });
            let mut visits = 0;
            walk_native_event_bus_constraints(
                &fixture.component,
                &fixture.activation,
                &mut |direction, index, _, _, _, _| {
                    assert_eq!(direction, crate::audio::BusDirection::Input);
                    assert_eq!(index, visits);
                    visits += 1;
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(visits, count as usize);
        }
        let mut fixture = EventControl::new();
        fixture.activation.event_inputs[1].channel_count = 17;
        let mut visits = 0;
        let result = walk_native_event_bus_constraints(
            &fixture.component,
            &fixture.activation,
            &mut |_, _, _, _, _, _| {
                visits += 1;
                Ok(())
            },
        );
        assert!(matches!(
            result,
            Err(Error::NativeEvent(NativeEventError::TopologyChanged {
                direction: crate::audio::BusDirection::Input,
            }))
        ));
        assert_eq!(
            visits, 1,
            "late failure leaves only caller-local partial results"
        );
        assert_eq!(fixture.probe.observed.lock().unwrap().output_counts, 0);
    }

    #[test]
    fn checked_event_activation_never_widens_a_validated_count_with_a_second_query() {
        let mut fixture = EventControl::new();
        fixture.probe.next_input_count.store(3, Ordering::Relaxed);
        assert!(matches!(
            fixture.activate(2),
            Err(Error::NativeEvent(NativeEventError::BusIndex {
                index: 2,
                count: 2,
                ..
            }))
        ));
        let observed = fixture.probe.observed.lock().unwrap();
        assert_eq!(
            observed.input_counts, 1,
            "second count would report3 and admit an unprepared index"
        );
        assert!(observed.bus_activations.is_empty());
        assert!(observed.component_activations.is_empty());
        assert!(fixture.active);
        assert!(!fixture.activation.event_inputs[1].active);
        assert!(fixture.activation.event_readiness.is_none());
    }

    #[test]
    fn checked_event_deactivate_refusal_keeps_active_state_and_never_calls_activate_bus() {
        let mut fixture = EventControl::new();
        fixture
            .probe
            .deactivate_result
            .store(kResultFalse, Ordering::Relaxed);
        assert!(matches!(fixture.activate(1),Err(Error::NativeEvent(
            NativeEventError::ComponentActivation {active:false,result}
        )) if result==kResultFalse));
        let observed = fixture.probe.observed.lock().unwrap();
        assert_eq!(observed.component_activations, [false]);
        assert!(observed.bus_activations.is_empty());
        assert!(
            fixture.active,
            "failed deactivate is not an inactive component"
        );
        assert!(!fixture.activation.event_inputs[1].active);
        assert!(fixture.activation.event_readiness.is_none());
    }

    #[test]
    fn checked_event_reactivate_refusal_retains_only_the_real_bus_ack_and_never_grants_ready() {
        let mut fixture = EventControl::new();
        fixture
            .probe
            .reactivate_result
            .store(kResultFalse, Ordering::Relaxed);
        assert!(matches!(fixture.activate(1),Err(Error::NativeEvent(
            NativeEventError::ComponentActivation {active:true,result}
        )) if result==kResultFalse));
        let observed = fixture.probe.observed.lock().unwrap();
        assert_eq!(observed.component_activations, [false, true]);
        assert_eq!(observed.bus_activations, [(kInput as i32, 1, true)]);
        assert!(!fixture.active);
        assert!(
            fixture.activation.event_inputs[1].active,
            "activateBus actually acknowledged"
        );
        assert!(fixture.activation.event_readiness.is_none());
    }

    #[test]
    fn checked_event_zero_channels_remain_declared_without_fabricating_a_note_channel() {
        let mut fixture = EventControl::new();
        fixture.probe.channels.store(0, Ordering::Relaxed);
        for bus in &mut fixture.activation.event_inputs {
            bus.channel_count = 0;
        }
        let info = census_native_event_buses(&fixture.component, &fixture.activation).unwrap();
        assert_eq!(info.inputs.len(), 2);
        assert_eq!(info.inputs[0].channel_count, 0);
        assert!(matches!(
            validate_checked_note_events(&[on(0, 1)], 4, &fixture.activation.event_inputs),
            Err(Error::NativeEvent(NativeEventError::InvalidEvent {
                index: 0
            }))
        ));
        validate_checked_note_events(&[], 4, &fixture.activation.event_inputs).unwrap();
    }

    #[test]
    fn checked_note_count_refuses_before_even_the_first_invalid_event_is_inspected() {
        let fixture = Fixture::new();
        let oversized = vec![on(99, 1); 4097];
        assert!(matches!(
            validate_checked_note_events(&oversized, 4, &fixture.activation.event_inputs),
            Err(Error::NativeEvent(NativeEventError::Capacity))
        ));
    }

    #[test]
    fn checked_event_census_count_and_native_direction_fields_are_bounded() {
        use crate::audio::BusDirection::{Input, Output};
        assert_eq!(bounded_event_count(Input, 32).unwrap(), 32);
        for count in [-1, 33, i32::MAX] {
            assert!(matches!(
                bounded_event_count(Input, count),
                Err(Error::NativeEvent(NativeEventError::BusCount { .. }))
            ));
        }
        let mut info: BusInfo = unsafe { std::mem::zeroed() };
        info.mediaType = kEvent as i32;
        info.direction = kInput as i32;
        info.channelCount = 16;
        info.busType = BusTypes_::kMain as i32;
        assert_eq!(
            checked_event_bus_info(Input, 0, kResultOk, &info).unwrap(),
            (NativeEventBusRole::Main, 16)
        );
        assert!(matches!(
            checked_event_bus_info(Output, 0, kResultOk, &info),
            Err(Error::NativeEvent(NativeEventError::BusMetadata { .. }))
        ));
        info.mediaType = kAudio as i32;
        assert!(checked_event_bus_info(Input, 0, kResultOk, &info).is_err());
    }

    #[test]
    fn anchored_musical_native_chunks_keep_absolute_reference_and_exact_rounding() {
        use crate::audio::{ProcessMusicalReference, ProcessTimelineContext};
        use ProcessContext_::StatesAndFlags_ as S;
        let valid = (S::kProjectTimeMusicValid | S::kTempoValid) as u32;
        for path in [WindowPath::Flat, WindowPath::Buses, WindowPath::ScopedNotes] {
            let mut fixture = Fixture::new();
            for start in [-17, i64::MAX - 32] {
                let reference = ProcessMusicalReference::new(start - 1, 48_000, 3, 4).unwrap();
                fixture
                    .fields()
                    .set_process_musical_timeline_context(
                        ProcessTimelineContext {
                            sample_position: start,
                            sample_rate: 48_000,
                        },
                        reference,
                    )
                    .unwrap();
                let before = fixture.observed.lock().unwrap().calls;
                // This slice observes process context only. An empty checked
                // batch supplies no invented Event.ppqPosition or Note claim.
                let result = if matches!(path, WindowPath::ScopedNotes) {
                    fixture.run(&mut buffers(9), &[])
                } else {
                    run_sample_window(&mut fixture, path, 9).0
                };
                result.unwrap();
                let observed = fixture.observed.lock().unwrap();
                assert_eq!(&observed.frames[before..], [4, 4, 1]);
                assert_eq!(
                    &observed.positions[before..],
                    [
                        (start, 0, valid),
                        (start + 4, 0, valid),
                        (start + 8, 0, valid)
                    ]
                );
                for (row, quarter_bits) in observed.musical[before..].iter().zip([
                    0x3efd_208a_5a91_2e32, // 1/36000
                    0x3f22_3456_789a_bcdf, // 5/36000
                    0x3f30_624d_d2f1_a9fc, // 9/36000
                ]) {
                    assert_eq!(*row, [quarter_bits, 0x4054_0000_0000_0000, 0, 0, 0]);
                }
                drop(observed);
                let data = fixture.data.as_ref().unwrap();
                assert_eq!(data.musical_reference, Some(reference));
                assert_eq!(data.process_context.projectTimeSamples, start + 9);
                assert_eq!(
                    data.process_context.projectTimeMusic.to_bits(),
                    0x3f32_3456_789a_bcdf
                ); // 10/36000
            }
        }
    }

    #[test]
    fn anchored_musical_install_is_atomic_and_successful_position_only_install_clears_it() {
        use crate::audio::{ProcessMusicalReference, ProcessTimelineContext};
        let mut fixture = Fixture::new();
        let reference = ProcessMusicalReference::new(-1, 7, 3, 4).unwrap();
        fixture
            .fields()
            .set_process_musical_timeline_context(
                ProcessTimelineContext {
                    sample_position: -17,
                    sample_rate: 48_000,
                },
                reference,
            )
            .unwrap();
        let snapshot = context_facts(&fixture.data.as_ref().unwrap().process_context);
        let pointer = fixture.data.as_ref().unwrap().process_data.processContext;
        for rate in [0, 44_100] {
            assert!(matches!(
                fixture.fields().set_process_musical_timeline_context(
                    ProcessTimelineContext {
                        sample_position: 91,
                        sample_rate: rate
                    },
                    reference
                ),
                Err(Error::MusicalTime(MusicalTimeError::SampleRate))
            ));
            assert!(matches!(
                fixture
                    .fields()
                    .set_process_timeline_context(ProcessTimelineContext {
                        sample_position: 91,
                        sample_rate: rate
                    }),
                Err(Error::InvalidParameter(_))
            ));
            let data = fixture.data.as_ref().unwrap();
            assert_eq!(context_facts(&data.process_context), snapshot);
            assert_eq!(data.musical_reference, Some(reference));
            assert_eq!(data.process_data.processContext, pointer);
        }
        fixture
            .fields()
            .set_process_timeline_context(ProcessTimelineContext {
                sample_position: -5,
                sample_rate: 48_000,
            })
            .unwrap();
        let data = fixture.data.as_ref().unwrap();
        assert_eq!(data.musical_reference, None);
        assert_eq!(data.process_context.projectTimeSamples, -5);
        assert_eq!(data.process_context.state, 0);
        assert_eq!(data.process_context.projectTimeMusic, 0.0);
        assert_eq!(data.process_context.tempo, 0.0);
        assert_eq!(data.process_data.processContext, pointer);
        assert_eq!(fixture.observed.lock().unwrap().calls, 0);
    }

    #[test]
    fn anchored_musical_window_refusals_preserve_real_com_input_pcm_and_parameter_owners() {
        use crate::audio::{ProcessMusicalReference, ProcessTimelineContext};
        for bad_rate in [false, true] {
            let mut fixture = Fixture::new();
            let start = if bad_rate { -17 } else { i64::MAX - 4 };
            let reference = ProcessMusicalReference::new(start, 48_000, 3, 4).unwrap();
            fixture
                .fields()
                .set_process_musical_timeline_context(
                    ProcessTimelineContext {
                        sample_position: start,
                        sample_rate: 48_000,
                    },
                    reference,
                )
                .unwrap();
            if bad_rate {
                fixture.data.as_mut().unwrap().process_context.sampleRate = 44_100.0;
            }
            fixture.parameters.push(ParameterChange {
                id: 7,
                value: 0.5,
                sample_offset: 8,
            });
            fixture.input.events.lock().unwrap().push(on(0, 71));
            fixture.chunks.push(Some(off(0, 71)));
            fixture.gui.lock().unwrap().push((17, 0.25));
            let input = fixture.input.events.lock().unwrap().clone();
            let chunks = fixture.chunks.clone();
            let parameter_owner = (fixture.parameters.as_ptr(), fixture.parameters.capacity());
            let snapshot = context_facts(&fixture.data.as_ref().unwrap().process_context);
            let (result, pcm) = run_sample_window(&mut fixture, WindowPath::Buses, 9);
            if bad_rate {
                assert!(matches!(
                    result,
                    Err(Error::MusicalTime(MusicalTimeError::SampleRate))
                ));
            } else {
                assert!(matches!(
                    result,
                    Err(Error::NativeEvent(NativeEventError::InvalidFrames))
                ));
            }
            assert_eq!(pcm, [93.0; 9]);
            assert_eq!(fixture.observed.lock().unwrap().calls, 0);
            assert_eq!(*fixture.input.events.lock().unwrap(), input);
            assert_eq!(fixture.chunks, chunks);
            assert_eq!(
                (fixture.parameters.as_ptr(), fixture.parameters.capacity()),
                parameter_owner
            );
            assert_eq!(fixture.parameters.len(), 1);
            assert_eq!(
                (
                    fixture.parameters[0].id,
                    fixture.parameters[0].value,
                    fixture.parameters[0].sample_offset
                ),
                (7, 0.5, 8)
            );
            assert_eq!(*fixture.gui.lock().unwrap(), [(17, 0.25)]);
            let data = fixture.data.as_ref().unwrap();
            assert_eq!(context_facts(&data.process_context), snapshot);
            assert_eq!(data.musical_reference, Some(reference));
        }
    }

    #[test]
    fn anchored_musical_post_native_fault_keeps_primary_failure_and_does_not_replay() {
        use crate::audio::{ProcessMusicalReference, ProcessTimelineContext};
        for native_failure in [false, true] {
            let mut fixture = Fixture::new();
            let reference = ProcessMusicalReference::new(-1, 7, 3, 4).unwrap();
            fixture
                .fields()
                .set_process_musical_timeline_context(
                    ProcessTimelineContext {
                        sample_position: -17,
                        sample_rate: 48_000,
                    },
                    reference,
                )
                .unwrap();
            fixture.context_fault.store(3, Ordering::Relaxed);
            if native_failure {
                fixture.fail_call.store(1, Ordering::Relaxed);
            }
            fixture.parameters.push(ParameterChange {
                id: 7,
                value: 0.5,
                sample_offset: 8,
            });
            let mut pcm = buffers(9);
            pcm.outputs[0].channels[0].fill(93.0);
            let result = fixture.run(&mut pcm, &[]);
            if native_failure {
                assert!(matches!(result, Err(Error::ProcessFailed(code)) if code==kResultFalse));
            } else {
                assert!(matches!(
                    result,
                    Err(Error::MusicalTime(MusicalTimeError::SampleRate))
                ));
            }
            assert_eq!(fixture.observed.lock().unwrap().frames, [4]);
            assert_eq!(pcm.outputs[0].channels[0], [93.0; 9]);
            assert!(fixture.input.events.lock().unwrap().is_empty());
            assert!(fixture.chunks.is_empty());
            assert!(fixture.parameters.is_empty());
            fixture.context_fault.store(0, Ordering::Relaxed);
            fixture.fail_call.store(0, Ordering::Relaxed);
            fixture
                .fields()
                .set_process_musical_timeline_context(
                    ProcessTimelineContext {
                        sample_position: 100,
                        sample_rate: 48_000,
                    },
                    reference,
                )
                .unwrap();
            fixture.run(&mut buffers(4), &[]).unwrap();
            let observed = fixture.observed.lock().unwrap();
            assert_eq!(observed.frames, [4, 4]);
            assert!(observed.received[1].is_empty());
        }
    }
    #[test]
    fn anchored_musical_queued_ingress_refuses_before_native_without_consuming_owners() {
        use crate::audio::{ProcessMusicalReference, ProcessTimelineContext};
        for case in 0..4 {
            for checked in [false, true] {
                let mut fixture = Fixture::new();
                if case == 0 {
                    fixture.input.add_event(on(0, 77));
                }
                fixture
                    .fields()
                    .set_process_musical_timeline_context(
                        ProcessTimelineContext {
                            sample_position: -17,
                            sample_rate: 48_000,
                        },
                        ProcessMusicalReference::new(-18, 48_000, 3, 4).unwrap(),
                    )
                    .unwrap();
                match case {
                    1 => fixture.input.add_event(on(0, 88)),
                    2 => fixture.chunks.push(Some(off(0, 99))),
                    3 => {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let _guard = fixture.input.events.lock().unwrap();
                            panic!("owned poisoned ingress control");
                        }));
                    }
                    _ => {}
                }
                fixture.parameters.push(ParameterChange {
                    id: 7,
                    value: 0.75,
                    sample_offset: 0,
                });
                let original = fixture
                    .input
                    .events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                let chunks = fixture.chunks.clone();
                let parameter_owner = (fixture.parameters.as_ptr(), fixture.parameters.capacity());
                let context = context_facts(&fixture.data.as_ref().unwrap().process_context);
                let mut pcm = buffers(4);
                pcm.outputs[0].channels[0].fill(93.0);
                let result = if checked {
                    fixture.run(&mut pcm, &[])
                } else {
                    fixture.fields().process_bus_audio(&mut pcm)
                };
                let expected = if case == 3 {
                    NativeEventError::StoragePoisoned
                } else {
                    NativeEventError::PendingEvents
                };
                assert!(
                    matches!(result, Err(Error::NativeEvent(error)) if error == expected),
                    "Some must refuse case={case} checked={checked} before native"
                );
                assert_eq!(fixture.observed.lock().unwrap().calls, 0);
                assert_eq!(pcm.outputs[0].channels[0], [93.0; 4]);
                assert_eq!(
                    *fixture
                        .input
                        .events
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()),
                    original
                );
                assert_eq!(fixture.chunks, chunks);
                assert_eq!(
                    (fixture.parameters.as_ptr(), fixture.parameters.capacity()),
                    parameter_owner
                );
                assert_eq!(fixture.parameters.len(), 1);
                assert_eq!(
                    (
                        fixture.parameters[0].id,
                        fixture.parameters[0].value,
                        fixture.parameters[0].sample_offset
                    ),
                    (7, 0.75, 0)
                );
                assert_eq!(
                    context_facts(&fixture.data.as_ref().unwrap().process_context),
                    context
                );
            }
        }
    }

    #[test]
    fn anchored_musical_late_legacy_ingress_is_not_delivered_or_discarded() {
        use crate::audio::{ProcessMusicalReference, ProcessTimelineContext};
        for checked in [false, true] {
            let mut fixture = Fixture::new();
            fixture
                .fields()
                .set_process_musical_timeline_context(
                    ProcessTimelineContext {
                        sample_position: -17,
                        sample_rate: 48_000,
                    },
                    ProcessMusicalReference::new(-18, 48_000, 3, 4).unwrap(),
                )
                .unwrap();
            fixture.inject_legacy.store(true, Ordering::Relaxed);
            let mut pcm = buffers(9);
            if checked {
                fixture.run(&mut pcm, &[]).unwrap();
            } else {
                fixture.fields().process_bus_audio(&mut pcm).unwrap();
            }
            {
                let observed = fixture.observed.lock().unwrap();
                assert_eq!(observed.frames, [4, 4, 1]);
                assert!(observed.received.iter().all(Vec::is_empty));
            }
            let pending = fixture.input.events.lock().unwrap().clone();
            assert_eq!(pending.len(), 1, "late original ingress must remain owned");
            assert!(matches!(
                pending[0].data,
                crate::midi::PluginEventData::NoteOn { note_id: 999, .. }
            ));
            assert_eq!(pending[0].ppq_position.to_bits(), 0);
            fixture.parameters.push(ParameterChange {
                id: 7,
                value: 0.75,
                sample_offset: 0,
            });
            let mut next = buffers(4);
            next.outputs[0].channels[0].fill(93.0);
            let result = if checked {
                fixture.run(&mut next, &[])
            } else {
                fixture.fields().process_bus_audio(&mut next)
            };
            assert!(
                matches!(
                    result,
                    Err(Error::NativeEvent(NativeEventError::PendingEvents))
                ),
                "late ingress must explicitly block the next Some call"
            );
            assert_eq!(fixture.observed.lock().unwrap().calls, 3);
            assert_eq!(next.outputs[0].channels[0], [93.0; 4]);
            assert_eq!(*fixture.input.events.lock().unwrap(), pending);
            assert_eq!(fixture.parameters.len(), 1);
            fixture
                .fields()
                .set_process_timeline_context(ProcessTimelineContext {
                    sample_position: 100,
                    sample_rate: 48_000,
                })
                .unwrap();
            fixture.fields().process_bus_audio(&mut next).unwrap();
            let observed = fixture.observed.lock().unwrap();
            assert_eq!(observed.calls, 4);
            assert_eq!(observed.received[3].len(), 1);
            assert_eq!(
                observed.received[3][0].2, 999,
                "explicit None recovers the original queued event once"
            );
            assert!(fixture.input.events.lock().unwrap().is_empty());
        }
    }
    #[test]
    fn anchored_musical_explicit_checked_events_refuse_before_consumption() {
        use crate::audio::{ProcessMusicalReference, ProcessTimelineContext};
        let mut fixture = Fixture::new();
        fixture
            .fields()
            .set_process_musical_timeline_context(
                ProcessTimelineContext {
                    sample_position: -17,
                    sample_rate: 48_000,
                },
                ProcessMusicalReference::new(-18, 48_000, 3, 4).unwrap(),
            )
            .unwrap();
        fixture.parameters.push(ParameterChange {
            id: 7,
            value: 0.75,
            sample_offset: 0,
        });
        let mut caller = on(0, 71);
        caller.ppq_position = -3.5;
        let mut pcm = buffers(4);
        pcm.outputs[0].channels[0].fill(93.0);
        let context = context_facts(&fixture.data.as_ref().unwrap().process_context);
        assert!(
            matches!(
                fixture.run(&mut pcm, &[caller.clone()]),
                Err(Error::MusicalTime(MusicalTimeError::EventInputUnsupported))
            ),
            "this first anchored capability is pure audio"
        );
        assert_eq!(fixture.observed.lock().unwrap().calls, 0);
        assert_eq!(pcm.outputs[0].channels[0], [93.0; 4]);
        assert_eq!(fixture.parameters.len(), 1);
        assert_eq!(fixture.parameters[0].value, 0.75);
        assert!(fixture.input.events.lock().unwrap().is_empty());
        assert!(fixture.chunks.is_empty());
        assert_eq!(
            context_facts(&fixture.data.as_ref().unwrap().process_context),
            context
        );
        // Explicit caller-supplied PPQ responsibility under None is unchanged.
        // Clearing the optional anchor is an explicit SDK caller operation.
        fixture
            .fields()
            .set_process_timeline_context(ProcessTimelineContext {
                sample_position: 100,
                sample_rate: 48_000,
            })
            .unwrap();
        fixture.run(&mut pcm, &[caller]).unwrap();
        let observed = fixture.observed.lock().unwrap();
        assert_eq!(observed.calls, 1);
        assert_eq!(observed.received[0].len(), 1);
        assert_eq!(observed.received[0][0].2, 71);
        assert!(fixture.parameters.is_empty());
    }
    #[test]
    fn anchored_musical_busy_ingress_refuses_without_waiting_or_consuming() {
        use crate::audio::{ProcessMusicalReference, ProcessTimelineContext};
        for checked in [false, true] {
            let mut fixture = Fixture::new();
            fixture
                .fields()
                .set_process_musical_timeline_context(
                    ProcessTimelineContext {
                        sample_position: -17,
                        sample_rate: 48_000,
                    },
                    ProcessMusicalReference::new(-18, 48_000, 3, 4).unwrap(),
                )
                .unwrap();
            fixture.parameters.push(ParameterChange {
                id: 7,
                value: 0.75,
                sample_offset: 0,
            });
            let mut pcm = buffers(4);
            pcm.outputs[0].channels[0].fill(93.0);
            let pointer = fixture.data.as_ref().unwrap().process_data.inputEvents;
            {
                let mut fields = fixture.fields();
                let original_ingress = fields.input_events;
                // The same-thread held owner would deadlock a blocking check.
                // The admitted guard must return StorageBusy without acquiring it.
                let _held = original_ingress.events.lock().unwrap();
                let result = if checked {
                    fields.process_bus_audio_with_note_events(&mut pcm, &[])
                } else {
                    fields.process_bus_audio(&mut pcm)
                };
                assert!(matches!(
                    result,
                    Err(Error::NativeEvent(NativeEventError::StorageBusy))
                ));
            }
            assert_eq!(fixture.observed.lock().unwrap().calls, 0);
            assert_eq!(pcm.outputs[0].channels[0], [93.0; 4]);
            assert_eq!(fixture.parameters.len(), 1);
            assert_eq!(fixture.parameters[0].value, 0.75);
            assert_eq!(
                fixture.data.as_ref().unwrap().process_data.inputEvents,
                pointer
            );
            assert!(fixture.input.events.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn anchored_musical_empty_retained_slots_and_capacity_are_not_events() {
        use crate::audio::{ProcessMusicalReference, ProcessTimelineContext};
        for checked in [false, true] {
            let mut fixture = Fixture::new();
            fixture
                .fields()
                .set_process_musical_timeline_context(
                    ProcessTimelineContext {
                        sample_position: -17,
                        sample_rate: 48_000,
                    },
                    ProcessMusicalReference::new(-18, 48_000, 3, 4).unwrap(),
                )
                .unwrap();
            fixture.chunks.resize_with(3, || None);
            let chunks = (fixture.chunks.as_ptr(), fixture.chunks.capacity());
            let original = {
                let mut queue = fixture.input.events.lock().unwrap();
                queue.reserve(8192);
                (queue.as_ptr(), queue.capacity())
            };
            let pointer = fixture.data.as_ref().unwrap().process_data.inputEvents;
            let mut pcm = buffers(9);
            if checked {
                fixture.run(&mut pcm, &[]).unwrap();
            } else {
                fixture.fields().process_bus_audio(&mut pcm).unwrap();
            }
            assert_eq!(fixture.observed.lock().unwrap().frames, [4, 4, 1]);
            assert!(fixture
                .observed
                .lock()
                .unwrap()
                .received
                .iter()
                .all(Vec::is_empty));
            assert_eq!(fixture.chunks.len(), 3);
            assert!(fixture.chunks.iter().all(Option::is_none));
            assert_eq!((fixture.chunks.as_ptr(), fixture.chunks.capacity()), chunks);
            let queue = fixture.input.events.lock().unwrap();
            assert!(queue.is_empty());
            assert_eq!((queue.as_ptr(), queue.capacity()), original);
            assert_eq!(
                fixture.data.as_ref().unwrap().process_data.inputEvents,
                pointer
            );
        }
    }

    #[test]
    fn anchored_musical_late_ingress_and_pointer_survive_native_or_advance_failure() {
        use crate::audio::{ProcessMusicalReference, ProcessTimelineContext};
        for checked in [false, true] {
            for native_failure in [false, true] {
                let mut fixture = Fixture::new();
                let context = ProcessTimelineContext {
                    sample_position: -17,
                    sample_rate: 48_000,
                };
                let reference = ProcessMusicalReference::new(-18, 48_000, 3, 4).unwrap();
                fixture
                    .fields()
                    .set_process_musical_timeline_context(context, reference)
                    .unwrap();
                fixture.inject_legacy.store(true, Ordering::Relaxed);
                fixture.context_fault.store(3, Ordering::Relaxed);
                if native_failure {
                    fixture.fail_call.store(1, Ordering::Relaxed);
                }
                fixture.parameters.push(ParameterChange {
                    id: 7,
                    value: 0.75,
                    sample_offset: 8,
                });
                let pointer = fixture.data.as_ref().unwrap().process_data.inputEvents;
                let mut pcm = buffers(9);
                pcm.outputs[0].channels[0].fill(93.0);
                let result = if checked {
                    fixture.run(&mut pcm, &[])
                } else {
                    fixture.fields().process_bus_audio(&mut pcm)
                };
                if native_failure {
                    assert!(
                        matches!(result, Err(Error::ProcessFailed(value)) if value == kResultFalse)
                    );
                } else {
                    assert!(matches!(
                        result,
                        Err(Error::MusicalTime(MusicalTimeError::SampleRate))
                    ));
                }
                assert_eq!(fixture.observed.lock().unwrap().frames, [4]);
                assert!(fixture.observed.lock().unwrap().received[0].is_empty());
                assert_eq!(pcm.outputs[0].channels[0], [93.0; 9]);
                assert_eq!(
                    fixture.data.as_ref().unwrap().process_data.inputEvents,
                    pointer
                );
                let pending = fixture.input.events.lock().unwrap().clone();
                assert_eq!(pending.len(), 1);
                assert!(matches!(
                    pending[0].data,
                    crate::midi::PluginEventData::NoteOn { note_id: 999, .. }
                ));
                assert!(
                    fixture.parameters.is_empty(),
                    "consumed native failure keeps original cleanup"
                );
                fixture.context_fault.store(0, Ordering::Relaxed);
                fixture.fail_call.store(0, Ordering::Relaxed);
                fixture
                    .fields()
                    .set_process_musical_timeline_context(context, reference)
                    .unwrap();
                let mut next = buffers(4);
                next.outputs[0].channels[0].fill(93.0);
                assert!(matches!(
                    fixture.fields().process_bus_audio(&mut next),
                    Err(Error::NativeEvent(NativeEventError::PendingEvents))
                ));
                assert_eq!(fixture.observed.lock().unwrap().calls, 1);
                assert_eq!(next.outputs[0].channels[0], [93.0; 4]);
                assert_eq!(*fixture.input.events.lock().unwrap(), pending);
                fixture
                    .fields()
                    .set_process_timeline_context(context)
                    .unwrap();
                fixture.fields().process_bus_audio(&mut next).unwrap();
                let observed = fixture.observed.lock().unwrap();
                assert_eq!(observed.calls, 2);
                assert_eq!(observed.received[1].len(), 1);
                assert_eq!(observed.received[1][0].2, 999);
                assert_eq!(
                    fixture.data.as_ref().unwrap().process_data.inputEvents,
                    pointer
                );
                assert!(fixture.input.events.lock().unwrap().is_empty());
            }
        }
    }
    #[test]
    fn anchored_musical_reference_occupies_the_existing_complete_process_box() {
        use crate::audio::{ProcessMusicalReference, ProcessTimelineContext};
        use crate::host_storage::{HostStorageKind, HostStorageRef};
        let mut fixture = Fixture::new();
        let (address, layout) = {
            let process = fixture.data.as_ref().unwrap();
            let observed = HostStorageRef::boxed(process);
            assert_eq!(observed.kind(), HostStorageKind::CompleteAllocation);
            assert_eq!(
                observed.layout(),
                std::alloc::Layout::new::<HostProcessData>()
            );
            let field = std::ptr::from_ref(&process.musical_reference) as usize;
            assert!(field >= observed.address());
            assert!(
                field + std::mem::size_of::<Option<crate::audio::ProcessMusicalReference>>()
                    <= observed.address() + observed.layout().size()
            );
            assert_eq!(process.musical_reference, None);
            (observed.address(), observed.layout())
        };
        fixture
            .fields()
            .set_process_musical_timeline_context(
                ProcessTimelineContext {
                    sample_position: -17,
                    sample_rate: 48_000,
                },
                ProcessMusicalReference::new(-18, 48_000, 3, 4).unwrap(),
            )
            .unwrap();
        let process = fixture.data.as_ref().unwrap();
        let observed = HostStorageRef::boxed(process);
        assert_eq!((observed.address(), observed.layout()), (address, layout));
        assert!(process.musical_reference.is_some());
        // This is the original visitor primitive's complete Box observation,
        // not an Audio grant or a complete native/plugin heap qualification.
    }
}
