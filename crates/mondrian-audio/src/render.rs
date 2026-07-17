use crate::plan::{
    source_port_key, CompiledChannelStrip, CompiledProcessor, CompiledRack, PreparedAudioPlan,
};
use mondrian_core::{
    AudioComponentEditId, AudioSamplePosition, AudioSampleRate, AudioSampleRounding, MixBusId,
    TimelineTime, TimelineTimeError, TrackId,
};
use mondrian_timeline::audio::{
    AudioChannelStripOutputPort, AudioRouteDestination, AudioTransitionCurve,
};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Pull-style source Adapter for generated contribution identities.
pub trait AudioPcmSource {
    /// Return one normalized floating sample at an absolute source sample position.
    /// Values outside `[-1, 1]` are legal.
    fn sample(
        &mut self,
        edit: AudioComponentEditId,
        source_frame: i64,
        channel: usize,
    ) -> Result<f32, AudioExecutionError>;
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

/// Exclusive mutable execution state for one Playback, Export, or Audition run.
pub struct AudioRenderSession {
    plan: Arc<PreparedAudioPlan>,
    tracks: BTreeMap<TrackId, NodeBuffers>,
    buses: BTreeMap<MixBusId, NodeBuffers>,
    output: NodeBuffers,
    route_mix: Vec<f32>,
}

impl AudioRenderSession {
    /// Allocate all scratch storage before realtime execution begins.
    pub fn new(plan: Arc<PreparedAudioPlan>) -> Result<Self, AudioExecutionError> {
        let contract = plan.contract();
        let samples = contract
            .max_block_frames
            .checked_mul(contract.channels)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        let tracks = plan
            .program()
            .track_channels
            .keys()
            .map(|id| (*id, NodeBuffers::new(samples)))
            .collect();
        let buses =
            plan.program().buses.keys().map(|id| (*id, NodeBuffers::new(samples))).collect();
        Ok(Self {
            plan,
            tracks,
            buses,
            output: NodeBuffers::new(samples),
            route_mix: vec![0.0; samples],
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
        for buffers in self.tracks.values_mut() {
            buffers.clear(samples);
        }
        for buffers in self.buses.values_mut() {
            buffers.clear(samples);
        }
        self.output.clear(samples);

        let sample_rate = AudioSampleRate::new(contract.sample_rate)?;
        for contribution in &self.plan.program().contributions {
            let Some(track) = self.tracks.get_mut(&contribution.track_id) else {
                continue;
            };
            let scope = self
                .plan
                .program()
                .processing_scopes
                .get(&contribution.processing_scope)
                .ok_or(AudioExecutionError::MissingPreparedScope)?;
            for frame in 0..request.frames {
                let absolute_sample = request
                    .start_sample
                    .checked_add(
                        i64::try_from(frame).map_err(|_| AudioExecutionError::BufferTooLarge)?,
                    )
                    .ok_or(AudioExecutionError::BufferTooLarge)?;
                let sequence_time =
                    TimelineTime::new(absolute_sample, i64::from(sample_rate.hz()))?;
                if !contribution.sequence_range.contains(sequence_time)? {
                    continue;
                }
                let clip_local = sequence_time.checked_sub(contribution.sequence_range.start)?;
                let scope_time = contribution.scope_in.checked_add(clip_local)?;
                let edit_time = contribution.local_time_in.checked_add(clip_local)?;
                let source_time = contribution.source_time_map.map(sequence_time)?;
                let source_frame = AudioSamplePosition::from_timeline_time(
                    source_time,
                    sample_rate,
                    AudioSampleRounding::Floor,
                )?
                .sample();

                let scope_gain_db = scope
                    .input_gain_automation
                    .as_ref()
                    .map_or(Ok(scope.input_gain_db), |curve| curve.evaluate(scope_time))?
                    + scope.rack.gain_db(scope_time)?;
                let volume_db = contribution
                    .volume_automation
                    .as_ref()
                    .map_or(Ok(contribution.volume_db), |curve| {
                        curve.evaluate(edit_time)
                    })?;
                let pan = contribution
                    .pan_automation
                    .as_ref()
                    .map_or(Ok(contribution.pan), |curve| curve.evaluate(edit_time))?
                    .clamp(-1.0, 1.0);
                let envelope = contribution_envelope(
                    contribution,
                    clip_local,
                    sequence_time,
                    &self.plan.program().transitions,
                )?;
                let gain = db_to_linear(scope_gain_db + volume_db) * envelope;
                for channel in 0..contract.channels {
                    let pan_gain = stereo_balance_gain(channel, contract.channels, pan);
                    let value = source.sample(contribution.edit_id, source_frame, channel)?;
                    track.input[frame * contract.channels + channel] += value * gain * pan_gain;
                }
            }
        }

        for (track_id, buffers) in &mut self.tracks {
            let channel = self
                .plan
                .program()
                .track_channels
                .get(track_id)
                .ok_or(AudioExecutionError::MissingPreparedTrack)?;
            process_strip(
                &channel.strip,
                channel.muted,
                request,
                contract.sample_rate,
                contract.channels,
                buffers,
            )?;
        }

        for bus_id in &self.plan.program().bus_order {
            self.route_mix[..samples].fill(0.0);
            sum_incoming(
                &mut self.route_mix[..samples],
                AudioRouteDestination::Bus(*bus_id),
                &self.plan.program().routes,
                &self.tracks,
                &self.buses,
                samples,
            );
            let buffers =
                self.buses.get_mut(bus_id).ok_or(AudioExecutionError::MissingPreparedBus)?;
            buffers.input[..samples].copy_from_slice(&self.route_mix[..samples]);
            let strip = self
                .plan
                .program()
                .buses
                .get(bus_id)
                .ok_or(AudioExecutionError::MissingPreparedBus)?;
            process_strip(
                strip,
                false,
                request,
                contract.sample_rate,
                contract.channels,
                buffers,
            )?;
        }

        sum_incoming(
            &mut self.output.input[..samples],
            AudioRouteDestination::Output(self.plan.program().output_id),
            &self.plan.program().routes,
            &self.tracks,
            &self.buses,
            samples,
        );
        process_strip(
            &self.plan.program().output,
            false,
            request,
            contract.sample_rate,
            contract.channels,
            &mut self.output,
        )?;
        destination.copy_from_slice(&self.output.post_mute[..samples]);
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

fn process_strip(
    strip: &CompiledChannelStrip,
    muted: bool,
    request: AudioRenderRequest,
    sample_rate: u32,
    channels: usize,
    buffers: &mut NodeBuffers,
) -> Result<(), AudioExecutionError> {
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
        for channel in 0..channels {
            let index = frame * channels + channel;
            let pre = buffers.input[index] * pre_gain;
            let post = pre * post_gain;
            buffers.pre_fader[index] = pre;
            buffers.post_fader_pre_mute[index] = post;
            buffers.post_mute[index] = if muted { 0.0 } else { post };
        }
    }
    Ok(())
}

fn sum_incoming(
    destination: &mut [f32],
    destination_node: AudioRouteDestination,
    routes: &[mondrian_timeline::audio::AudioRoute],
    tracks: &BTreeMap<TrackId, NodeBuffers>,
    buses: &BTreeMap<MixBusId, NodeBuffers>,
    samples: usize,
) {
    for route in routes.iter().filter(|route| route.destination == destination_node) {
        let (track_id, bus_id, port) = source_port_key(route.source);
        let source = track_id
            .and_then(|id| tracks.get(&id))
            .or_else(|| bus_id.and_then(|id| buses.get(&id)))
            .map(|buffers| buffers.port(port, samples));
        if let Some(source) = source {
            for (destination, source) in destination.iter_mut().zip(source) {
                *destination += *source;
            }
        }
    }
}

fn contribution_envelope(
    contribution: &crate::CompiledAudioContribution,
    clip_local: TimelineTime,
    sequence_time: TimelineTime,
    transitions: &[crate::plan::CompiledTransition],
) -> Result<f32, AudioExecutionError> {
    let mut gain = 1.0_f32;
    if let Some((duration, curve)) = contribution.fade_in {
        if duration > TimelineTime::ZERO && clip_local < duration {
            gain *= rising_curve(clip_local.to_f64() / duration.to_f64(), curve);
        }
    }
    if let Some((duration, curve)) = contribution.fade_out {
        if duration > TimelineTime::ZERO {
            let remaining = contribution.sequence_range.duration.checked_sub(clip_local)?;
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
        if transition.left == contribution.edit_id {
            gain *= transition_falling(progress, transition.curve);
        } else if transition.right == contribution.edit_id {
            gain *= transition_rising(progress, transition.curve);
        }
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
