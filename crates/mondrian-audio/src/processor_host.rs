//! Plan-time processor resolution and Session-owned callback dispatch.

use crate::dsp;
use crate::processor_parameters::{
    fill_parameter_lane_values, prepare_parameter_event_batch, ProcessorParameterEventScratch,
};
use crate::schedule::{PreparedAudioSchedule, PreparedProcessor, PreparedRack};
#[cfg(test)]
use crate::AudioParameterEvent;
use crate::{
    AudioExecutionError, AudioKernelBackend, AudioParameterEventBatch, AudioProcessor,
    AudioProcessorAudioIo, AudioProcessorExecutionContract, AudioProcessorFactory,
    AudioProcessorHostError, AudioProcessorInputBus, AudioProcessorOccurrence,
    AudioProcessorPrepareRequest, AudioProcessorProcessContext, AudioProcessorResolver,
    AudioRenderContract, AudioRenderRequest, AudioStateEntry, BuiltInAudioProcessorResolver,
};
use mondrian_core::{AudioChannelLayout, AudioSampleRate, ParameterId};
use mondrian_timeline::audio::{
    gain_parameter_schema, AudioProcessorDefinitionRef, BUILTIN_GAIN_DEFINITION_ID,
    GAIN_DB_PARAMETER_ID,
};
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

impl AudioProcessorResolver for BuiltInAudioProcessorResolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        match request.definition() {
            AudioProcessorDefinitionRef::BuiltIn { definition_id, schema_version }
                if definition_id == BUILTIN_GAIN_DEFINITION_ID && *schema_version == 1 =>
            {
                let parameter_id = ParameterId::new_static(GAIN_DB_PARAMETER_ID);
                let Some((parameter_slot, parameter)) = request
                    .parameters()
                    .iter()
                    .enumerate()
                    .find(|(_, (candidate, _))| *candidate == &parameter_id)
                    .map(|(slot, (_, parameter))| (slot, parameter))
                else {
                    return Err(AudioProcessorHostError::InvalidContract(
                        "built-in Gain is missing its canonical parameter".to_owned(),
                    ));
                };
                if request.parameters().len() != 1
                    || parameter.schema != gain_parameter_schema()
                    || parameter.automation.parameter_id != parameter_id
                    || request.opaque_state().is_some()
                {
                    return Err(AudioProcessorHostError::InvalidContract(
                        "built-in Gain definition snapshot or state is not canonical".to_owned(),
                    ));
                }
                let session_scratch_bytes = request
                    .render_contract()
                    .max_block_frames
                    .checked_mul(std::mem::size_of::<f64>())
                    .and_then(|bytes| {
                        request
                            .render_contract()
                            .max_block_frames
                            .checked_mul(request.render_contract().channel_count())
                            .and_then(|samples| {
                                samples
                                    .checked_mul(std::mem::size_of::<f32>())
                                    .and_then(|gain_bytes| bytes.checked_add(gain_bytes))
                            })
                    })
                    .ok_or_else(|| {
                        AudioProcessorHostError::InvalidContract(
                            "built-in Gain scratch capacity overflowed".to_owned(),
                        )
                    })?;
                let execution_contract = AudioProcessorExecutionContract::new(
                    0,
                    false,
                    true,
                    true,
                    session_scratch_bytes,
                )?;
                Ok(Arc::new(BuiltInGainFactory {
                    parameter_slot,
                    max_block_frames: request.render_contract().max_block_frames,
                    channel_layout: request.render_contract().channel_layout,
                    execution_contract,
                }))
            }
            AudioProcessorDefinitionRef::BuiltIn { definition_id, schema_version } => {
                Err(AudioProcessorHostError::Unavailable(format!(
                    "built-in {definition_id} schema {schema_version}"
                )))
            }
            AudioProcessorDefinitionRef::Vst3 { class_id, schema_version, .. } => {
                Err(AudioProcessorHostError::Unavailable(format!(
                    "VST3 class {class_id} schema {schema_version}"
                )))
            }
            AudioProcessorDefinitionRef::Clap { plugin_id, schema_version } => {
                Err(AudioProcessorHostError::Unavailable(format!(
                    "CLAP plugin {plugin_id} schema {schema_version}"
                )))
            }
        }
    }
}

