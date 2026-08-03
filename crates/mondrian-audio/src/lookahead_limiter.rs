//! Linked-channel sample-peak Lookahead Limiter behind the shared Processor Host.

use crate::dsp;
use crate::processor_parameters::fill_parameter_lane_values;
use crate::{
    AudioParameterEventBatch, AudioProcessor, AudioProcessorAudioIo,
    AudioProcessorExecutionContract, AudioProcessorFactory, AudioProcessorHostError,
    AudioProcessorPrepareRequest, AudioProcessorProcessContext, AudioProcessorTail,
};
use mondrian_core::{AudioChannelLayout, ParameterId};
use mondrian_timeline::audio::{
    lookahead_limiter_ceiling_parameter_schema, lookahead_limiter_lookahead_parameter_schema,
    lookahead_limiter_release_parameter_schema, LOOKAHEAD_LIMITER_CEILING_DB_PARAMETER_ID,
    LOOKAHEAD_LIMITER_LOOKAHEAD_MS_PARAMETER_ID, LOOKAHEAD_LIMITER_MAX_LOOKAHEAD_MS,
    LOOKAHEAD_LIMITER_RELEASE_MS_PARAMETER_ID,
};
use std::sync::Arc;

const CEILING_DB_MIN: f64 = -24.0;
const CEILING_DB_MAX: f64 = 0.0;
const RELEASE_MS_MIN: f64 = 5.0;
const RELEASE_MS_MAX: f64 = 5_000.0;

pub(super) fn prepare_lookahead_limiter(
    request: AudioProcessorPrepareRequest<'_>,
) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
    let ceiling_id = ParameterId::new_static(LOOKAHEAD_LIMITER_CEILING_DB_PARAMETER_ID);
    let lookahead_id = ParameterId::new_static(LOOKAHEAD_LIMITER_LOOKAHEAD_MS_PARAMETER_ID);
    let release_id = ParameterId::new_static(LOOKAHEAD_LIMITER_RELEASE_MS_PARAMETER_ID);
    let (ceiling_slot, ceiling) = parameter(&request, &ceiling_id, "Ceiling")?;
    let (lookahead_slot, lookahead) = parameter(&request, &lookahead_id, "Lookahead")?;
    let (release_slot, release) = parameter(&request, &release_id, "Release")?;
    if request.parameters().len() != 3
        || ceiling.schema != lookahead_limiter_ceiling_parameter_schema()
        || lookahead.schema != lookahead_limiter_lookahead_parameter_schema()
        || release.schema != lookahead_limiter_release_parameter_schema()
        || ceiling.automation.parameter_id != ceiling_id
        || lookahead.automation.parameter_id != lookahead_id
        || release.automation.parameter_id != release_id
        || !lookahead.automation.keyframes.is_empty()
        || request.opaque_state().is_some()
    {
        return Err(AudioProcessorHostError::InvalidContract(
            "built-in Lookahead Limiter definition snapshot or state is not canonical".to_owned(),
        ));
    }

    let lookahead_frames = milliseconds_to_frames_ceil(
        lookahead.automation.default_value,
        request.render_contract().sample_rate,
    )?;
    let channel_layout = request.render_contract().channel_layout;
    let channels = channel_layout.channel_count();
    let max_block_frames = request.render_contract().max_block_frames;
    let delayed_samples = lookahead_frames.checked_mul(channels).ok_or_else(|| {
        AudioProcessorHostError::InvalidContract(
            "built-in Lookahead Limiter delay capacity overflowed".to_owned(),
        )
    })?;
    let block_samples = max_block_frames.checked_mul(channels).ok_or_else(|| {
        AudioProcessorHostError::InvalidContract(
            "built-in Lookahead Limiter block capacity overflowed".to_owned(),
        )
    })?;
    let peak_capacity = lookahead_frames.checked_add(1).ok_or_else(|| {
        AudioProcessorHostError::InvalidContract(
            "built-in Lookahead Limiter peak window overflowed".to_owned(),
        )
    })?;
    let session_scratch_bytes = limiter_scratch_bytes(
        delayed_samples,
        lookahead_frames,
        peak_capacity,
        max_block_frames,
        block_samples,
    )?;
    let execution_contract = AudioProcessorExecutionContract::new(
        lookahead_frames,
        AudioProcessorTail::None,
        true,
        true,
        true,
        session_scratch_bytes,
    )?;
    Ok(Arc::new(LookaheadLimiterFactory {
        ceiling_slot,
        lookahead_slot,
        release_slot,
        lookahead_ms: lookahead.automation.default_value,
        lookahead_frames,
        max_block_frames,
        channel_layout,
        execution_contract,
    }))
}

