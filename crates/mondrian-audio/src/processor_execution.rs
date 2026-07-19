//! Session-owned execution of prepared processor occurrences.

use crate::dsp;
use crate::schedule::{
    PreparedAudioSchedule, PreparedProcessor, PreparedProcessorOperation, PreparedRack,
};
use crate::{
    AudioExecutionError, AudioKernelBackend, AudioParameterEvent, AudioParameterEventBatch,
    AudioRenderRequest,
};
use mondrian_core::AudioSampleRate;
#[cfg(test)]
use mondrian_core::ParameterId;
use std::ops::Range;

#[derive(Debug)]
pub(crate) struct PreparedProcessorRuntime {
    states: Vec<ProcessorState>,
    parameter_events: ProcessorParameterEventScratch,
    maximum_parameter_lanes: usize,
    parameter_event_capacity: usize,
}

impl PreparedProcessorRuntime {
    pub(crate) fn new(schedule: &PreparedAudioSchedule) -> Result<Self, AudioExecutionError> {
        let maximum_parameter_lanes = schedule
            .processors
            .iter()
            .map(|processor| processor.parameter_ids.len())
            .max()
            .unwrap_or(0);
        let states = schedule
            .processors
            .iter()
            .map(|processor| {
                if processor.parameter_ids.len() != processor.parameter_curves.len() {
                    return Err(AudioExecutionError::InvalidPreparedSchedule);
                }
                Ok(match processor.operation {
                    PreparedProcessorOperation::Gain { parameter_slot }
                        if parameter_slot < processor.parameter_ids.len() =>
                    {
                        ProcessorState::Gain { origin: processor.origin }
                    }
                    PreparedProcessorOperation::Gain { .. } => {
                        return Err(AudioExecutionError::InvalidPreparedSchedule);
                    }
                })
            })
            .collect::<Result<Vec<_>, AudioExecutionError>>()?;
        let parameter_event_capacity = schedule.summary.maximum_parameter_events_per_block;
        Ok(Self {
            states,
            parameter_events: ProcessorParameterEventScratch::new(
                maximum_parameter_lanes,
                parameter_event_capacity,
            ),
            maximum_parameter_lanes,
            parameter_event_capacity,
        })
    }

    pub(crate) fn occurrence_count(&self) -> usize {
        self.states.len()
    }

    pub(crate) const fn maximum_parameter_lanes(&self) -> usize {
        self.maximum_parameter_lanes
    }

    pub(crate) const fn parameter_event_capacity(&self) -> usize {
        self.parameter_event_capacity
    }

