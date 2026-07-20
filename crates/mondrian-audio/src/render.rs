use crate::delay::FixedDelayLine;
use crate::dsp;
use crate::processor_execution::PreparedProcessorRuntime;
use crate::schedule::{
    PreparedAudioPlan, PreparedAudioSchedule, PreparedAutomationCurve, PreparedContribution,
    PreparedNode, PreparedNodeOrigin, PreparedTransitionBinding, PreparedTransitionDirection,
};
use mondrian_core::{
    AudioChannelLayout, AudioChannelPosition, AudioComponentEditId, AudioSampleRate, TimelineTime,
    TimelineTimeError,
};
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
        source_layout: AudioChannelLayout,
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

/// Consumer-owned continuity identity. A discontinuity must use a new value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioContinuityEpoch(u64);

impl AudioContinuityEpoch {
    /// Construct a consumer-scoped monotonic continuity identity.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the opaque numeric identity for diagnostics.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Explicit cold/seek/recovery entry selected by the owning coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioStateEntry {
    /// Fresh continuity identity; an existing epoch cannot be reset in place.
    pub epoch: AudioContinuityEpoch,
    /// First Sequence-domain sample that the next block must evaluate.
    pub start_sample: i64,
}

/// Immutable allocation envelope established before realtime rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioRenderCapacity {
    /// Maximum sample frames accepted by one call.
    pub max_block_frames: usize,
    /// Interleaved channel count.
    pub channels: usize,
    /// Liveness-reused graph scratch slots.
    pub node_scratch_slots: usize,
    /// Preallocated interleaved samples in every node buffer.
    pub samples_per_node_slot: usize,
    /// Largest native source channel count prepared for one Contribution.
    pub maximum_source_channels: usize,
    /// Reused native-source sample storage retained by the Session.
    pub source_scratch_samples: usize,
    /// Preallocated interleaved gain lane shared by Route automation.
    pub route_gain_scratch_samples: usize,
    /// Non-zero Contribution and Route compensation lines.
    pub compensation_delay_line_count: usize,
    /// Interleaved sample storage retained by all compensation lines.
    pub compensation_delay_samples: usize,
    /// Generated processor occurrences with independent Session state.
    pub processor_occurrences: usize,
    /// Largest parameter-lane count for one processor occurrence.
    pub maximum_processor_parameter_lanes: usize,
    /// Reused sample-accurate event storage for one processor block.
    pub parameter_event_capacity: usize,
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
    source_pcm: Vec<f32>,
    contribution_pcm: Vec<f32>,
    contribution_processed: Vec<f32>,
    contribution_sample_gains: Vec<f32>,
    strip_pre_gains: Vec<f32>,
    strip_post_gains: Vec<f32>,
    route_gains: Vec<f32>,
    frame_gain_db: Vec<f64>,
    frame_pan: Vec<f64>,
}

impl RenderScratch {
    fn new(max_frames: usize, samples: usize, source_samples: usize) -> Self {
        Self {
            source_frames: vec![-1; max_frames],
            source_pcm: vec![0.0; source_samples],
            contribution_pcm: vec![0.0; samples],
            contribution_processed: vec![0.0; samples],
            contribution_sample_gains: vec![0.0; samples],
            strip_pre_gains: vec![0.0; samples],
            strip_post_gains: vec![0.0; samples],
            route_gains: vec![0.0; samples],
            frame_gain_db: vec![0.0; max_frames],
            frame_pan: vec![0.0; max_frames],
        }
    }
}

/// Exclusive mutable execution state for one Playback, Export, or Audition run.
pub struct AudioRenderSession {
    plan: Arc<PreparedAudioPlan>,
    node_buffers: Vec<NodeBuffers>,
    scratch: RenderScratch,
    contribution_delay_lines: Vec<FixedDelayLine>,
    route_delay_lines: Vec<FixedDelayLine>,
    processor_runtime: PreparedProcessorRuntime,
    continuity: SessionContinuity,
    capacity: AudioRenderCapacity,
}

#[derive(Debug, Clone, Copy)]
enum SessionContinuity {
    Unentered,
    Active {
        epoch: AudioContinuityEpoch,
        next_sample: i64,
    },
    Poisoned {
        epoch: AudioContinuityEpoch,
    },
}