fn parameter<'a>(
    request: &'a AudioProcessorPrepareRequest<'_>,
    id: &ParameterId,
    name: &str,
) -> Result<(usize, &'a mondrian_timeline::audio::AudioProcessorParameter), AudioProcessorHostError>
{
    request
        .parameters()
        .iter()
        .enumerate()
        .find(|(_, (candidate, _))| *candidate == id)
        .map(|(slot, (_, parameter))| (slot, parameter))
        .ok_or_else(|| {
            AudioProcessorHostError::InvalidContract(format!(
                "built-in Lookahead Limiter is missing its canonical {name} parameter"
            ))
        })
}

fn milliseconds_to_frames_ceil(
    milliseconds: f64,
    sample_rate: u32,
) -> Result<usize, AudioProcessorHostError> {
    let frames = milliseconds * f64::from(sample_rate) / 1_000.0;
    let rounded = frames.ceil();
    if !milliseconds.is_finite()
        || !(0.0..=LOOKAHEAD_LIMITER_MAX_LOOKAHEAD_MS).contains(&milliseconds)
        || !rounded.is_finite()
        || rounded > usize::MAX as f64
    {
        return Err(AudioProcessorHostError::InvalidContract(
            "built-in Lookahead Limiter lookahead is not representable".to_owned(),
        ));
    }
    Ok(rounded as usize)
}

fn limiter_scratch_bytes(
    delayed_samples: usize,
    lookahead_frames: usize,
    peak_capacity: usize,
    max_block_frames: usize,
    block_samples: usize,
) -> Result<usize, AudioProcessorHostError> {
    let sections = [
        delayed_samples.checked_mul(std::mem::size_of::<f32>()),
        lookahead_frames.checked_mul(std::mem::size_of::<f64>()),
        lookahead_frames.checked_mul(std::mem::size_of::<f64>()),
        peak_capacity.checked_mul(std::mem::size_of::<f32>()),
        peak_capacity.checked_mul(std::mem::size_of::<u64>()),
        max_block_frames.checked_mul(std::mem::size_of::<f64>()),
        max_block_frames.checked_mul(std::mem::size_of::<f64>()),
        block_samples.checked_mul(std::mem::size_of::<f32>()),
    ];
    sections
        .into_iter()
        .try_fold(0_usize, |total, section| total.checked_add(section?))
        .ok_or_else(|| {
            AudioProcessorHostError::InvalidContract(
                "built-in Lookahead Limiter scratch bytes overflowed".to_owned(),
            )
        })
}

#[derive(Debug)]
struct LookaheadLimiterFactory {
    ceiling_slot: usize,
    lookahead_slot: usize,
    release_slot: usize,
    lookahead_ms: f64,
    lookahead_frames: usize,
    max_block_frames: usize,
    channel_layout: AudioChannelLayout,
    execution_contract: AudioProcessorExecutionContract,
}

impl AudioProcessorFactory for LookaheadLimiterFactory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.execution_contract
    }

    fn create(&self) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        let channels = self.channel_layout.channel_count();
        let delayed_samples = self.lookahead_frames.checked_mul(channels).ok_or_else(|| {
            AudioProcessorHostError::InstanceCreation(
                "built-in Lookahead Limiter delay capacity overflowed".to_owned(),
            )
        })?;
        let block_samples = self.max_block_frames.checked_mul(channels).ok_or_else(|| {
            AudioProcessorHostError::InstanceCreation(
                "built-in Lookahead Limiter block capacity overflowed".to_owned(),
            )
        })?;
        let peak_capacity = self.lookahead_frames.checked_add(1).ok_or_else(|| {
            AudioProcessorHostError::InstanceCreation(
                "built-in Lookahead Limiter peak capacity overflowed".to_owned(),
            )
        })?;
        Ok(Box::new(LookaheadLimiterProcessor {
            ceiling_slot: self.ceiling_slot,
            lookahead_slot: self.lookahead_slot,
            release_slot: self.release_slot,
            lookahead_ms: self.lookahead_ms,
            lookahead_frames: self.lookahead_frames,
            channel_layout: self.channel_layout,
            delayed_audio: vec![0.0; delayed_samples],
            delayed_ceiling_db: vec![0.0; self.lookahead_frames],
            delayed_release_ms: vec![0.0; self.lookahead_frames],
            delay_cursor: 0,
            peaks: FixedPeakWindow::new(peak_capacity),
            processed_frames: 0,
            applied_gain: 1.0,
            last_release_ms: None,
            last_release_coefficient: 0.0,
            ceiling_values: vec![0.0; self.max_block_frames],
            release_values: vec![0.0; self.max_block_frames],
            interleaved_gains: vec![0.0; block_samples],
        }))
    }
}