    pub(crate) fn enter_state(
        &mut self,
        processors: &[PreparedProcessor],
    ) -> Result<(), AudioExecutionError> {
        if processors.len() != self.states.len() {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        for (processor, state) in processors.iter().zip(&self.states) {
            if !state.matches(processor) {
                return Err(AudioExecutionError::InvalidPreparedSchedule);
            }
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
        channels: usize,
        backend: AudioKernelBackend,
        pcm: &mut [f32],
        frame_values: &mut [f64],
        interleaved_gains: &mut [f32],
    ) -> Result<(), AudioExecutionError> {
        let expected_samples = request
            .frames
            .checked_mul(channels)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        if pcm.len() != expected_samples
            || frame_values.len() != request.frames
            || interleaved_gains.len() != expected_samples
        {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        for processor_index in rack.processors.clone() {
            let processor = processors
                .get(processor_index)
                .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            let state = self
                .states
                .get_mut(processor_index)
                .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            let batch = prepare_parameter_event_batch(
                processor,
                request,
                sample_rate,
                &mut self.parameter_events,
            )?;
            match (processor.operation, state) {
                (
                    PreparedProcessorOperation::Gain { parameter_slot },
                    ProcessorState::Gain { origin },
                ) if *origin == processor.origin => {
                    let events = batch
                        .events(parameter_slot)
                        .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
                    if let [event] = events {
                        if event.sample_offset != 0 {
                            return Err(AudioExecutionError::InvalidPreparedSchedule);
                        }
                        dsp::multiply_constant_in_place(
                            backend,
                            pcm,
                            dsp::db_to_linear(event.value),
                        );
                    } else {
                        fill_parameter_lane_values(batch, parameter_slot, frame_values)?;
                        dsp::expand_frame_db_to_interleaved_gains(
                            frame_values,
                            channels,
                            interleaved_gains,
                        );
                        dsp::multiply_in_place(backend, pcm, interleaved_gains);
                    }
                }
                _ => return Err(AudioExecutionError::InvalidPreparedSchedule),
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

#[derive(Debug, Clone, Copy)]
enum ProcessorState {
    Gain {
        origin: crate::schedule::PreparedProcessorOrigin,
    },
}

impl ProcessorState {
    fn matches(self, processor: &PreparedProcessor) -> bool {
        matches!(
            (self, processor.operation),
            (
                Self::Gain { origin },
                PreparedProcessorOperation::Gain { .. }
            ) if origin == processor.origin
        )
    }
}

#[derive(Debug)]
struct ProcessorParameterEventScratch {
    lane_ranges: Vec<Range<usize>>,
    events: Vec<AudioParameterEvent>,
}

impl ProcessorParameterEventScratch {
    fn new(maximum_lanes: usize, event_capacity: usize) -> Self {
        Self {
            lane_ranges: Vec::with_capacity(maximum_lanes),
            events: Vec::with_capacity(event_capacity),
        }
    }
}

fn prepare_parameter_event_batch<'a>(
    processor: &'a PreparedProcessor,
    request: AudioRenderRequest,
    sample_rate: AudioSampleRate,
    scratch: &'a mut ProcessorParameterEventScratch,
) -> Result<AudioParameterEventBatch<'a>, AudioExecutionError> {
    if processor.parameter_ids.len() != processor.parameter_curves.len()
        || processor.parameter_ids.len() > scratch.lane_ranges.capacity()
    {
        return Err(AudioExecutionError::InvalidPreparedSchedule);
    }
    let required_events = processor
        .parameter_curves
        .iter()
        .try_fold(0_usize, |count, curve| {
            count.checked_add(if request.frames == 0 {
                0
            } else if curve.is_constant() {
                1
            } else {
                request.frames
            })
        })
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    if required_events > scratch.events.capacity() {
        return Err(AudioExecutionError::InvalidPreparedSchedule);
    }

    scratch.lane_ranges.clear();
    scratch.events.clear();
    for curve in &processor.parameter_curves {
        let start = scratch.events.len();
        if request.frames > 0 && curve.is_constant() {
            scratch
                .events
                .push(AudioParameterEvent { sample_offset: 0, value: curve.constant_value() });
        } else if request.frames > 0 {
            let mut cursor = curve.initial_cursor(request.start_sample);
            for offset in 0..request.frames {
                let offset_i64 =
                    i64::try_from(offset).map_err(|_| AudioExecutionError::BufferTooLarge)?;
                let sample = request
                    .start_sample
                    .checked_add(offset_i64)
                    .ok_or(AudioExecutionError::BufferTooLarge)?;
                scratch.events.push(AudioParameterEvent {
                    sample_offset: u32::try_from(offset)
                        .map_err(|_| AudioExecutionError::BufferTooLarge)?,
                    value: curve.evaluate_sample(sample, sample_rate, &mut cursor)?,
                });
            }
        }
        scratch.lane_ranges.push(start..scratch.events.len());
    }
    Ok(AudioParameterEventBatch::new(
        request.start_sample,
        request.frames,
        &processor.parameter_ids,
        &scratch.lane_ranges,
        &scratch.events,
    ))
}

fn fill_parameter_lane_values(
    batch: AudioParameterEventBatch<'_>,
    lane: usize,
    destination: &mut [f64],
) -> Result<(), AudioExecutionError> {
    if destination.len() != batch.block_frames() || batch.parameter_id(lane).is_none() {
        return Err(AudioExecutionError::InvalidPreparedSchedule);
    }
    let events = batch.events(lane).ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
    if destination.is_empty() {
        return if events.is_empty() {
            Ok(())
        } else {
            Err(AudioExecutionError::InvalidPreparedSchedule)
        };
    }
    if let [event] = events {
        if event.sample_offset != 0 {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        destination.fill(event.value);
        return Ok(());
    }
    if events.len() != destination.len() {
        return Err(AudioExecutionError::InvalidPreparedSchedule);
    }
    for (offset, (event, value)) in events.iter().zip(destination).enumerate() {
        if usize::try_from(event.sample_offset).ok() != Some(offset) {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        *value = event.value;
    }
    Ok(())
}