impl AudioRenderSession {
    /// Allocate all scratch storage before realtime execution begins.
    pub fn new(plan: Arc<PreparedAudioPlan>) -> Result<Self, AudioExecutionError> {
        let contract = plan.contract();
        let samples = contract
            .max_block_frames
            .checked_mul(contract.channel_count())
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        let source_samples = contract
            .max_block_frames
            .checked_mul(plan.schedule.summary.maximum_source_channels)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        let node_buffers = (0..plan.schedule.summary.scratch_slot_count)
            .map(|_| NodeBuffers::new(samples))
            .collect();
        let contribution_delay_lines = plan
            .schedule
            .contributions
            .iter()
            .map(|contribution| {
                FixedDelayLine::new(
                    contribution.compensation_delay_frames,
                    contract.channel_count(),
                )
            })
            .collect::<Result<Vec<_>, AudioExecutionError>>()?;
        let route_delay_lines = plan
            .schedule
            .routes
            .iter()
            .map(|route| {
                FixedDelayLine::new(route.compensation_delay_frames, contract.channel_count())
            })
            .collect::<Result<Vec<_>, AudioExecutionError>>()?;
        let compensation_delay_line_count = contribution_delay_lines
            .iter()
            .chain(route_delay_lines.iter())
            .filter(|line| line.sample_capacity() > 0)
            .count();
        let compensation_delay_samples = contribution_delay_lines
            .iter()
            .chain(route_delay_lines.iter())
            .try_fold(0_usize, |total, line| {
                total
                    .checked_add(line.sample_capacity())
                    .ok_or(AudioExecutionError::BufferTooLarge)
            })?;
        let processor_runtime = PreparedProcessorRuntime::new(&plan.schedule)?;
        let capacity = AudioRenderCapacity {
            max_block_frames: contract.max_block_frames,
            channels: contract.channel_count(),
            node_scratch_slots: plan.schedule.summary.scratch_slot_count,
            samples_per_node_slot: samples,
            maximum_source_channels: plan.schedule.summary.maximum_source_channels,
            source_scratch_samples: source_samples,
            route_gain_scratch_samples: samples,
            compensation_delay_line_count,
            compensation_delay_samples,
            processor_occurrences: processor_runtime.occurrence_count(),
            maximum_processor_parameter_lanes: processor_runtime.maximum_parameter_lanes(),
            parameter_event_capacity: processor_runtime.parameter_event_capacity(),
        };
        Ok(Self {
            plan,
            node_buffers,
            scratch: RenderScratch::new(contract.max_block_frames, samples, source_samples),
            contribution_delay_lines,
            route_delay_lines,
            processor_runtime,
            continuity: SessionContinuity::Unentered,
            capacity,
        })
    }

    /// Total prepared latency of the selected Program Output.
    pub fn output_latency_frames(&self) -> usize {
        self.plan.output_latency_frames()
    }

    /// Whether this Plan owns history and therefore rejects unentered or
    /// discontinuous block execution.
    pub fn requires_state_entry(&self) -> bool {
        self.plan.requires_state_entry()
    }

    #[cfg(test)]
    pub(crate) fn require_state_entry_for_test(&mut self) {
        Arc::make_mut(&mut self.plan).schedule.summary.requires_state_entry = true;
    }

    #[cfg(test)]
    pub(crate) fn processor_parameter_events_for_test(
        &mut self,
        processor_index: usize,
        request: AudioRenderRequest,
    ) -> Result<
        Vec<(mondrian_core::ParameterId, Vec<crate::AudioParameterEvent>)>,
        AudioExecutionError,
    > {
        self.processor_runtime.parameter_events_for_test(
            &self.plan.schedule.processors,
            processor_index,
            request,
            AudioSampleRate::new(self.plan.contract().sample_rate)?,
        )
    }

    /// Reset mutable execution history and enter one fresh continuity epoch.
    pub fn enter_state(&mut self, entry: AudioStateEntry) -> Result<(), AudioExecutionError> {
        let previous_epoch = match self.continuity {
            SessionContinuity::Unentered => None,
            SessionContinuity::Active { epoch, .. } | SessionContinuity::Poisoned { epoch } => {
                Some(epoch)
            }
        };
        if previous_epoch == Some(entry.epoch) {
            return Err(AudioExecutionError::ReusedContinuityEpoch(entry.epoch));
        }
        for delay in self
            .contribution_delay_lines
            .iter_mut()
            .chain(self.route_delay_lines.iter_mut())
        {
            delay.reset();
        }
        self.processor_runtime.enter_state(&self.plan.schedule.processors)?;
        self.continuity = SessionContinuity::Active {
            epoch: entry.epoch,
            next_sample: entry.start_sample,
        };
        Ok(())
    }

