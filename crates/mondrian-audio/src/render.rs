use crate::delay::FixedDelayLine;
use crate::dsp;
use crate::meter::AudioMeterBank;
use crate::processor_host::PreparedProcessorHost;
use crate::schedule::{
    PreparedAudioPlan, PreparedAudioSchedule, PreparedAutomationCurve, PreparedContribution,
    PreparedNode, PreparedNodeOrigin, PreparedTransitionBinding, PreparedTransitionDirection,
};
use crate::AudioRenderContract;
use mondrian_core::{
    AudioChannelLayout, AudioChannelPosition, AudioComponentEditId, AudioSampleRate,
    SourceSamplingBoundary, TimelineTime, TimelineTimeError,
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
    /// Internal priming frames required before Timeline-aligned public PCM.
    pub public_output_lookahead_frames: usize,
    /// Processor-private Session scratch declared by all realized factories.
    pub processor_session_scratch_bytes: usize,
    /// Fixed per-channel state across every prepared meter target.
    pub meter_channel_state_count: usize,
    /// Prepared Track, Bus, and Program Output meter targets.
    pub meter_target_count: usize,
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
    processor_host: PreparedProcessorHost,
    meter_bank: AudioMeterBank,
    meter_block_serial: u64,
    continuity: SessionContinuity,
    capacity: AudioRenderCapacity,
}

#[derive(Debug, Clone, Copy)]
enum SessionContinuity {
    Unentered,
    Active {
        epoch: AudioContinuityEpoch,
        next_public_sample: i64,
        next_execution_sample: i64,
        alignment_primed: bool,
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
        if compensation_delay_samples != plan.schedule.summary.compensation_delay_samples
            || plan.public_output_lookahead_frames()
                > contract.public_output_lookahead_budget_frames
            || compensation_delay_samples
                .checked_mul(std::mem::size_of::<f32>())
                .is_none_or(|bytes| bytes > contract.compensation_delay_scratch_budget_bytes)
        {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        let processor_host = PreparedProcessorHost::new(&plan.schedule, contract)?;
        let meter_bank = AudioMeterBank::new(
            contract.channel_layout,
            plan.schedule.nodes.iter().map(|node| match node.origin {
                PreparedNodeOrigin::Track(track_id) => crate::AudioMeterTarget::Track(track_id),
                PreparedNodeOrigin::Bus(bus_id) => crate::AudioMeterTarget::Bus(bus_id),
                PreparedNodeOrigin::Output(output_id) => {
                    crate::AudioMeterTarget::ProgramOutput(output_id)
                }
            }),
        );
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
            processor_occurrences: processor_host.occurrence_count(),
            maximum_processor_parameter_lanes: processor_host.maximum_parameter_lanes(),
            parameter_event_capacity: processor_host.parameter_event_capacity(),
            public_output_lookahead_frames: plan.public_output_lookahead_frames(),
            processor_session_scratch_bytes: processor_host.session_scratch_bytes(),
            meter_channel_state_count: meter_bank.channel_state_count(),
            meter_target_count: meter_bank.target_count(),
        };
        Ok(Self {
            plan,
            node_buffers,
            scratch: RenderScratch::new(contract.max_block_frames, samples, source_samples),
            contribution_delay_lines,
            route_delay_lines,
            processor_host,
            meter_bank,
            meter_block_serial: 0,
            continuity: SessionContinuity::Unentered,
            capacity,
        })
    }

    /// Internal lookahead needed to return Timeline-aligned public PCM.
    pub fn public_output_lookahead_frames(&self) -> usize {
        self.plan.public_output_lookahead_frames()
    }

