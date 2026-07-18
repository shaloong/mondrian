use crate::dsp;
use crate::plan::{CompiledChannelStrip, CompiledProcessor, CompiledRack};
use crate::schedule::{
    PreparedAudioPlan, PreparedAudioSchedule, PreparedContribution, PreparedNodeOrigin,
    PreparedTransitionBinding, PreparedTransitionDirection,
};
use mondrian_core::{AudioComponentEditId, AudioSampleRate, TimelineTime, TimelineTimeError};
use mondrian_timeline::audio::{AudioChannelStripOutputPort, AudioTransitionCurve};
use std::sync::Arc;

/// Pull-style source Adapter for generated contribution identities.
pub trait AudioPcmSource {
    /// Fill one indexed interleaved contribution block.
    ///
    /// `source_frames` contains one absolute source position for each output
    /// frame. Negative positions are silence. Implementations must preserve
    /// order and duplicates, accept non-contiguous/reverse coordinates, and
    /// either fill the complete pre-zeroed destination or return an error.
    /// Values outside `[-1, 1]` are legal.
    fn read_indexed_interleaved(
        &mut self,
        edit: AudioComponentEditId,
        source_frames: &[i64],
        channels: usize,
        destination: &mut [f32],
    ) -> Result<(), AudioExecutionError>;
}

/// One exact block requested from a prepared Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioRenderRequest {
    /// First Sequence-domain sample frame on the plan's Evaluation Grid.
    pub start_sample: i64,
    /// Exact number of sample frames.
    pub frames: usize,
}

#[derive(Debug)]
struct NodeBuffers {
    input: Vec<f32>,
    pre_fader: Vec<f32>,
    post_fader_pre_mute: Vec<f32>,
    post_mute: Vec<f32>,
}

impl NodeBuffers {
    fn new(samples: usize) -> Self {
        Self {
            input: vec![0.0; samples],
            pre_fader: vec![0.0; samples],
            post_fader_pre_mute: vec![0.0; samples],
            post_mute: vec![0.0; samples],
        }
    }

    fn clear(&mut self, samples: usize) {
        self.input[..samples].fill(0.0);
        self.pre_fader[..samples].fill(0.0);
        self.post_fader_pre_mute[..samples].fill(0.0);
        self.post_mute[..samples].fill(0.0);
    }

    fn port(&self, port: AudioChannelStripOutputPort, samples: usize) -> &[f32] {
        match port {
            AudioChannelStripOutputPort::PreFader => &self.pre_fader[..samples],
            AudioChannelStripOutputPort::PostFaderPreMute => &self.post_fader_pre_mute[..samples],
            AudioChannelStripOutputPort::PostMute => &self.post_mute[..samples],
        }
    }
}

#[derive(Debug)]
struct RenderScratch {
    source_frames: Vec<i64>,
    contribution_pcm: Vec<f32>,
    contribution_sample_gains: Vec<f32>,
    strip_pre_gains: Vec<f32>,
    strip_post_gains: Vec<f32>,
}

impl RenderScratch {
    fn new(max_frames: usize, samples: usize) -> Self {
        Self {
            source_frames: vec![-1; max_frames],
            contribution_pcm: vec![0.0; samples],
            contribution_sample_gains: vec![0.0; samples],
            strip_pre_gains: vec![0.0; samples],
            strip_post_gains: vec![0.0; samples],
        }
    }
}

/// Exclusive mutable execution state for one Playback, Export, or Audition run.
pub struct AudioRenderSession {
    plan: Arc<PreparedAudioPlan>,
    node_buffers: Vec<NodeBuffers>,
    scratch: RenderScratch,
}

impl AudioRenderSession {
    /// Allocate all scratch storage before realtime execution begins.
    pub fn new(plan: Arc<PreparedAudioPlan>) -> Result<Self, AudioExecutionError> {
        let contract = plan.contract();
        let samples = contract
            .max_block_frames
            .checked_mul(contract.channels)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        let node_buffers = (0..plan.schedule.summary.scratch_slot_count)
            .map(|_| NodeBuffers::new(samples))
            .collect();
        Ok(Self {
            plan,
            node_buffers,
            scratch: RenderScratch::new(contract.max_block_frames, samples),
        })
    }