struct LookaheadLimiterProcessor {
    ceiling_slot: usize,
    lookahead_slot: usize,
    release_slot: usize,
    lookahead_ms: f64,
    lookahead_frames: usize,
    channel_layout: AudioChannelLayout,
    delayed_audio: Vec<f32>,
    delayed_ceiling_db: Vec<f64>,
    delayed_release_ms: Vec<f64>,
    delay_cursor: usize,
    peaks: FixedPeakWindow,
    processed_frames: u64,
    applied_gain: f32,
    last_release_ms: Option<f64>,
    last_release_coefficient: f32,
    ceiling_values: Vec<f64>,
    release_values: Vec<f64>,
    interleaved_gains: Vec<f32>,
}

impl AudioProcessor for LookaheadLimiterProcessor {
    fn enter_state(&mut self, _start_sample: i64) -> Result<(), AudioProcessorHostError> {
        self.delayed_audio.fill(0.0);
        self.delayed_ceiling_db.fill(0.0);
        self.delayed_release_ms.fill(0.0);
        self.delay_cursor = 0;
        self.peaks.clear();
        self.processed_frames = 0;
        self.applied_gain = 1.0;
        self.last_release_ms = None;
        self.last_release_coefficient = 0.0;
        Ok(())
    }

    fn process(
        &mut self,
        context: AudioProcessorProcessContext,
        audio: &mut dyn AudioProcessorAudioIo,
        parameters: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        let request = context.request();
        let channels = self.channel_layout.channel_count();
        let samples = request
            .frames
            .checked_mul(channels)
            .ok_or_else(|| process_error("block sample capacity overflowed"))?;
        let delayed_samples = self
            .lookahead_frames
            .checked_mul(channels)
            .ok_or_else(|| process_error("delay sample capacity overflowed"))?;
        if context.channel_layout() != self.channel_layout
            || audio.main_layout() != self.channel_layout
            || audio.frames() != request.frames
            || audio.main_interleaved().len() != samples
            || parameters.block_start_sample() != request.start_sample
            || parameters.block_frames() != request.frames
            || request.frames > self.ceiling_values.len()
            || request.frames > self.release_values.len()
            || samples > self.interleaved_gains.len()
            || self.delayed_audio.len() != delayed_samples
            || self.delayed_ceiling_db.len() != self.lookahead_frames
            || self.delayed_release_ms.len() != self.lookahead_frames
        {
            return Err(process_error("received an inconsistent prepared block"));
        }
        if audio.main_interleaved().iter().any(|sample| !sample.is_finite()) {
            return Err(process_error("received non-finite PCM"));
        }
        validate_lookahead_lane(
            parameters,
            self.lookahead_slot,
            self.lookahead_ms,
            request.frames,
        )?;
        fill_parameter_lane_values(
            parameters,
            self.ceiling_slot,
            &mut self.ceiling_values[..request.frames],
        )
        .map_err(|error| process_error(&error.to_string()))?;
        fill_parameter_lane_values(
            parameters,
            self.release_slot,
            &mut self.release_values[..request.frames],
        )
        .map_err(|error| process_error(&error.to_string()))?;
        if self.ceiling_values[..request.frames]
            .iter()
            .any(|value| !value.is_finite() || !(CEILING_DB_MIN..=CEILING_DB_MAX).contains(value))
            || self.release_values[..request.frames].iter().any(|value| {
                !value.is_finite() || !(RELEASE_MS_MIN..=RELEASE_MS_MAX).contains(value)
            })
        {
            return Err(process_error("received an out-of-contract parameter value"));
        }
        let frame_count = u64::try_from(request.frames)
            .map_err(|_| process_error("block frame count is not representable"))?;
        self.processed_frames
            .checked_add(frame_count)
            .ok_or_else(|| process_error("processed-frame identity overflowed"))?;
        let lookahead_frames = u64::try_from(self.lookahead_frames)
            .map_err(|_| process_error("lookahead is not representable"))?;

        if request.frames == 0 {
            return Ok(());
        }
        let pcm = audio.main_interleaved();
        for frame in 0..request.frames {
            let sample_start = frame
                .checked_mul(channels)
                .ok_or_else(|| process_error("frame sample offset overflowed"))?;
            let sample_end = sample_start
                .checked_add(channels)
                .ok_or_else(|| process_error("frame sample extent overflowed"))?;
            let frame_peak = pcm[sample_start..sample_end]
                .iter()
                .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
            let minimum_peak_index = self.processed_frames.saturating_sub(lookahead_frames);
            self.peaks.expire_before(minimum_peak_index);
            self.peaks
                .push(self.processed_frames, frame_peak)
                .map_err(|()| process_error("peak window capacity was violated"))?;

            let mut ceiling_db = self.ceiling_values[frame];
            let mut release_ms = self.release_values[frame];
            if self.lookahead_frames > 0 {
                std::mem::swap(
                    &mut ceiling_db,
                    &mut self.delayed_ceiling_db[self.delay_cursor],
                );
                std::mem::swap(
                    &mut release_ms,
                    &mut self.delayed_release_ms[self.delay_cursor],
                );
                let delayed_start = self
                    .delay_cursor
                    .checked_mul(channels)
                    .ok_or_else(|| process_error("delay cursor offset overflowed"))?;
                for channel in 0..channels {
                    std::mem::swap(
                        &mut pcm[sample_start + channel],
                        &mut self.delayed_audio[delayed_start + channel],
                    );
                }
                self.delay_cursor += 1;
                if self.delay_cursor == self.lookahead_frames {
                    self.delay_cursor = 0;
                }
            }

            let ceiling_linear = dsp::db_to_linear(ceiling_db);
            let peak = self
                .peaks
                .maximum()
                .ok_or_else(|| process_error("peak window lost the current frame"))?;
            let target_gain = if peak > ceiling_linear {
                ceiling_linear / peak
            } else {
                1.0
            };
            if self.processed_frames >= lookahead_frames {
                let release_coefficient = if self.last_release_ms == Some(release_ms) {
                    self.last_release_coefficient
                } else {
                    let coefficient = release_coefficient(release_ms, context.sample_rate().hz());
                    self.last_release_ms = Some(release_ms);
                    self.last_release_coefficient = coefficient;
                    coefficient
                };
                let released_gain = 1.0 - (1.0 - self.applied_gain) * release_coefficient;
                self.applied_gain = if target_gain < self.applied_gain {
                    target_gain
                } else {
                    released_gain.min(target_gain)
                };
            } else {
                self.applied_gain = 1.0;
            }
            self.ceiling_values[frame] = ceiling_db;
            self.interleaved_gains[sample_start..sample_end].fill(self.applied_gain);
            self.processed_frames = self
                .processed_frames
                .checked_add(1)
                .ok_or_else(|| process_error("processed-frame identity overflowed"))?;
        }
        dsp::multiply_in_place(
            context.kernel_backend(),
            pcm,
            &self.interleaved_gains[..samples],
        );
        for frame in 0..request.frames {
            let ceiling = dsp::db_to_linear(self.ceiling_values[frame]);
            let start = frame * channels;
            for sample in &mut pcm[start..start + channels] {
                *sample = sample.clamp(-ceiling, ceiling);
            }
        }
        Ok(())
    }
}