    /// Return the fixed allocation envelope owned by this Session.
    pub const fn capacity(&self) -> AudioRenderCapacity {
        self.capacity
    }

    /// Render into caller-owned interleaved float storage without Session-owned
    /// allocation, buffer growth, author-map search, or event-container creation.
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
            .checked_mul(contract.channel_count())
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        if destination.len() != samples {
            return Err(AudioExecutionError::OutputSizeMismatch);
        }
        destination.fill(0.0);

        let next_sample = request
            .start_sample
            .checked_add(
                i64::try_from(request.frames).map_err(|_| AudioExecutionError::BufferTooLarge)?,
            )
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        let active_epoch = if self.requires_state_entry() {
            match self.continuity {
                SessionContinuity::Unentered => {
                    return Err(AudioExecutionError::StateEntryRequired);
                }
                SessionContinuity::Poisoned { epoch } => {
                    return Err(AudioExecutionError::ContinuityPoisoned(epoch));
                }
                SessionContinuity::Active { epoch, next_sample }
                    if next_sample != request.start_sample =>
                {
                    return Err(AudioExecutionError::NonContiguousBlock {
                        epoch,
                        expected_start_sample: next_sample,
                        actual_start_sample: request.start_sample,
                    });
                }
                SessionContinuity::Active { epoch, .. } => Some(epoch),
            }
        } else {
            None
        };
        if let Some(epoch) = active_epoch {
            // Any later `?` leaves the epoch poisoned: mutable processors or
            // delay lines may already have consumed a prefix of this block.
            self.continuity = SessionContinuity::Poisoned { epoch };
        }

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
                    contract.channel_layout,
                    backend,
                    &mut self.node_buffers[scratch_slot],
                    &mut self.scratch,
                    &mut self.contribution_delay_lines,
                    &mut self.processor_runtime,
                )?;
            } else {
                for route_index in node.incoming.clone() {
                    let delay_line = self
                        .route_delay_lines
                        .get_mut(route_index)
                        .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
                    sum_prepared_route(
                        schedule,
                        route_index,
                        request,
                        sample_rate,
                        contract.channel_count(),
                        backend,
                        &mut self.node_buffers,
                        delay_line,
                        &mut self.scratch,
                    )?;
                }
            }

            process_strip(
                schedule,
                node,
                request,
                contract.sample_rate,
                contract.channel_count(),
                backend,
                &mut self.node_buffers[scratch_slot],
                &mut self.scratch,
                &mut self.processor_runtime,
            )?;
        }

        let output_scratch = schedule.nodes[schedule.output_slot].scratch_slot;
        destination.copy_from_slice(&self.node_buffers[output_scratch].post_mute[..samples]);
        if let Some(epoch) = active_epoch {
            self.continuity = SessionContinuity::Active { epoch, next_sample };
        }
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
        .checked_mul(plan.contract().channel_count())
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let mut output = vec![0.0; samples];
    let mut session = AudioRenderSession::new(plan)?;
    if session.requires_state_entry() {
        session.enter_state(AudioStateEntry {
            epoch: AudioContinuityEpoch::new(0),
            start_sample: request.start_sample,
        })?;
    }
    session.render_into(source, request, &mut output)?;
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn render_track_contributions(
    schedule: &PreparedAudioSchedule,
    node_slot: usize,
    source: &mut impl AudioPcmSource,
    request: AudioRenderRequest,
    sample_rate: AudioSampleRate,
    channel_layout: AudioChannelLayout,
    backend: crate::AudioKernelBackend,
    track: &mut NodeBuffers,
    scratch: &mut RenderScratch,
    delay_lines: &mut [FixedDelayLine],
    processor_runtime: &mut PreparedProcessorRuntime,
) -> Result<(), AudioExecutionError> {
    let channels = channel_layout.channel_count();
    let samples = request
        .frames
        .checked_mul(channels)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    for contribution_index in schedule.nodes[node_slot].contributions.clone() {
        let contribution = &schedule.contributions[contribution_index];
        let delay_line = delay_lines
            .get_mut(contribution_index)
            .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
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
            advance_silent_compensation(delay_line, track, scratch, samples)?;
            continue;
        };
        let active_start_sample = request
            .start_sample
            .checked_add(
                i64::try_from(active_frames.start)
                    .map_err(|_| AudioExecutionError::BufferTooLarge)?,
            )
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        let active_frame_count = active_frames.len();
        let active_samples = active_frame_count
            .checked_mul(channels)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        let active_sample_start = active_frames
            .start
            .checked_mul(channels)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        let active_sample_end = active_sample_start
            .checked_add(active_samples)
            .ok_or(AudioExecutionError::BufferTooLarge)?;

        let source_layout = contribution.channel_mixer.source_layout();
        let source_samples = request
            .frames
            .checked_mul(source_layout.channel_count())
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        scratch.source_pcm[..source_samples].fill(0.0);
        source.read_indexed_interleaved(
            contribution.semantic.edit_id,
            &scratch.source_frames[..request.frames],
            source_layout,
            &mut scratch.source_pcm[..source_samples],
        )?;
        contribution.channel_mixer.mix_into(
            backend,
            request.frames,
            &scratch.source_pcm[..source_samples],
            &mut scratch.contribution_pcm[..samples],
        )?;

        let scope_gain_db = &mut scratch.frame_gain_db[..active_frame_count];
        if let Some(gain) = contribution.constant_scope_gain {
            dsp::multiply_constant_into(
                backend,
                &mut scratch.contribution_processed[active_sample_start..active_sample_end],
                &scratch.contribution_pcm[active_sample_start..active_sample_end],
                gain,
            );
        } else {
            if let Some(curve) = &contribution.scope_input_automation {
                fill_prepared_curve(curve, active_start_sample, sample_rate, scope_gain_db)?;
            } else {
                scope_gain_db.fill(scope.input_gain_db);
            }
            dsp::expand_frame_db_to_interleaved_gains(
                scope_gain_db,
                channels,
                &mut scratch.contribution_sample_gains[..active_samples],
            );
            dsp::multiply_into(
                backend,
                &mut scratch.contribution_processed[active_sample_start..active_sample_end],
                &scratch.contribution_pcm[active_sample_start..active_sample_end],
                &scratch.contribution_sample_gains[..active_samples],
            );
        }

        processor_runtime.process_rack(
            &contribution.scope_rack,
            &schedule.processors,
            AudioRenderRequest {
                start_sample: active_start_sample,
                frames: active_frame_count,
            },
            sample_rate,
            channels,
            backend,
            &mut scratch.contribution_processed[active_sample_start..active_sample_end],
            &mut scratch.frame_gain_db[..active_frame_count],
            &mut scratch.contribution_sample_gains[..active_samples],
        )?;

        scratch.contribution_sample_gains[..active_samples].fill(0.0);
        if let Some((gain, pan)) = contribution.constant_edit_gain_pan {
            for frame in 0..active_frame_count {
                for channel in 0..channels {
                    scratch.contribution_sample_gains[frame * channels + channel] =
                        gain * stereo_balance_gain(channel_layout, channel, pan);
                }
            }
        } else {
            let gain_db = &mut scratch.frame_gain_db[..active_frame_count];
            let pan_values = &mut scratch.frame_pan[..active_frame_count];
            if let Some(curve) = &contribution.volume_automation {
                fill_prepared_curve(curve, active_start_sample, sample_rate, gain_db)?;
            } else {
                gain_db.fill(contribution.semantic.volume_db);
            }
            if let Some(curve) = &contribution.pan_automation {
                fill_prepared_curve(curve, active_start_sample, sample_rate, pan_values)?;
            } else {
                pan_values.fill(contribution.semantic.pan);
            }
            for (local_index, frame) in active_frames.clone().enumerate() {
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
                let pan = pan_values[local_index].clamp(-1.0, 1.0);
                let envelope = contribution_envelope(
                    contribution,
                    clip_local,
                    sequence_time,
                    &schedule.transitions[contribution.transitions.clone()],
                )?;
                let gain = dsp::db_to_linear(gain_db[local_index]) * envelope;
                for channel in 0..channels {
                    scratch.contribution_sample_gains[local_index * channels + channel] =
                        gain * stereo_balance_gain(channel_layout, channel, pan);
                }
            }
        }
        dsp::multiply_into(
            backend,
            &mut scratch.contribution_pcm[active_sample_start..active_sample_end],
            &scratch.contribution_processed[active_sample_start..active_sample_end],
            &scratch.contribution_sample_gains[..active_samples],
        );
        if delay_line.sample_capacity() == 0 {
            dsp::add(
                backend,
                &mut track.input[..samples],
                &scratch.contribution_pcm[..samples],
            );
        } else {
            delay_line.add_interleaved(
                &scratch.contribution_pcm[..samples],
                &mut track.input[..samples],
            )?;
        }
    }
    Ok(())
}