    /// Exact immutable Render Contract that admitted this Session.
    pub fn contract(&self) -> AudioRenderContract {
        self.plan.contract()
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
        self.processor_host.parameter_events_for_test(
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
        // State entry can reset several delay/processor instances before one
        // later instance fails. Consume and poison the new epoch first so a
        // partially reset Session can never resume either old or new history.
        self.continuity = SessionContinuity::Poisoned { epoch: entry.epoch };
        for delay in self
            .contribution_delay_lines
            .iter_mut()
            .chain(self.route_delay_lines.iter_mut())
        {
            delay.reset();
        }
        self.processor_host.enter_state(&self.plan.schedule.processors, entry)?;
        self.continuity = SessionContinuity::Active {
            epoch: entry.epoch,
            next_public_sample: entry.start_sample,
            next_execution_sample: entry.start_sample,
            alignment_primed: self.public_output_lookahead_frames() == 0,
        };
        Ok(())
    }

    /// Return the fixed allocation envelope owned by this Session.
    pub const fn capacity(&self) -> AudioRenderCapacity {
        self.capacity
    }

    /// Clone the latest successfully completed Track/Bus/Output meter block.
    ///
    /// Cloning allocates and is therefore an observation/control-thread API,
    /// not part of `render_into`'s realtime contract.
    pub fn latest_meter_frame(&self) -> crate::AudioMeterFrame {
        self.meter_bank.snapshot()
    }

    /// Obtain a lock-free observation handle for another thread.
    ///
    /// Clone this before moving the Session into an audio callback. Snapshot
    /// allocation occurs only when the observer is read, never while publishing.
    pub fn meter_observer(&self) -> crate::AudioMeterObserver {
        self.meter_bank.observer()
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

        let next_public_sample = checked_advance_sample(request.start_sample, request.frames)?;
        let active = if self.requires_state_entry() {
            match self.continuity {
                SessionContinuity::Unentered => {
                    return Err(AudioExecutionError::StateEntryRequired);
                }
                SessionContinuity::Poisoned { epoch } => {
                    return Err(AudioExecutionError::ContinuityPoisoned(epoch));
                }
                SessionContinuity::Active { epoch, next_public_sample, .. }
                    if next_public_sample != request.start_sample =>
                {
                    return Err(AudioExecutionError::NonContiguousBlock {
                        epoch,
                        expected_start_sample: next_public_sample,
                        actual_start_sample: request.start_sample,
                    });
                }
                SessionContinuity::Active {
                    epoch,
                    next_execution_sample,
                    alignment_primed,
                    ..
                } => Some((epoch, next_execution_sample, alignment_primed)),
            }
        } else {
            if self.public_output_lookahead_frames() != 0 {
                return Err(AudioExecutionError::InvalidPreparedSchedule);
            }
            None
        };
        if let Some((epoch, _, _)) = active {
            // Any later `?` leaves the epoch poisoned: mutable processors or
            // delay lines may already have consumed a prefix of this block.
            self.continuity = SessionContinuity::Poisoned { epoch };
        }

        let mut execution_start_sample = active
            .map_or(request.start_sample, |(_, next_execution_sample, _)| {
                next_execution_sample
            });
        let mut alignment_primed = active.is_none_or(|(_, _, primed)| primed);
        if !alignment_primed && request.frames > 0 {
            let mut remaining = self.public_output_lookahead_frames();
            while remaining > 0 {
                let frames = remaining.min(contract.max_block_frames);
                self.render_internal(
                    source,
                    AudioRenderRequest { start_sample: execution_start_sample, frames },
                    None,
                )?;
                execution_start_sample = checked_advance_sample(execution_start_sample, frames)?;
                remaining -= frames;
            }
            alignment_primed = true;
        }
        let meter_block_serial = self
            .meter_block_serial
            .checked_add(1)
            .ok_or(AudioExecutionError::MeterSerialExhausted)?;
        self.meter_bank
            .begin_block(meter_block_serial, request.start_sample, request.frames);
        self.render_internal(
            source,
            AudioRenderRequest {
                start_sample: execution_start_sample,
                frames: request.frames,
            },
            Some(request.start_sample),
        )?;
        let output_scratch = self
            .plan
            .schedule
            .nodes
            .get(self.plan.schedule.output_slot)
            .ok_or(AudioExecutionError::InvalidPreparedSchedule)?
            .scratch_slot;
        destination.copy_from_slice(&self.node_buffers[output_scratch].post_mute[..samples]);
        if !self.meter_bank.publish_completed_block() {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        self.meter_block_serial = meter_block_serial;
        if let Some((epoch, _, _)) = active {
            self.continuity = SessionContinuity::Active {
                epoch,
                next_public_sample,
                next_execution_sample: checked_advance_sample(
                    execution_start_sample,
                    request.frames,
                )?,
                alignment_primed,
            };
        }
        Ok(())
    }

    fn render_internal(
        &mut self,
        source: &mut impl AudioPcmSource,
        request: AudioRenderRequest,
        meter_start_sample: Option<i64>,
    ) -> Result<(), AudioExecutionError> {
        let contract = self.plan.contract();
        if request.frames > contract.max_block_frames {
            return Err(AudioExecutionError::BlockTooLarge);
        }
        let samples = request
            .frames
            .checked_mul(contract.channel_count())
            .ok_or(AudioExecutionError::BufferTooLarge)?;
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
                    &mut self.processor_host,
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
                contract.channel_layout,
                backend,
                &mut self.node_buffers[scratch_slot],
                &mut self.scratch,
                &mut self.processor_host,
            )?;
            if meter_start_sample.is_some()
                && !self.meter_bank.measure_target(
                    node_slot,
                    &self.node_buffers[scratch_slot].post_mute[..samples],
                )
            {
                return Err(AudioExecutionError::InvalidPreparedSchedule);
            }
        }
        Ok(())
    }
}

