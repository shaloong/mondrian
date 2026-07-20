//! Session-preallocated lowering of prepared curves into processor event batches.

use crate::schedule::PreparedProcessor;
use crate::{
    AudioExecutionError, AudioParameterEvent, AudioParameterEventBatch, AudioRenderRequest,
};
use mondrian_core::AudioSampleRate;
use std::ops::Range;

#[derive(Debug)]
pub(crate) struct ProcessorParameterEventScratch {
    lane_ranges: Vec<Range<usize>>,
    events: Vec<AudioParameterEvent>,
}

impl ProcessorParameterEventScratch {
    pub(crate) fn new(maximum_lanes: usize, event_capacity: usize) -> Self {
        Self {
            lane_ranges: Vec::with_capacity(maximum_lanes),
            events: Vec::with_capacity(event_capacity),
        }
    }
}

pub(crate) fn prepare_parameter_event_batch<'a>(
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

pub(crate) fn fill_parameter_lane_values(
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
