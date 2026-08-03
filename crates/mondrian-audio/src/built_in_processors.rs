//! Canonical native Processor definitions realized by the shared Host.

use crate::dsp;
use crate::processor_parameters::fill_parameter_lane_values;
use crate::{
    AudioParameterEventBatch, AudioProcessor, AudioProcessorAudioIo,
    AudioProcessorExecutionContract, AudioProcessorFactory, AudioProcessorHostError,
    AudioProcessorPrepareRequest, AudioProcessorProcessContext, AudioProcessorResolver,
    AudioProcessorTail, BuiltInAudioProcessorResolver,
};
use mondrian_core::{AudioChannelLayout, ParameterId};
use mondrian_timeline::audio::{
    gain_parameter_schema, sample_delay_frames_parameter_schema, AudioProcessorDefinitionRef,
    BUILTIN_GAIN_DEFINITION_ID, BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID,
    BUILTIN_SAMPLE_DELAY_DEFINITION_ID, GAIN_DB_PARAMETER_ID, SAMPLE_DELAY_FRAMES_PARAMETER_ID,
    SAMPLE_DELAY_MAX_FRAMES,
};
use std::sync::Arc;

impl AudioProcessorResolver for BuiltInAudioProcessorResolver {
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        match request.definition() {
            AudioProcessorDefinitionRef::BuiltIn { definition_id, schema_version }
                if definition_id == BUILTIN_GAIN_DEFINITION_ID && *schema_version == 1 =>
            {
                prepare_gain(request)
            }
            AudioProcessorDefinitionRef::BuiltIn { definition_id, schema_version }
                if definition_id == BUILTIN_SAMPLE_DELAY_DEFINITION_ID && *schema_version == 1 =>
            {
                prepare_sample_delay(request)
            }
            AudioProcessorDefinitionRef::BuiltIn { definition_id, schema_version }
                if definition_id == BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID
                    && *schema_version == 1 =>
            {
                crate::lookahead_limiter::prepare_lookahead_limiter(request)
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

fn prepare_gain(
    request: AudioProcessorPrepareRequest<'_>,
) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
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
        AudioProcessorTail::None,
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

fn prepare_sample_delay(
    request: AudioProcessorPrepareRequest<'_>,
) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
    let parameter_id = ParameterId::new_static(SAMPLE_DELAY_FRAMES_PARAMETER_ID);
    let Some((parameter_slot, parameter)) = request
        .parameters()
        .iter()
        .enumerate()
        .find(|(_, (candidate, _))| *candidate == &parameter_id)
        .map(|(slot, (_, parameter))| (slot, parameter))
    else {
        return Err(AudioProcessorHostError::InvalidContract(
            "built-in Sample Delay is missing its canonical parameter".to_owned(),
        ));
    };
    let value = parameter.automation.default_value;
    if request.parameters().len() != 1
        || parameter.schema != sample_delay_frames_parameter_schema()
        || parameter.automation.parameter_id != parameter_id
        || !parameter.automation.keyframes.is_empty()
        || !value.is_finite()
        || value.fract() != 0.0
        || !(0.0..=SAMPLE_DELAY_MAX_FRAMES as f64).contains(&value)
        || request.opaque_state().is_some()
    {
        return Err(AudioProcessorHostError::InvalidContract(
            "built-in Sample Delay definition snapshot, value, or state is not canonical"
                .to_owned(),
        ));
    }
    let delay_frames = usize::try_from(value as i64).map_err(|_| {
        AudioProcessorHostError::InvalidContract(
            "built-in Sample Delay length is not representable".to_owned(),
        )
    })?;
    let delay_samples = delay_frames
        .checked_mul(request.render_contract().channel_count())
        .ok_or_else(|| {
            AudioProcessorHostError::InvalidContract(
                "built-in Sample Delay capacity overflowed".to_owned(),
            )
        })?;
    let session_scratch_bytes =
        delay_samples.checked_mul(std::mem::size_of::<f32>()).ok_or_else(|| {
            AudioProcessorHostError::InvalidContract(
                "built-in Sample Delay scratch bytes overflowed".to_owned(),
            )
        })?;
    // This is intentional audible delay, not hidden implementation latency.
    // Reporting it as compensable latency would make PDC cancel the effect.
    let execution_contract = AudioProcessorExecutionContract::new(
        0,
        if delay_frames == 0 {
            AudioProcessorTail::None
        } else {
            AudioProcessorTail::Finite(delay_frames)
        },
        delay_frames > 0,
        true,
        true,
        session_scratch_bytes,
    )?;
    Ok(Arc::new(BuiltInSampleDelayFactory {
        parameter_slot,
        delay_frames,
        channel_layout: request.render_contract().channel_layout,
        execution_contract,
    }))
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

#[derive(Debug)]
struct BuiltInSampleDelayFactory {
    parameter_slot: usize,
    delay_frames: usize,
    channel_layout: AudioChannelLayout,
    execution_contract: AudioProcessorExecutionContract,
}

impl AudioProcessorFactory for BuiltInSampleDelayFactory {
    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        self.execution_contract
    }

    fn create(&self) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        let delay_samples = self
            .delay_frames
            .checked_mul(self.channel_layout.channel_count())
            .ok_or_else(|| {
                AudioProcessorHostError::InstanceCreation(
                    "built-in Sample Delay capacity overflowed".to_owned(),
                )
            })?;
        Ok(Box::new(BuiltInSampleDelayProcessor {
            parameter_slot: self.parameter_slot,
            delay_frames: self.delay_frames,
            delay_line: vec![0.0; delay_samples],
            cursor: 0,
        }))
    }
}

struct BuiltInSampleDelayProcessor {
    parameter_slot: usize,
    delay_frames: usize,
    delay_line: Vec<f32>,
    cursor: usize,
}

impl AudioProcessor for BuiltInSampleDelayProcessor {
    fn enter_state(&mut self, _start_sample: i64) -> Result<(), AudioProcessorHostError> {
        self.delay_line.fill(0.0);
        self.cursor = 0;
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
            AudioProcessorHostError::Process(
                "built-in Sample Delay block size overflowed".to_owned(),
            )
        })?;
        let delay_samples = self.delay_frames.checked_mul(channels).ok_or_else(|| {
            AudioProcessorHostError::Process("built-in Sample Delay capacity overflowed".to_owned())
        })?;
        if audio.main_layout() != context.channel_layout()
            || audio.frames() != request.frames
            || audio.main_interleaved().len() != samples
            || parameters.block_start_sample() != request.start_sample
            || parameters.block_frames() != request.frames
            || self.delay_line.len() != delay_samples
        {
            return Err(AudioProcessorHostError::Process(
                "built-in Sample Delay received an inconsistent prepared block".to_owned(),
            ));
        }
        let events = parameters.events(self.parameter_slot).ok_or_else(|| {
            AudioProcessorHostError::Process(
                "built-in Sample Delay parameter lane is absent".to_owned(),
            )
        })?;
        if request.frames == 0 {
            if !events.is_empty() {
                return Err(AudioProcessorHostError::Process(
                    "built-in Sample Delay received events for an empty block".to_owned(),
                ));
            }
            return Ok(());
        }
        let [event] = events else {
            return Err(AudioProcessorHostError::Process(
                "built-in Sample Delay requires one constant parameter event".to_owned(),
            ));
        };
        if event.sample_offset != 0 || event.value != self.delay_frames as f64 {
            return Err(AudioProcessorHostError::Process(
                "built-in Sample Delay parameter changed after preparation".to_owned(),
            ));
        }
        if self.delay_line.is_empty() {
            return Ok(());
        }
        for sample in audio.main_interleaved() {
            std::mem::swap(sample, &mut self.delay_line[self.cursor]);
            self.cursor += 1;
            if self.cursor == self.delay_line.len() {
                self.cursor = 0;
            }
        }
        Ok(())
    }
}
