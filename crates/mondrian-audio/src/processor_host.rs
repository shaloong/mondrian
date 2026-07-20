//! Plan-time processor resolution and Session-owned callback dispatch.

use crate::processor_parameters::{prepare_parameter_event_batch, ProcessorParameterEventScratch};
use crate::schedule::{PreparedAudioSchedule, PreparedProcessor, PreparedRack};
#[cfg(test)]
use crate::AudioParameterEvent;
use crate::{
    AudioExecutionError, AudioKernelBackend, AudioProcessor, AudioProcessorAudioIo,
    AudioProcessorExecutionContract, AudioProcessorFactory, AudioProcessorHostError,
    AudioProcessorInputBus, AudioProcessorOccurrence, AudioProcessorPrepareRequest,
    AudioProcessorProcessContext, AudioProcessorResolver, AudioRenderContract, AudioRenderRequest,
    AudioStateEntry, BuiltInAudioProcessorResolver,
};
#[cfg(test)]
use mondrian_core::ParameterId;
use mondrian_core::{AudioChannelLayout, AudioSampleRate};
use std::fmt;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct PreparedProcessorFactoryBinding {
    contract: AudioProcessorExecutionContract,
    factory: Arc<dyn AudioProcessorFactory>,
}

impl PreparedProcessorFactoryBinding {
    pub(crate) fn resolve(
        resolver: &dyn AudioProcessorResolver,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Self, AudioProcessorHostError> {
        let mode = request.render_contract().processing_mode;
        let factory = resolver.prepare(request)?;
        let contract = factory.execution_contract();
        if !contract.admits(mode) {
            return Err(AudioProcessorHostError::InvalidContract(format!(
                "processor does not admit {mode:?} execution"
            )));
        }
        Ok(Self { contract, factory })
    }

    pub(crate) const fn contract(&self) -> AudioProcessorExecutionContract {
        self.contract
    }

    fn create(&self) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        if self.factory.execution_contract() != self.contract {
            return Err(AudioProcessorHostError::InvalidContract(
                "prepared factory changed its execution contract before Session creation"
                    .to_owned(),
            ));
        }
        self.factory.create()
    }
}

impl fmt::Debug for PreparedProcessorFactoryBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedProcessorFactoryBinding")
            .field("contract", &self.contract)
            .finish_non_exhaustive()
    }
}

struct MainBusAudioIo<'a> {
    channel_layout: AudioChannelLayout,
    frames: usize,
    pcm: &'a mut [f32],
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

    fn auxiliary_input(&self, _bus_key: &str) -> Option<AudioProcessorInputBus<'_>> {
        None
    }
}

struct HostedProcessorInstance {
    occurrence: AudioProcessorOccurrence,
    processor: Box<dyn AudioProcessor>,
}

pub(crate) struct PreparedProcessorHost {
    instances: Vec<HostedProcessorInstance>,
    parameter_events: ProcessorParameterEventScratch,
    render_contract: AudioRenderContract,
    maximum_parameter_lanes: usize,
    parameter_event_capacity: usize,
    session_scratch_bytes: usize,
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
                Ok(HostedProcessorInstance {
                    occurrence: processor.occurrence,
                    processor: processor.factory.create()?,
                })
            })
            .collect::<Result<Vec<_>, AudioExecutionError>>()?;
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
            instance.processor.enter_state(entry.start_sample)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process_rack(
        &mut self,
        rack: &PreparedRack,
        processors: &[PreparedProcessor],
        request: AudioRenderRequest,
        sample_rate: AudioSampleRate,
        channel_layout: AudioChannelLayout,
        backend: AudioKernelBackend,
        pcm: &mut [f32],
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
        let context = AudioProcessorProcessContext::new(
            request,
            sample_rate,
            channel_layout,
            self.render_contract.processing_mode,
            backend,
        );
        let instances = &mut self.instances;
        let parameter_events = &mut self.parameter_events;
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
            let batch =
                prepare_parameter_event_batch(processor, request, sample_rate, parameter_events)?;
            let mut audio = MainBusAudioIo { channel_layout, frames: request.frames, pcm };
            instance.processor.process(context, &mut audio, batch)?;
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
        let batch = prepare_parameter_event_batch(
            processor,
            request,
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