    /// Render into caller-owned interleaved float storage without allocation.
    pub fn render_into(
        &mut self,
        source: &mut impl AudioPcmSource,
        request: AudioRenderRequest,
        destination: &mut [f32],
    ) -> Result<(), AudioExecutionError> {
        let contract = self.plan.contract();
        if request.frames > contract.max_block_frames {
            return Err(AudioExecutionError::BlockTooLarge);
        }
        let samples = request
            .frames
            .checked_mul(contract.channels)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        if destination.len() != samples {
            return Err(AudioExecutionError::OutputSizeMismatch);
        }
        destination.fill(0.0);

        let schedule = &self.plan.schedule;
        let backend = self.plan.kernel_backend();
        let sample_rate = AudioSampleRate::new(contract.sample_rate)?;
        for node_slot in 0..schedule.nodes.len() {
            let node = &schedule.nodes[node_slot];
            let scratch_slot = node.scratch_slot;
            self.node_buffers[scratch_slot].clear(samples);

            if matches!(node.origin, PreparedNodeOrigin::Track(_)) {
                render_track_contributions(
                    schedule,
                    node_slot,
                    source,
                    request,
                    sample_rate,
                    contract.channels,
                    backend,
                    &mut self.node_buffers[scratch_slot],
                    &mut self.scratch,
                )?;
            } else {
                for route_index in node.incoming.clone() {
                    sum_prepared_route(
                        schedule,
                        route_index,
                        samples,
                        backend,
                        &mut self.node_buffers,
                    )?;
                }
            }

            process_strip(
                &node.strip,
                node.muted,
                node.constant_pre_gain,
                node.constant_post_gain,
                request,
                contract.sample_rate,
                contract.channels,
                backend,
                &mut self.node_buffers[scratch_slot],
                &mut self.scratch,
            )?;
        }

        let output_scratch = schedule.nodes[schedule.output_slot].scratch_slot;
        destination.copy_from_slice(&self.node_buffers[output_scratch].post_mute[..samples]);
        Ok(())
    }
}