fn checked_advance_sample(start_sample: i64, frames: usize) -> Result<i64, AudioExecutionError> {
    start_sample
        .checked_add(i64::try_from(frames).map_err(|_| AudioExecutionError::BufferTooLarge)?)
        .ok_or(AudioExecutionError::BufferTooLarge)
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
    processor_host: &mut PreparedProcessorHost,
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
        scratch.contribution_pcm[..samples].fill(0.0);
        scratch.contribution_processed[..samples].fill(0.0);
        scratch.contribution_sample_gains[..samples].fill(0.0);
        let Some(execution_frames) = contribution_execution_frames(contribution, request)? else {
            advance_silent_compensation(delay_line, track, scratch, samples)?;
            continue;
        };
        let execution_start_sample = request
            .start_sample
            .checked_add(
                i64::try_from(execution_frames.start)
                    .map_err(|_| AudioExecutionError::BufferTooLarge)?,
            )
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        let execution_frame_count = execution_frames.len();
        let execution_samples = execution_frame_count
            .checked_mul(channels)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        let execution_sample_start = execution_frames
            .start
            .checked_mul(channels)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        let execution_sample_end = execution_sample_start
            .checked_add(execution_samples)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        let scope_signal_start_sample = subtract_signal_delay(
            execution_start_sample,
            contribution.source_algorithmic_latency_frames,
        )?;
        let edit_signal_delay_frames = contribution
            .source_algorithmic_latency_frames
            .checked_add(contribution.scope_rack.algorithmic_latency_frames)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        let edit_signal_start_sample =
            subtract_signal_delay(execution_start_sample, edit_signal_delay_frames)?;

        if prepare_source_frames(
            contribution,
            request,
            sample_rate,
            &mut scratch.source_frames[..request.frames],
        )?
        .is_some()
        {
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
        }

        let scope_gain_db = &mut scratch.frame_gain_db[..execution_frame_count];
        if let Some(gain) = contribution.constant_scope_gain {
            dsp::multiply_constant_into(
                backend,
                &mut scratch.contribution_processed[execution_sample_start..execution_sample_end],
                &scratch.contribution_pcm[execution_sample_start..execution_sample_end],
                gain,
            );
        } else {
            if let Some(curve) = &contribution.scope_input_automation {
                fill_prepared_curve(curve, scope_signal_start_sample, sample_rate, scope_gain_db)?;
            } else {
                scope_gain_db.fill(scope.input_gain_db);
            }
            dsp::expand_frame_db_to_interleaved_gains(
                scope_gain_db,
                channels,
                &mut scratch.contribution_sample_gains[..execution_samples],
            );
            dsp::multiply_into(
                backend,
                &mut scratch.contribution_processed[execution_sample_start..execution_sample_end],
                &scratch.contribution_pcm[execution_sample_start..execution_sample_end],
                &scratch.contribution_sample_gains[..execution_samples],
            );
        }

        processor_host.process_rack(
            &contribution.scope_rack,
            &schedule.processors,
            AudioRenderRequest {
                start_sample: execution_start_sample,
                frames: execution_frame_count,
            },
            sample_rate,
            channel_layout,
            backend,
            &mut scratch.contribution_processed[execution_sample_start..execution_sample_end],
        )?;

        scratch.contribution_sample_gains[..execution_samples].fill(0.0);
        if let Some((gain, pan)) = contribution.constant_edit_gain_pan {
            for frame in 0..execution_frame_count {
                for channel in 0..channels {
                    scratch.contribution_sample_gains[frame * channels + channel] =
                        gain * stereo_balance_gain(channel_layout, channel, pan);
                }
            }
        } else {
            let gain_db = &mut scratch.frame_gain_db[..execution_frame_count];
            let pan_values = &mut scratch.frame_pan[..execution_frame_count];
            if let Some(curve) = &contribution.volume_automation {
                fill_prepared_curve(curve, edit_signal_start_sample, sample_rate, gain_db)?;
            } else {
                gain_db.fill(contribution.semantic.volume_db);
            }
            if let Some(curve) = &contribution.pan_automation {
                fill_prepared_curve(curve, edit_signal_start_sample, sample_rate, pan_values)?;
            } else {
                pan_values.fill(contribution.semantic.pan);
            }
            for local_index in 0..execution_frame_count {
                let signal_sample = edit_signal_start_sample
                    .checked_add(
                        i64::try_from(local_index)
                            .map_err(|_| AudioExecutionError::BufferTooLarge)?,
                    )
                    .ok_or(AudioExecutionError::BufferTooLarge)?;
                let sequence_time = TimelineTime::new(signal_sample, i64::from(sample_rate.hz()))?;
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
            &mut scratch.contribution_pcm[execution_sample_start..execution_sample_end],
            &scratch.contribution_processed[execution_sample_start..execution_sample_end],
            &scratch.contribution_sample_gains[..execution_samples],
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

fn contribution_execution_frames(
    contribution: &PreparedContribution,
    request: AudioRenderRequest,
) -> Result<Option<std::ops::Range<usize>>, AudioExecutionError> {
    let request_frames =
        i64::try_from(request.frames).map_err(|_| AudioExecutionError::BufferTooLarge)?;
    let request_end = request
        .start_sample
        .checked_add(request_frames)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let execution_start = request.start_sample.max(contribution.sequence_start_sample);
    let execution_end = contribution
        .execution_end_sample
        .map_or(request_end, |end| request_end.min(end));
    if execution_start >= execution_end {
        return Ok(None);
    }
    let first_index = usize::try_from(execution_start - request.start_sample)
        .map_err(|_| AudioExecutionError::BufferTooLarge)?;
    let execution_len = usize::try_from(execution_end - execution_start)
        .map_err(|_| AudioExecutionError::BufferTooLarge)?;
    let end_index = first_index
        .checked_add(execution_len)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    Ok(Some(first_index..end_index))
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
    let scale = contribution.semantic.source_time_map.scale;
    let boundary = contribution.semantic.source_time_map.sampling_boundary;
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
        let indexed_numerator = match boundary {
            SourceSamplingBoundary::Covering => numerator,
            SourceSamplingBoundary::StrictPredecessor => {
                numerator.checked_sub(1).ok_or(AudioExecutionError::BufferTooLarge)?
            }
        };
        destination[frame] = i64::try_from(indexed_numerator.div_euclid(denominator))
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
        let destination_input_delay = schedule
            .nodes
            .get(route.destination_slot)
            .ok_or(AudioExecutionError::InvalidPreparedSchedule)?
            .latency
            .input_frames;
        let destination_signal_start =
            subtract_signal_delay(request.start_sample, destination_input_delay)?;
        fill_prepared_curve(curve, destination_signal_start, sample_rate, frame_db)?;
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
    channel_layout: AudioChannelLayout,
    backend: crate::AudioKernelBackend,
    buffers: &mut NodeBuffers,
    scratch: &mut RenderScratch,
    processor_host: &mut PreparedProcessorHost,
) -> Result<(), AudioExecutionError> {
    let channels = channel_layout.channel_count();
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
    processor_host.process_rack(
        &node.pre_rack,
        &schedule.processors,
        request,
        sample_rate,
        channel_layout,
        backend,
        &mut buffers.pre_fader[..samples],
    )?;

    if let Some(curve) = &node.fader_automation {
        let frame_db = &mut scratch.frame_gain_db[..request.frames];
        let fader_signal_start =
            subtract_signal_delay(request.start_sample, node.latency.pre_fader_frames)?;
        fill_prepared_curve(curve, fader_signal_start, sample_rate, frame_db)?;
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
    processor_host.process_rack(
        &node.post_rack,
        &schedule.processors,
        request,
        sample_rate,
        channel_layout,
        backend,
        &mut buffers.post_fader_pre_mute[..samples],
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
                gain *= falling_curve(
                    (remaining.to_f64() / duration.to_f64()).clamp(0.0, 1.0),
                    curve,
                );
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
    /// Successful meter block identity can no longer advance safely.
    #[error("audio meter block serial is exhausted")]
    MeterSerialExhausted,
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
    /// A realized processor failed to instantiate, reset, or execute.
    #[error(transparent)]
    ProcessorHost(#[from] crate::AudioProcessorHostError),
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
