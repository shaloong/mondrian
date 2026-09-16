//! Plan-time processor resolution and Session-owned callback dispatch.

use crate::processor_parameters::{prepare_parameter_event_batch, ProcessorParameterEventScratch};
use crate::schedule::{
    PreparedAudioSchedule, PreparedProcessor, PreparedProcessorAuxiliaryBus, PreparedRack,
    PreparedSidechainBinding,
};
#[cfg(test)]
use crate::AudioParameterEvent;
use crate::{delay::FixedDelayLine, dsp};
use crate::{
    AudioExecutionError, AudioKernelBackend, AudioProcessor, AudioProcessorAudioIo,
    AudioProcessorAuxiliaryInputContract, AudioProcessorExecutionContract, AudioProcessorFactory,
    AudioProcessorHostError, AudioProcessorInputBus, AudioProcessorMainAndInputBuses,
    AudioProcessorOccurrence, AudioProcessorOccurrenceOwner, AudioProcessorPrepareRequest,
    AudioProcessorProcessContext, AudioProcessorResolver, AudioRenderContract, AudioRenderRequest,
    AudioStateEntry, BuiltInAudioProcessorResolver,
};
#[cfg(test)]
use mondrian_core::ParameterId;
use mondrian_core::{AudioChannelLayout, AudioSampleRate};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct PreparedProcessorFactoryBinding {
    contract: AudioProcessorExecutionContract,
    auxiliary_inputs: AudioProcessorAuxiliaryInputContract,
    factory: Arc<dyn AudioProcessorFactory>,
}

impl PreparedProcessorFactoryBinding {
    pub(crate) fn resolve(
        resolver: &dyn AudioProcessorResolver,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Self, AudioProcessorHostError> {
        let render_contract = request.render_contract();
        let mode = render_contract.processing_mode;
        let factory = isolate_adapter_call("resolution", || resolver.prepare(request))?;
        let contract =
            isolate_adapter_value("execution-contract query", || factory.execution_contract())?;
        let auxiliary_inputs = isolate_adapter_value("auxiliary-input contract query", || {
            factory.auxiliary_input_contract()
        })?;
        auxiliary_inputs.validate(render_contract.channel_layout)?;
        if !contract.admits(mode) {
            return Err(AudioProcessorHostError::InvalidContract(format!(
                "processor does not admit {mode:?} execution"
            )));
        }
        Ok(Self { contract, auxiliary_inputs, factory })
    }

    pub(crate) const fn contract(&self) -> AudioProcessorExecutionContract {
        self.contract
    }

    pub(crate) fn auxiliary_inputs(&self) -> &AudioProcessorAuxiliaryInputContract {
        &self.auxiliary_inputs
    }

    fn create(&self) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        let current_contract = isolate_adapter_value("execution-contract revalidation", || {
            self.factory.execution_contract()
        })?;
        if current_contract != self.contract {
            return Err(AudioProcessorHostError::InvalidContract(
                "prepared factory changed its execution contract before Session creation"
                    .to_owned(),
            ));
        }
        let current_auxiliary_inputs =
            isolate_adapter_value("auxiliary-input contract revalidation", || {
                self.factory.auxiliary_input_contract()
            })?;
        if current_auxiliary_inputs != self.auxiliary_inputs {
            return Err(AudioProcessorHostError::InvalidContract(
                "prepared factory changed its auxiliary-input contract before Session creation"
                    .to_owned(),
            ));
        }
        isolate_adapter_call("instance creation", || self.factory.create())
    }
}

impl fmt::Debug for PreparedProcessorFactoryBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedProcessorFactoryBinding")
            .field("contract", &self.contract)
            .field("auxiliary_inputs", &self.auxiliary_inputs)
            .finish_non_exhaustive()
    }
}

struct MainBusAudioIo<'a> {
    channel_layout: AudioChannelLayout,
    frames: usize,
    pcm: &'a mut [f32],
    auxiliary_buses: &'a [PreparedProcessorAuxiliaryBus],
    auxiliary_pcm: &'a [f32],
}