/// Allocation-owning deterministic reference entry point.
pub fn render_audio(
    plan: Arc<PreparedAudioPlan>,
    source: &mut impl AudioPcmSource,
    request: AudioRenderRequest,
) -> Result<Vec<f32>, AudioExecutionError> {
    let samples = request
        .frames
        .checked_mul(plan.contract().channels)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let mut output = vec![0.0; samples];
    AudioRenderSession::new(plan)?.render_into(source, request, &mut output)?;
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn render_track_contributions(
    schedule: &PreparedAudioSchedule,
    node_slot: usize,
    source: &mut impl AudioPcmSource,
    request: AudioRenderRequest,
    sample_rate: AudioSampleRate,
    channels: usize,
    backend: crate::AudioKernelBackend,
    track: &mut NodeBuffers,
    scratch: &mut RenderScratch,
) -> Result<(), AudioExecutionError> {
    let samples = request
        .frames
        .checked_mul(channels)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    for contribution_index in schedule.nodes[node_slot].contributions.clone() {
        let contribution = &schedule.contributions[contribution_index];
        let scope = &schedule.scopes[contribution.scope_slot];
        scratch.source_frames[..request.frames].fill(-1);
        scratch.contribution_sample_gains[..samples].fill(0.0);
        let Some(active_frames) = prepare_source_frames(
            contribution,
            request,
            sample_rate,
            &mut scratch.source_frames[..request.frames],
        )?
        else {
            continue;
        };
        let has_audible_frame = scratch.source_frames[active_frames.clone()]
            .iter()
            .any(|source_frame| *source_frame >= 0);
        if let Some((gain, pan)) = contribution.constant_gain_pan {
            for frame in active_frames.clone() {
                for channel in 0..channels {
                    scratch.contribution_sample_gains[frame * channels + channel] =
                        gain * stereo_balance_gain(channel, channels, pan);
                }
            }
        } else {
            for frame in active_frames {
                let absolute_sample = request
                    .start_sample
                    .checked_add(
                        i64::try_from(frame).map_err(|_| AudioExecutionError::BufferTooLarge)?,
                    )
                    .ok_or(AudioExecutionError::BufferTooLarge)?;
                let sequence_time =
                    TimelineTime::new(absolute_sample, i64::from(sample_rate.hz()))?;
                let clip_local =
                    sequence_time.checked_sub(contribution.semantic.sequence_range.start)?;
                let scope_time = contribution.semantic.scope_in.checked_add(clip_local)?;
                let edit_time = contribution.semantic.local_time_in.checked_add(clip_local)?;
                let scope_gain_db = scope
                    .input_gain_automation
                    .as_ref()
                    .map_or(Ok(scope.input_gain_db), |curve| curve.evaluate(scope_time))?
                    + scope.rack.gain_db(scope_time)?;
                let volume_db = contribution
                    .semantic
                    .volume_automation
                    .as_ref()
                    .map_or(Ok(contribution.semantic.volume_db), |curve| {
                        curve.evaluate(edit_time)
                    })?;
                let pan = contribution
                    .semantic
                    .pan_automation
                    .as_ref()
                    .map_or(Ok(contribution.semantic.pan), |curve| {
                        curve.evaluate(edit_time)
                    })?
                    .clamp(-1.0, 1.0);
                let envelope = contribution_envelope(
                    contribution,
                    clip_local,
                    sequence_time,
                    &schedule.transitions[contribution.transitions.clone()],
                )?;
                let gain = db_to_linear(scope_gain_db + volume_db) * envelope;
                for channel in 0..channels {
                    scratch.contribution_sample_gains[frame * channels + channel] =
                        gain * stereo_balance_gain(channel, channels, pan);
                }
            }
        }
        if !has_audible_frame {
            continue;
        }
        scratch.contribution_pcm[..samples].fill(0.0);
        source.read_indexed_interleaved(
            contribution.semantic.edit_id,
            &scratch.source_frames[..request.frames],
            channels,
            &mut scratch.contribution_pcm[..samples],
        )?;
        dsp::multiply_add(
            backend,
            &mut track.input[..samples],
            &scratch.contribution_pcm[..samples],
            &scratch.contribution_sample_gains[..samples],
        );
    }
    Ok(())
}

fn prepare_source_frames(
    contribution: &PreparedContribution,
    request: AudioRenderRequest,
    sample_rate: AudioSampleRate,
    destination: &mut [i64],
) -> Result<Option<std::ops::Range<usize>>, AudioExecutionError> {
    let request_frames =
        i64::try_from(request.frames).map_err(|_| AudioExecutionError::BufferTooLarge)?;
    let request_end = request
        .start_sample
        .checked_add(request_frames)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let active_start = request.start_sample.max(contribution.sequence_start_sample);
    let active_end = request_end.min(contribution.sequence_end_sample);
    if active_start >= active_end {
        return Ok(None);
    }
    let first_index = usize::try_from(active_start - request.start_sample)
        .map_err(|_| AudioExecutionError::BufferTooLarge)?;
    let active_len = usize::try_from(active_end - active_start)
        .map_err(|_| AudioExecutionError::BufferTooLarge)?;
    let active_range = first_index..first_index.saturating_add(active_len);

    let sequence_time = TimelineTime::new(active_start, i64::from(sample_rate.hz()))?;
    let source_time = contribution.semantic.source_time_map.map(sequence_time)?;
    let scale = contribution.semantic.source_time_map.speed.scale();
    let denominator = i128::from(source_time.denominator())
        .checked_mul(i128::from(scale.denominator()))
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let mut numerator = i128::from(source_time.numerator())
        .checked_mul(i128::from(sample_rate.hz()))
        .and_then(|value| value.checked_mul(i128::from(scale.denominator())))
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let step = i128::from(scale.numerator())
        .checked_mul(i128::from(source_time.denominator()))
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    for frame in active_range.clone() {
        destination[frame] = i64::try_from(numerator.div_euclid(denominator))
            .map_err(|_| AudioExecutionError::BufferTooLarge)?;
        numerator = numerator.checked_add(step).ok_or(AudioExecutionError::BufferTooLarge)?;
    }
    Ok(Some(active_range))
}

fn sum_prepared_route(
    schedule: &PreparedAudioSchedule,
    route_index: usize,
    samples: usize,
    backend: crate::AudioKernelBackend,
    buffers: &mut [NodeBuffers],
) -> Result<(), AudioExecutionError> {
    let route = schedule
        .routes
        .get(route_index)
        .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
    let source_scratch = schedule.nodes[route.source_slot].scratch_slot;
    let destination_scratch = schedule.nodes[route.destination_slot].scratch_slot;
    if source_scratch == destination_scratch {
        return Err(AudioExecutionError::InvalidPreparedSchedule);
    }
    if source_scratch < destination_scratch {
        let (left, right) = buffers.split_at_mut(destination_scratch);
        let source = left[source_scratch].port(route.source_port, samples);
        dsp::add(backend, &mut right[0].input[..samples], source);
    } else {
        let (left, right) = buffers.split_at_mut(source_scratch);
        let destination = &mut left[destination_scratch].input[..samples];
        let source = right[0].port(route.source_port, samples);
        dsp::add(backend, destination, source);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_strip(
    strip: &CompiledChannelStrip,
    muted: bool,
    constant_pre_gain: Option<f32>,
    constant_post_gain: Option<f32>,
    request: AudioRenderRequest,
    sample_rate: u32,
    channels: usize,
    backend: crate::AudioKernelBackend,
    buffers: &mut NodeBuffers,
    scratch: &mut RenderScratch,
) -> Result<(), AudioExecutionError> {
    let samples = request
        .frames
        .checked_mul(channels)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    if let (Some(pre_gain), Some(post_gain)) = (constant_pre_gain, constant_post_gain) {
        dsp::multiply_constant_into(
            backend,
            &mut buffers.pre_fader[..samples],
            &buffers.input[..samples],
            pre_gain,
        );
        dsp::multiply_constant_into(
            backend,
            &mut buffers.post_fader_pre_mute[..samples],
            &buffers.pre_fader[..samples],
            post_gain,
        );
    } else {
        for frame in 0..request.frames {
            let absolute_sample = request
                .start_sample
                .checked_add(i64::try_from(frame).map_err(|_| AudioExecutionError::BufferTooLarge)?)
                .ok_or(AudioExecutionError::BufferTooLarge)?;
            let time = TimelineTime::new(absolute_sample, i64::from(sample_rate))?;
            let pre_gain = db_to_linear(strip.input_trim_db + strip.pre_fader.gain_db(time)?);
            let fader_db = strip
                .fader_automation
                .as_ref()
                .map_or(Ok(strip.fader_db), |curve| curve.evaluate(time))?;
            let post_gain = db_to_linear(fader_db + strip.post_fader.gain_db(time)?);
            scratch.strip_pre_gains[frame * channels..(frame + 1) * channels].fill(pre_gain);
            scratch.strip_post_gains[frame * channels..(frame + 1) * channels].fill(post_gain);
        }
        dsp::multiply_into(
            backend,
            &mut buffers.pre_fader[..samples],
            &buffers.input[..samples],
            &scratch.strip_pre_gains[..samples],
        );
        dsp::multiply_into(
            backend,
            &mut buffers.post_fader_pre_mute[..samples],
            &buffers.pre_fader[..samples],
            &scratch.strip_post_gains[..samples],
        );
    }
    if muted {
        buffers.post_mute[..samples].fill(0.0);
    } else {
        buffers.post_mute[..samples].copy_from_slice(&buffers.post_fader_pre_mute[..samples]);
    }
    Ok(())
}

fn contribution_envelope(
    contribution: &PreparedContribution,
    clip_local: TimelineTime,
    sequence_time: TimelineTime,
    transitions: &[PreparedTransitionBinding],
) -> Result<f32, AudioExecutionError> {
    let mut gain = 1.0_f32;
    if let Some((duration, curve)) = contribution.semantic.fade_in {
        if duration > TimelineTime::ZERO && clip_local < duration {
            gain *= rising_curve(clip_local.to_f64() / duration.to_f64(), curve);
        }
    }
    if let Some((duration, curve)) = contribution.semantic.fade_out {
        if duration > TimelineTime::ZERO {
            let remaining =
                contribution.semantic.sequence_range.duration.checked_sub(clip_local)?;
            if remaining < duration {
                gain *= falling_curve(remaining.to_f64() / duration.to_f64(), curve);
            }
        }
    }
    for transition in transitions {
        if !transition.sequence_range.contains(sequence_time)? {
            continue;
        }
        let local = sequence_time.checked_sub(transition.sequence_range.start)?;
        let progress =
            (local.to_f64() / transition.sequence_range.duration.to_f64()).clamp(0.0, 1.0);
        gain *= match transition.direction {
            PreparedTransitionDirection::Rising => transition_rising(progress, transition.curve),
            PreparedTransitionDirection::Falling => transition_falling(progress, transition.curve),
        };
    }
    Ok(gain)
}

fn rising_curve(progress: f64, curve: mondrian_timeline::audio::AudioFadeCurve) -> f32 {
    match curve {
        mondrian_timeline::audio::AudioFadeCurve::ConstantGain => progress as f32,
        mondrian_timeline::audio::AudioFadeCurve::EqualPower => {
            (progress * std::f64::consts::FRAC_PI_2).sin() as f32
        }
    }
}

fn falling_curve(progress_remaining: f64, curve: mondrian_timeline::audio::AudioFadeCurve) -> f32 {
    rising_curve(progress_remaining, curve)
}

fn transition_rising(progress: f64, curve: AudioTransitionCurve) -> f32 {
    match curve {
        AudioTransitionCurve::ConstantGain => progress as f32,
        AudioTransitionCurve::EqualPower => (progress * std::f64::consts::FRAC_PI_2).sin() as f32,
    }
}

fn transition_falling(progress: f64, curve: AudioTransitionCurve) -> f32 {
    match curve {
        AudioTransitionCurve::ConstantGain => (1.0 - progress) as f32,
        AudioTransitionCurve::EqualPower => (progress * std::f64::consts::FRAC_PI_2).cos() as f32,
    }
}

fn stereo_balance_gain(channel: usize, channels: usize, pan: f64) -> f32 {
    if channels < 2 {
        return 1.0;
    }
    match channel {
        0 if pan > 0.0 => (pan * std::f64::consts::FRAC_PI_2).cos() as f32,
        1 if pan < 0.0 => (-pan * std::f64::consts::FRAC_PI_2).cos() as f32,
        _ => 1.0,
    }
}

impl CompiledRack {
    fn gain_db(&self, time: TimelineTime) -> Result<f64, AudioExecutionError> {
        self.processors.iter().try_fold(0.0, |sum, processor| match processor {
            CompiledProcessor::Gain { automation } => {
                Ok(sum + automation.as_ref().map_or(Ok(0.0), |curve| curve.evaluate(time))?)
            }
        })
    }
}

fn db_to_linear(db: f64) -> f32 {
    10.0_f64.powf(db / 20.0) as f32
}

/// Deterministic DSP execution failure.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AudioExecutionError {
    /// Requested allocation or index cannot be represented.
    #[error("audio render buffer is too large")]
    BufferTooLarge,
    /// Request exceeds the prepared maximum block size.
    #[error("audio render block exceeds the prepared maximum")]
    BlockTooLarge,
    /// Caller-provided output size disagrees with the Render Contract.
    #[error("audio render output size does not match the request")]
    OutputSizeMismatch,
    /// A prepared Track was unexpectedly absent.
    #[error("prepared audio Track is missing")]
    MissingPreparedTrack,
    /// A prepared Bus was unexpectedly absent.
    #[error("prepared audio Bus is missing")]
    MissingPreparedBus,
    /// A prepared processing scope was unexpectedly absent.
    #[error("prepared audio processing scope is missing")]
    MissingPreparedScope,
    /// Dense schedule storage disagrees with its immutable preparation facts.
    #[error("prepared audio schedule is internally inconsistent")]
    InvalidPreparedSchedule,
    /// A media or nested source Adapter could not provide required PCM.
    #[error("audio source is unavailable: {0}")]
    SourceUnavailable(String),
    /// Exact timeline arithmetic failed.
    #[error(transparent)]
    Time(#[from] TimelineTimeError),
    /// Exact author-time to sample-grid conversion failed.
    #[error(transparent)]
    AudioTime(#[from] mondrian_core::AudioTimeError),
    /// Automation authoring/evaluation failed.
    #[error("invalid audio automation: {0}")]
    Automation(String),
}

impl From<mondrian_core::AutomationError> for AudioExecutionError {
    fn from(error: mondrian_core::AutomationError) -> Self {
        Self::Automation(error.to_string())
    }
}