#[derive(Debug)]
struct BuiltInGainFactory {
    parameter_slot: usize,
    max_block_frames: usize,
    channel_layout: AudioChannelLayout,
    execution_contract: AudioProcessorExecutionContract,
}

impl AudioProcessorFactory for BuiltInGainFactory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.execution_contract
    }

    fn create(&self) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        let samples = self
            .max_block_frames
            .checked_mul(self.channel_layout.channel_count())
            .ok_or_else(|| {
                AudioProcessorHostError::InstanceCreation(
                    "built-in Gain scratch capacity overflowed".to_owned(),
                )
            })?;
        Ok(Box::new(BuiltInGainProcessor {
            parameter_slot: self.parameter_slot,
            frame_values: vec![0.0; self.max_block_frames],
            interleaved_gains: vec![0.0; samples],
        }))
    }
}

struct BuiltInGainProcessor {
    parameter_slot: usize,
    frame_values: Vec<f64>,
    interleaved_gains: Vec<f32>,
}

impl AudioProcessor for BuiltInGainProcessor {
    fn enter_state(&mut self, _start_sample: i64) -> Result<(), AudioProcessorHostError> {
        Ok(())
    }

    fn process(
        &mut self,
        context: AudioProcessorProcessContext,
        audio: &mut dyn AudioProcessorAudioIo,
        parameters: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        let request = context.request();
        let channels = context.channel_layout().channel_count();
        let samples = request.frames.checked_mul(channels).ok_or_else(|| {
            AudioProcessorHostError::Process("built-in Gain block size overflowed".to_owned())
        })?;
        if audio.main_layout() != context.channel_layout()
            || audio.frames() != request.frames
            || audio.main_interleaved().len() != samples
            || parameters.block_start_sample() != request.start_sample
            || parameters.block_frames() != request.frames
            || request.frames > self.frame_values.len()
            || samples > self.interleaved_gains.len()
        {
            return Err(AudioProcessorHostError::Process(
                "built-in Gain received an inconsistent prepared block".to_owned(),
            ));
        }
        let events = parameters.events(self.parameter_slot).ok_or_else(|| {
            AudioProcessorHostError::Process("built-in Gain parameter lane is absent".to_owned())
        })?;
        if let [event] = events {
            if event.sample_offset != 0 {
                return Err(AudioProcessorHostError::Process(
                    "built-in Gain constant event is not at offset zero".to_owned(),
                ));
            }
            dsp::multiply_constant_in_place(
                context.kernel_backend(),
                audio.main_interleaved(),
                dsp::db_to_linear(event.value),
            );
        } else {
            fill_parameter_lane_values(
                parameters,
                self.parameter_slot,
                &mut self.frame_values[..request.frames],
            )
            .map_err(|error| AudioProcessorHostError::Process(error.to_string()))?;
            dsp::expand_frame_db_to_interleaved_gains(
                &self.frame_values[..request.frames],
                channels,
                &mut self.interleaved_gains[..samples],
            );
            dsp::multiply_in_place(
                context.kernel_backend(),
                audio.main_interleaved(),
                &self.interleaved_gains[..samples],
            );
        }
        Ok(())
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
        let instances = schedule
            .processors
            .iter()
            .map(|processor| {
                if processor.parameter_ids.len() != processor.parameter_curves.len() {
                    return Err(AudioExecutionError::InvalidPreparedSchedule);
                }
                Ok(HostedProcessorInstance {
                    occurrence: processor.occurrence,
                    processor: processor.factory.create()?,
                })
            })
            .collect::<Result<Vec<_>, AudioExecutionError>>()?;
        let parameter_event_capacity = schedule.summary.maximum_parameter_events_per_block;
        let session_scratch_bytes = schedule
            .processors
            .iter()
            .try_fold(0_usize, |bytes, processor| {
                bytes.checked_add(processor.factory.contract().session_scratch_bytes())
            })
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