impl AudioProcessorAudioIo for MainBusAudioIo<'_> {
    fn main_layout(&self) -> AudioChannelLayout {
        self.channel_layout
    }

    fn frames(&self) -> usize {
        self.frames
    }

    fn main_interleaved(&mut self) -> &mut [f32] {
        self.pcm
    }

    fn auxiliary_input(&self, bus_key: &str) -> Option<AudioProcessorInputBus<'_>> {
        let samples = self.frames.checked_mul(self.channel_layout.channel_count())?;
        let bus_index = self.auxiliary_buses.iter().position(|bus| bus.bus_key == bus_key)?;
        let start = bus_index.checked_mul(samples)?;
        let end = start.checked_add(samples)?;
        Some(AudioProcessorInputBus {
            bus_key: &self.auxiliary_buses[bus_index].bus_key,
            channel_layout: self.channel_layout,
            frames: self.frames,
            interleaved: self.auxiliary_pcm.get(start..end)?,
        })
    }

    fn main_and_auxiliary_input(
        &mut self,
        bus_key: &str,
    ) -> Option<AudioProcessorMainAndInputBuses<'_>> {
        let samples = self.frames.checked_mul(self.channel_layout.channel_count())?;
        let bus_index = self.auxiliary_buses.iter().position(|bus| bus.bus_key == bus_key)?;
        let start = bus_index.checked_mul(samples)?;
        let end = start.checked_add(samples)?;
        let auxiliary = AudioProcessorInputBus {
            bus_key: &self.auxiliary_buses[bus_index].bus_key,
            channel_layout: self.channel_layout,
            frames: self.frames,
            interleaved: self.auxiliary_pcm.get(start..end)?,
        };
        Some(AudioProcessorMainAndInputBuses {
            main_layout: self.channel_layout,
            frames: self.frames,
            main_interleaved: self.pcm,
            auxiliary,
        })
    }
}

struct HostedProcessorInstance {
    occurrence: AudioProcessorOccurrence,
    processor: Box<dyn AudioProcessor>,
    continuity: HostedProcessorContinuity,
}

#[derive(Debug, Clone, Copy)]
enum HostedProcessorContinuity {
    Stateless,
    Unentered,
    Failed,
    Pending {
        epoch: crate::AudioContinuityEpoch,
    },
    Active {
        epoch: crate::AudioContinuityEpoch,
        next_sample: i64,
    },
}

pub(crate) struct PreparedProcessorHost {
    instances: Vec<HostedProcessorInstance>,
    parameter_events: ProcessorParameterEventScratch,
    render_contract: AudioRenderContract,
    maximum_parameter_lanes: usize,
    parameter_event_capacity: usize,
    session_scratch_bytes: usize,
    auxiliary_pcm: Vec<f32>,
    sidechain_frame_db: Vec<f64>,
    sidechain_gains: Vec<f32>,
}