fn validate_lookahead_lane(
    parameters: AudioParameterEventBatch<'_>,
    slot: usize,
    expected: f64,
    frames: usize,
) -> Result<(), AudioProcessorHostError> {
    let events = parameters
        .events(slot)
        .ok_or_else(|| process_error("Lookahead parameter lane is absent"))?;
    if frames == 0 {
        return if events.is_empty() {
            Ok(())
        } else {
            Err(process_error(
                "received Lookahead events for an empty block",
            ))
        };
    }
    let [event] = events else {
        return Err(process_error(
            "Lookahead must remain constant after preparation",
        ));
    };
    if event.sample_offset != 0 || event.value != expected {
        return Err(process_error("Lookahead changed after preparation"));
    }
    Ok(())
}

fn release_coefficient(milliseconds: f64, sample_rate: u32) -> f32 {
    let samples = milliseconds * f64::from(sample_rate) / 1_000.0;
    (-1.0 / samples).exp() as f32
}

fn process_error(reason: &str) -> AudioProcessorHostError {
    AudioProcessorHostError::Process(format!("built-in Lookahead Limiter {reason}"))
}

struct FixedPeakWindow {
    values: Vec<f32>,
    indices: Vec<u64>,
    head: usize,
    len: usize,
}

impl FixedPeakWindow {
    fn new(capacity: usize) -> Self {
        Self {
            values: vec![0.0; capacity],
            indices: vec![0; capacity],
            head: 0,
            len: 0,
        }
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    fn expire_before(&mut self, minimum_index: u64) {
        while self.len > 0 && self.indices[self.head] < minimum_index {
            self.head = (self.head + 1) % self.values.len();
            self.len -= 1;
        }
    }

    fn push(&mut self, index: u64, value: f32) -> Result<(), ()> {
        while self.len > 0 {
            let back = (self.head + self.len - 1) % self.values.len();
            if self.values[back] > value {
                break;
            }
            self.len -= 1;
        }
        if self.len == self.values.len() {
            return Err(());
        }
        let tail = (self.head + self.len) % self.values.len();
        self.values[tail] = value;
        self.indices[tail] = index;
        self.len += 1;
        Ok(())
    }

    fn maximum(&self) -> Option<f32> {
        (self.len > 0).then(|| self.values[self.head])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookahead_duration_rounds_up_to_the_audio_grid() {
        assert_eq!(milliseconds_to_frames_ceil(0.0, 48_000), Ok(0));
        assert_eq!(milliseconds_to_frames_ceil(0.01, 48_000), Ok(1));
        assert_eq!(milliseconds_to_frames_ceil(5.0, 48_000), Ok(240));
        assert!(milliseconds_to_frames_ceil(20.001, 48_000).is_err());
        assert!(milliseconds_to_frames_ceil(f64::NAN, 48_000).is_err());
    }

    #[test]
    fn fixed_peak_window_retains_only_the_monotonic_live_maxima() {
        let mut window = FixedPeakWindow::new(3);
        window.push(0, 0.5).expect("first");
        window.push(1, 0.25).expect("second");
        window.push(2, 0.75).expect("third");
        assert_eq!(window.maximum(), Some(0.75));
        window.expire_before(3);
        assert_eq!(window.maximum(), None);
        window.push(3, 0.4).expect("replacement");
        assert_eq!(window.maximum(), Some(0.4));
    }
}