fn advance_silent_compensation(
    delay_line: &mut FixedDelayLine,
    track: &mut NodeBuffers,
    scratch: &mut RenderScratch,
    samples: usize,
) -> Result<(), AudioExecutionError> {
    if delay_line.sample_capacity() == 0 {
        return Ok(());
    }
    scratch.contribution_processed[..samples].fill(0.0);
    delay_line.add_interleaved(
        &scratch.contribution_processed[..samples],
        &mut track.input[..samples],
    )
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
    request: AudioRenderRequest,
    sample_rate: AudioSampleRate,
    channels: usize,
    backend: crate::AudioKernelBackend,
    buffers: &mut [NodeBuffers],
    delay_line: &mut FixedDelayLine,
    scratch: &mut RenderScratch,
) -> Result<(), AudioExecutionError> {
    let route = schedule
        .routes
        .get(route_index)
        .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
    let samples = request
        .frames
        .checked_mul(channels)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let automated_gains = if let Some(curve) = &route.gain_automation {
        let frame_db = &mut scratch.frame_gain_db[..request.frames];
        fill_prepared_curve(curve, request.start_sample, sample_rate, frame_db)?;
        let gains = &mut scratch.route_gains[..samples];
        dsp::expand_frame_db_to_interleaved_gains(frame_db, channels, gains);
        Some(&gains[..])
    } else {
        None
    };
    let source_scratch = schedule.nodes[route.source_slot].scratch_slot;
    let destination_scratch = schedule.nodes[route.destination_slot].scratch_slot;
    if source_scratch == destination_scratch {
        return Err(AudioExecutionError::InvalidPreparedSchedule);
    }
    if source_scratch < destination_scratch {
        let (left, right) = buffers.split_at_mut(destination_scratch);
        let source = left[source_scratch].port(route.source_port, samples);
        sum_route_signal(
            route,
            source,
            &mut right[0].input[..samples],
            automated_gains,
            backend,
            delay_line,
        )?;
    } else {
        let (left, right) = buffers.split_at_mut(source_scratch);
        let destination = &mut left[destination_scratch].input[..samples];
        let source = right[0].port(route.source_port, samples);
        sum_route_signal(
            route,
            source,
            destination,
            automated_gains,
            backend,
            delay_line,
        )?;
    }
    Ok(())
}