impl PreparedProcessorHost {
    pub(crate) fn new(
        schedule: &PreparedAudioSchedule,
        render_contract: AudioRenderContract,
    ) -> Result<Self, AudioExecutionError> {
        let maximum_parameter_lanes = schedule
            .processors
            .iter()
            .map(|processor| processor.parameter_ids.len())
            .max()
            .unwrap_or(0);
        if schedule
            .processors
            .iter()
            .any(|processor| processor.parameter_ids.len() != processor.parameter_curves.len())
        {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        let parameter_event_capacity = schedule.summary.maximum_parameter_events_per_block;
        let session_scratch_bytes = schedule
            .processors
            .iter()
            .try_fold(0_usize, |bytes, processor| {
                bytes.checked_add(processor.factory.contract().session_scratch_bytes())
            })
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        if session_scratch_bytes != schedule.summary.processor_session_scratch_bytes
            || session_scratch_bytes > render_contract.processor_session_scratch_budget_bytes
        {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        let instances = schedule
            .processors
            .iter()
            .map(|processor| {
                let continuity = if processor.factory.contract().requires_state_entry() {
                    HostedProcessorContinuity::Unentered
                } else {
                    HostedProcessorContinuity::Stateless
                };
                Ok(HostedProcessorInstance {
                    occurrence: processor.occurrence,
                    processor: processor.factory.create()?,
                    continuity,
                })
            })
            .collect::<Result<Vec<_>, AudioExecutionError>>()?;
        let maximum_auxiliary_buses = schedule
            .processors
            .iter()
            .map(|processor| processor.auxiliary_buses.len())
            .max()
            .unwrap_or(0);
        let block_samples = render_contract
            .max_block_frames
            .checked_mul(render_contract.channel_count())
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        Ok(Self {
            instances,
            parameter_events: ProcessorParameterEventScratch::new(
                maximum_parameter_lanes,
                parameter_event_capacity,
            ),
            render_contract,
            maximum_parameter_lanes,
            parameter_event_capacity,
            session_scratch_bytes,
            auxiliary_pcm: vec![
                0.0;
                maximum_auxiliary_buses
                    .checked_mul(block_samples)
                    .ok_or(AudioExecutionError::BufferTooLarge)?
            ],
            sidechain_frame_db: vec![0.0; render_contract.max_block_frames],
            sidechain_gains: vec![0.0; block_samples],
        })
    }

    pub(crate) fn occurrence_count(&self) -> usize {
        self.instances.len()
    }

    pub(crate) const fn maximum_parameter_lanes(&self) -> usize {
        self.maximum_parameter_lanes
    }

    pub(crate) const fn parameter_event_capacity(&self) -> usize {
        self.parameter_event_capacity
    }

    pub(crate) const fn session_scratch_bytes(&self) -> usize {
        self.session_scratch_bytes
    }

    pub(crate) fn enter_state(
        &mut self,
        processors: &[PreparedProcessor],
        entry: AudioStateEntry,
    ) -> Result<(), AudioExecutionError> {
        if processors.len() != self.instances.len() {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        for (processor, instance) in processors.iter().zip(&mut self.instances) {
            if processor.occurrence != instance.occurrence {
                return Err(AudioExecutionError::InvalidPreparedSchedule);
            }
            if !processor.factory.contract().requires_state_entry() {
                if !matches!(instance.continuity, HostedProcessorContinuity::Stateless) {
                    return Err(AudioExecutionError::InvalidPreparedSchedule);
                }
                continue;
            }
            if matches!(
                processor.occurrence.owner,
                AudioProcessorOccurrenceOwner::Contribution { .. }
            ) {
                instance.continuity = HostedProcessorContinuity::Pending { epoch: entry.epoch };
            } else {
                let signal_start_sample =
                    subtract_signal_delay(entry.start_sample, processor.input_signal_delay_frames)?;
                if let Err(error) = isolate_adapter_call("state entry", || {
                    instance.processor.enter_state(signal_start_sample)
                }) {
                    instance.continuity = HostedProcessorContinuity::Failed;
                    return Err(error.into());
                }
                instance.continuity = HostedProcessorContinuity::Active {
                    epoch: entry.epoch,
                    next_sample: signal_start_sample,
                };
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process_rack(
        &mut self,
        rack: &PreparedRack,
        processors: &[PreparedProcessor],
        processor_auxiliary_buses: &[PreparedProcessorAuxiliaryBus],
        sidechain_bindings: &[PreparedSidechainBinding],
        request: AudioRenderRequest,
        sample_rate: AudioSampleRate,
        channel_layout: AudioChannelLayout,
        backend: AudioKernelBackend,
        pcm: &mut [f32],
        sidechain_source_buffers: &[Vec<f32>],
        sidechain_delay_lines: &mut [FixedDelayLine],
    ) -> Result<(), AudioExecutionError> {
        let expected_samples = request
            .frames
            .checked_mul(channel_layout.channel_count())
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        if pcm.len() != expected_samples
            || sample_rate.hz() != self.render_contract.sample_rate
            || channel_layout != self.render_contract.channel_layout
            || request.frames > self.render_contract.max_block_frames
        {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        let instances = &mut self.instances;
        let parameter_events = &mut self.parameter_events;
        let auxiliary_pcm = &mut self.auxiliary_pcm;
        let sidechain_frame_db = &mut self.sidechain_frame_db;
        let sidechain_gains = &mut self.sidechain_gains;
        for processor_index in rack.processors.clone() {
            let processor = processors
                .get(processor_index)
                .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            let instance = instances
                .get_mut(processor_index)
                .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            if instance.occurrence != processor.occurrence {
                return Err(AudioExecutionError::InvalidPreparedSchedule);
            }
            let signal_request = AudioRenderRequest {
                start_sample: subtract_signal_delay(
                    request.start_sample,
                    processor.input_signal_delay_frames,
                )?,
                frames: request.frames,
            };
            match instance.continuity {
                HostedProcessorContinuity::Stateless => {
                    if processor.factory.contract().requires_state_entry() {
                        return Err(AudioExecutionError::InvalidPreparedSchedule);
                    }
                }
                HostedProcessorContinuity::Unentered => {
                    return Err(AudioExecutionError::StateEntryRequired);
                }
                HostedProcessorContinuity::Failed => {
                    return Err(AudioProcessorHostError::PoisonedInstance.into());
                }
                HostedProcessorContinuity::Pending { epoch } => {
                    if let Err(error) = isolate_adapter_call("lazy state entry", || {
                        instance.processor.enter_state(signal_request.start_sample)
                    }) {
                        instance.continuity = HostedProcessorContinuity::Failed;
                        return Err(error.into());
                    }
                    instance.continuity = HostedProcessorContinuity::Active {
                        epoch,
                        next_sample: signal_request.start_sample,
                    };
                }
                HostedProcessorContinuity::Active { next_sample, .. }
                    if next_sample != signal_request.start_sample =>
                {
                    return Err(AudioExecutionError::InvalidPreparedSchedule);
                }
                HostedProcessorContinuity::Active { .. } => {}
            }
            let batch = prepare_parameter_event_batch(
                processor,
                signal_request,
                sample_rate,
                parameter_events,
            )?;
            let context = AudioProcessorProcessContext::new(
                signal_request,
                sample_rate,
                channel_layout,
                self.render_contract.processing_mode,
                backend,
            );
            let auxiliary_buses = prepare_processor_auxiliary_buses(
                processor,
                processor_auxiliary_buses,
                sidechain_bindings,
                sidechain_source_buffers,
                sidechain_delay_lines,
                signal_request,
                sample_rate,
                expected_samples,
                channel_layout.channel_count(),
                auxiliary_pcm,
                sidechain_frame_db,
                sidechain_gains,
            )?;
            let auxiliary_samples = auxiliary_buses
                .len()
                .checked_mul(expected_samples)
                .ok_or(AudioExecutionError::BufferTooLarge)?;
            let mut audio = MainBusAudioIo {
                channel_layout,
                frames: request.frames,
                pcm,
                auxiliary_buses,
                auxiliary_pcm: &auxiliary_pcm[..auxiliary_samples],
            };
            if let Err(error) = isolate_adapter_call("block processing", || {
                instance.processor.process(context, &mut audio, batch)
            }) {
                instance.continuity = HostedProcessorContinuity::Failed;
                return Err(error.into());
            }
            if let HostedProcessorContinuity::Active { epoch, .. } = instance.continuity {
                let next_sample = signal_request
                    .start_sample
                    .checked_add(
                        i64::try_from(request.frames)
                            .map_err(|_| AudioExecutionError::BufferTooLarge)?,
                    )
                    .ok_or(AudioExecutionError::BufferTooLarge)?;
                instance.continuity = HostedProcessorContinuity::Active { epoch, next_sample };
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn parameter_events_for_test(
        &mut self,
        processors: &[PreparedProcessor],
        processor_index: usize,
        request: AudioRenderRequest,
        sample_rate: AudioSampleRate,
    ) -> Result<Vec<(ParameterId, Vec<AudioParameterEvent>)>, AudioExecutionError> {
        let processor = processors
            .get(processor_index)
            .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
        let signal_request = AudioRenderRequest {
            start_sample: subtract_signal_delay(
                request.start_sample,
                processor.input_signal_delay_frames,
            )?,
            frames: request.frames,
        };
        let batch = prepare_parameter_event_batch(
            processor,
            signal_request,
            sample_rate,
            &mut self.parameter_events,
        )?;
        (0..batch.lane_count())
            .map(|lane| {
                let parameter_id = batch
                    .parameter_id(lane)
                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?
                    .clone();
                let events = batch
                    .events(lane)
                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?
                    .to_vec();
                Ok((parameter_id, events))
            })
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_processor_auxiliary_buses<'a>(
    processor: &PreparedProcessor,
    processor_auxiliary_buses: &'a [PreparedProcessorAuxiliaryBus],
    sidechain_bindings: &[PreparedSidechainBinding],
    sidechain_source_buffers: &[Vec<f32>],
    sidechain_delay_lines: &mut [FixedDelayLine],
    signal_request: AudioRenderRequest,
    sample_rate: AudioSampleRate,
    samples: usize,
    channels: usize,
    auxiliary_pcm: &mut [f32],
    sidechain_frame_db: &mut [f64],
    sidechain_gains: &mut [f32],
) -> Result<&'a [PreparedProcessorAuxiliaryBus], AudioExecutionError> {
    let buses = processor_auxiliary_buses
        .get(processor.auxiliary_buses.clone())
        .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
    let required_samples =
        buses.len().checked_mul(samples).ok_or(AudioExecutionError::BufferTooLarge)?;
    let auxiliary_pcm = auxiliary_pcm
        .get_mut(..required_samples)
        .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
    auxiliary_pcm.fill(0.0);
    for (bus_index, bus) in buses.iter().enumerate() {
        let start = bus_index.checked_mul(samples).ok_or(AudioExecutionError::BufferTooLarge)?;
        let end = start.checked_add(samples).ok_or(AudioExecutionError::BufferTooLarge)?;
        let destination = &mut auxiliary_pcm[start..end];
        for binding_index in bus.bindings.clone() {
            let binding = sidechain_bindings
                .get(binding_index)
                .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            let source = sidechain_source_buffers
                .get(binding.source_buffer_slot)
                .and_then(|source| source.get(..samples))
                .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            let delay_line = sidechain_delay_lines
                .get_mut(binding_index)
                .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            if let Some(curve) = &binding.gain_automation {
                let frame_db = sidechain_frame_db
                    .get_mut(..signal_request.frames)
                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
                let mut cursor = curve.initial_cursor(signal_request.start_sample);
                for (frame, value) in frame_db.iter_mut().enumerate() {
                    let sample = signal_request
                        .start_sample
                        .checked_add(
                            i64::try_from(frame)
                                .map_err(|_| AudioExecutionError::BufferTooLarge)?,
                        )
                        .ok_or(AudioExecutionError::BufferTooLarge)?;
                    *value = curve.evaluate_sample(sample, sample_rate, &mut cursor)?;
                }
                let gains = sidechain_gains
                    .get_mut(..samples)
                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
                dsp::expand_frame_db_to_interleaved_gains(frame_db, channels, gains);
                delay_line.add_interleaved_with_gains(source, destination, gains)?;
            } else {
                let gain =
                    binding.constant_gain.ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
                delay_line.add_interleaved_constant(source, destination, gain)?;
            }
        }
    }
    Ok(buses)
}

fn isolate_adapter_call<T>(
    operation: &'static str,
    call: impl FnOnce() -> Result<T, AudioProcessorHostError>,
) -> Result<T, AudioProcessorHostError> {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(result) => result,
        Err(_) => Err(AudioProcessorHostError::AdapterPanicked(operation)),
    }
}

fn isolate_adapter_value<T>(
    operation: &'static str,
    call: impl FnOnce() -> T,
) -> Result<T, AudioProcessorHostError> {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(value) => Ok(value),
        Err(_) => Err(AudioProcessorHostError::AdapterPanicked(operation)),
    }
}

fn subtract_signal_delay(
    execution_sample: i64,
    signal_delay_frames: usize,
) -> Result<i64, AudioExecutionError> {
    let signal_delay =
        i64::try_from(signal_delay_frames).map_err(|_| AudioExecutionError::BufferTooLarge)?;
    execution_sample
        .checked_sub(signal_delay)
        .ok_or(AudioExecutionError::BufferTooLarge)
}

impl fmt::Debug for PreparedProcessorHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedProcessorHost")
            .field("occurrence_count", &self.instances.len())
            .field("render_contract", &self.render_contract)
            .field("maximum_parameter_lanes", &self.maximum_parameter_lanes)
            .field("parameter_event_capacity", &self.parameter_event_capacity)
            .field("session_scratch_bytes", &self.session_scratch_bytes)
            .finish()
    }
}

pub(crate) fn default_processor_resolver() -> &'static BuiltInAudioProcessorResolver {
    static RESOLVER: BuiltInAudioProcessorResolver = BuiltInAudioProcessorResolver;
    &RESOLVER
}

pub(crate) fn prepare_processor_factory(
    resolver: &dyn AudioProcessorResolver,
    processor: &crate::plan::CompiledProcessor,
    occurrence: AudioProcessorOccurrence,
    contract: AudioRenderContract,
) -> Result<PreparedProcessorFactoryBinding, AudioProcessorHostError> {
    PreparedProcessorFactoryBinding::resolve(
        resolver,
        AudioProcessorPrepareRequest::new(
            occurrence,
            &processor.definition,
            &processor.parameters,
            processor.opaque_state.as_deref(),
            contract,
        ),
    )
}