fn sum_route_signal(
    route: &crate::schedule::PreparedRoute,
    source: &[f32],
    destination: &mut [f32],
    automated_gains: Option<&[f32]>,
    backend: crate::AudioKernelBackend,
    delay_line: &mut FixedDelayLine,
) -> Result<(), AudioExecutionError> {
    if let Some(gains) = automated_gains {
        if delay_line.sample_capacity() == 0 {
            dsp::multiply_add(backend, destination, source, gains);
        } else {
            delay_line.add_interleaved_with_gains(source, destination, gains)?;
        }
        return Ok(());
    }
    let gain = route.constant_gain.ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
    if delay_line.sample_capacity() == 0 {
        if gain == 1.0 {
            dsp::add(backend, destination, source);
        } else {
            dsp::multiply_add_constant(backend, destination, source, gain);
        }
    } else if gain == 1.0 {
        delay_line.add_interleaved(source, destination)?;
    } else {
        delay_line.add_interleaved_constant(source, destination, gain)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_strip(
    schedule: &PreparedAudioSchedule,
    node: &PreparedNode,
    request: AudioRenderRequest,
    sample_rate: u32,
    channels: usize,
    backend: crate::AudioKernelBackend,
    buffers: &mut NodeBuffers,
    scratch: &mut RenderScratch,
    processor_runtime: &mut PreparedProcessorRuntime,
) -> Result<(), AudioExecutionError> {
    let samples = request
        .frames
        .checked_mul(channels)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    dsp::multiply_constant_into(
        backend,
        &mut buffers.pre_fader[..samples],
        &buffers.input[..samples],
        dsp::db_to_linear(node.strip.input_trim_db),
    );
    let sample_rate = AudioSampleRate::new(sample_rate)?;
    processor_runtime.process_rack(
        &node.pre_rack,
        &schedule.processors,
        request,
        sample_rate,
        channels,
        backend,
        &mut buffers.pre_fader[..samples],
        &mut scratch.frame_gain_db[..request.frames],
        &mut scratch.strip_pre_gains[..samples],
    )?;

    if let Some(curve) = &node.fader_automation {
        let frame_db = &mut scratch.frame_gain_db[..request.frames];
        fill_prepared_curve(curve, request.start_sample, sample_rate, frame_db)?;
        dsp::expand_frame_db_to_interleaved_gains(
            frame_db,
            channels,
            &mut scratch.strip_post_gains[..samples],
        );
        dsp::multiply_into(
            backend,
            &mut buffers.post_fader_pre_mute[..samples],
            &buffers.pre_fader[..samples],
            &scratch.strip_post_gains[..samples],
        );
    } else {
        dsp::multiply_constant_into(
            backend,
            &mut buffers.post_fader_pre_mute[..samples],
            &buffers.pre_fader[..samples],
            dsp::db_to_linear(node.strip.fader_db),
        );
    }
    processor_runtime.process_rack(
        &node.post_rack,
        &schedule.processors,
        request,
        sample_rate,
        channels,
        backend,
        &mut buffers.post_fader_pre_mute[..samples],
        &mut scratch.frame_gain_db[..request.frames],
        &mut scratch.strip_post_gains[..samples],
    )?;
    if node.muted {
        buffers.post_mute[..samples].fill(0.0);
    } else {
        buffers.post_mute[..samples].copy_from_slice(&buffers.post_fader_pre_mute[..samples]);
    }
    Ok(())
}

fn fill_prepared_curve(
    curve: &PreparedAutomationCurve,
    start_sample: i64,
    sample_rate: AudioSampleRate,
    destination: &mut [f64],
) -> Result<(), AudioExecutionError> {
    let mut cursor = curve.initial_cursor(start_sample);
    for (offset, value) in destination.iter_mut().enumerate() {
        let sample = start_sample
            .checked_add(i64::try_from(offset).map_err(|_| AudioExecutionError::BufferTooLarge)?)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        *value = curve.evaluate_sample(sample, sample_rate, &mut cursor)?;
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

fn stereo_balance_gain(layout: AudioChannelLayout, channel: usize, pan: f64) -> f32 {
    match layout.channel_position(channel) {
        Some(AudioChannelPosition::FrontLeft) if pan > 0.0 => {
            (pan * std::f64::consts::FRAC_PI_2).cos() as f32
        }
        Some(AudioChannelPosition::FrontRight) if pan < 0.0 => {
            (-pan * std::f64::consts::FRAC_PI_2).cos() as f32
        }
        _ => 1.0,
    }
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
    /// Source storage did not match the prepared channel-mix source layout.
    #[error("audio channel-mix source size does not match its prepared layout")]
    ChannelMixSourceSizeMismatch,
    /// Destination storage did not match the prepared channel-mix destination layout.
    #[error("audio channel-mix destination size does not match its prepared layout")]
    ChannelMixDestinationSizeMismatch,
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
    /// A stateful Plan was executed before its coordinator selected an entry.
    #[error("audio Session requires an explicit state entry")]
    StateEntryRequired,
    /// Resetting one epoch in place would erase the meaning of continuity evidence.
    #[error("audio continuity epoch {0:?} was reused for a state reset")]
    ReusedContinuityEpoch(AudioContinuityEpoch),
    /// A stateful block did not exactly continue the current Evaluation Grid position.
    #[error(
        "audio continuity epoch {epoch:?} expected sample {expected_start_sample}, got {actual_start_sample}"
    )]
    NonContiguousBlock {
        epoch: AudioContinuityEpoch,
        expected_start_sample: i64,
        actual_start_sample: i64,
    },
    /// An execution failure may have partially advanced mutable state.
    #[error("audio continuity epoch {0:?} is poisoned and requires a fresh state entry")]
    ContinuityPoisoned(AudioContinuityEpoch),
    /// A nested instance exhausted its private monotonic state-entry identity space.
    #[error("audio nested continuity epoch space is exhausted")]
    ContinuityEpochExhausted,
    /// Generic processor state cannot be evaluated backwards without a proven
    /// checkpoint, materialization, or processor-specific reverse capability.
    #[error("nested contribution {0} requires unsupported reverse state evaluation")]
    UnsupportedNestedStateDirection(AudioComponentEditId),
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
