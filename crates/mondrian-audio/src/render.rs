mod state_entry;

use crate::delay::FixedDelayLine;
use crate::dsp;
use crate::meter::AudioMeterBank;
use crate::processor_host::PreparedProcessorHost;
use crate::schedule::{
    PreparedAudioPlan, PreparedAudioSchedule, PreparedAutomationCurve, PreparedContribution,
    PreparedNode, PreparedNodeOrigin, PreparedTransitionBinding, PreparedTransitionDirection,
};
use crate::AudioRenderContract;
use crate::NativeHostAllocationOwner;
use mondrian_core::{
    AudioChannelLayout, AudioChannelPosition, AudioComponentEditId, AudioSampleRate,
    ExecutionCancellationToken, SourceSamplingBoundary, TimelineTime, TimelineTimeError,
};
use mondrian_timeline::audio::{
    AudioChannelSignalStage, AudioChannelStripOutputPort, AudioTransitionCurve,
};
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
    /// Consumer-declared activity. None preserves an explicitly unspecified
    /// transport; it is never inferred from sample coordinates or DSP mode.
    pub transport_activity: Option<mondrian_core::TimelineTransportActivity>,
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
    /// Stable sidechain source ports retained as block-local snapshots.
    pub sidechain_source_buffers: usize,
    /// Preallocated samples across all sidechain source snapshots.
    pub sidechain_source_samples: usize,
    /// Prepared processor auxiliary buses, including explicit silent buses.
    pub processor_auxiliary_buses: usize,
    /// Largest auxiliary-bus set presented to one processor callback.
    pub maximum_processor_auxiliary_buses: usize,
    /// Preallocated PCM samples for the largest processor auxiliary-bus set.
    pub processor_auxiliary_scratch_samples: usize,
    /// Actual output arena capacities, including unrouted additional outputs.
    pub processor_output_buffers: usize,
    /// Actual interleaved sample capacity across processor output buffers.
    pub processor_output_samples: usize,
    /// Enabled per-occurrence sidechain Route bindings.
    pub sidechain_bindings: usize,
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
    session_id: mondrian_core::AudioRenderSessionId,
    pending_reprepare_ticket: Option<crate::AudioProcessorReprepareTicket>,
    plan: Arc<PreparedAudioPlan>,
    // Component currently included in this Session's published policy weight.
    // Initially the Plan model; a parameter swap publishes its checked next
    // owner layouts. This is not physical-memory completeness or a new grant.
    parameter_event_charge_bytes: usize,
    node_buffers: Vec<NodeBuffers>,
    scratch: RenderScratch,
    contribution_delay_lines: Vec<FixedDelayLine>,
    route_delay_lines: Vec<FixedDelayLine>,
    sidechain_delay_lines: Vec<FixedDelayLine>,
    processor_delay_lines: Vec<FixedDelayLine>,
    sidechain_source_buffers: Vec<Vec<f32>>,
    processor_output_buffers: Vec<Vec<f32>>,
    processor_host: PreparedProcessorHost,
    meter_bank: AudioMeterBank,
    meter_block_serial: u64,
    continuity: SessionContinuity,
    capacity: AudioRenderCapacity,
    // Last: actual processor/delivery owners retire before this counted capability.
    native_child_settlement: Option<mondrian_core::NativeChildSettlementRegistrar>,
}

/// Known additional per-Session inline evidence slot, not a whole-heap claim.
pub(crate) fn native_child_settlement_slot_bytes() -> usize {
    std::alloc::Layout::new::<Option<mondrian_core::NativeChildSettlementRegistrar>>().size()
}

/// Private preparation of actual Session buffers, without processor creation.
/// No execution or installed native permission is available from this storage.
/// The normal constructor consumes it through the sole factory-population step;
/// future retained transfers require their own exact whole-root admission.
pub(crate) struct PreparedAudioSessionStorage {
    session: AudioRenderSession,
}

impl PreparedAudioSessionStorage {
    pub(crate) fn current_resource_footprint(
        &self,
    ) -> Result<crate::AudioSessionResourceFootprint, crate::AudioResourceFootprintError> {
        self.session.current_resource_footprint()
    }

    /// The canonical allocation envelope, derived by the same constructor body.
    pub(crate) fn capacity(&self) -> AudioRenderCapacity {
        self.session.capacity
    }

    /// Create actual exclusive processors only after all Session storage checks
    /// completed. A failed population drops this unpublished preparation.
    pub(crate) fn populate(mut self) -> Result<AudioRenderSession, AudioExecutionError> {
        let expected_occurrences = self.capacity().processor_occurrences;
        if let Err(error) = self.session.processor_host.populate_instances(
            &self.session.plan.schedule,
            self.session.native_child_settlement.as_ref(),
        ) {
            self.session.mark_native_child_settlement_unavailable();
            return Err(error);
        }
        if self.session.processor_host.occurrence_count() != expected_occurrences {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        // Evidence failure is sticky, not a new DSP/installation refusal.
        let _ = self.session.bind_current_native_child_settlement();
        Ok(self.session)
    }

    pub(crate) fn validate_native_child_settlement_domain(
        &self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        self.session.validate_native_child_settlement_domain(registrar)
    }

    pub(crate) fn bind_native_child_settlement(
        &mut self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        self.session.attach_native_child_settlement(registrar)?;
        // An unpublished or retired storage destination legitimately has no
        // instances. Its counted capability still covers a prospective transfer.
        if self.session.processor_host.occurrence_count() != 0 {
            self.session.bind_current_native_child_settlement()?;
        }
        Ok(())
    }

    pub(crate) fn attach_native_child_settlement(
        &mut self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        self.session.attach_native_child_settlement(registrar)
    }

    pub(crate) fn inherit_native_child_settlement(&mut self, current: &AudioRenderSession) {
        if let Some(registrar) = &current.native_child_settlement {
            // This counted branch exists before populate/create or Source commit.
            let _ = self.session.attach_native_child_settlement(registrar);
        }
    }
}

/// Prepared immutable owners for one local same-contract parameter handoff.
/// The current Session, grant, installed claim and all native bindings still
/// require independent admission before the sole bounded ownership swap.
pub(crate) struct PreparedNativeAudioParameterPlan {
    current: crate::AudioSessionCursorIdentity,
    next_plan: Arc<PreparedAudioPlan>,
    next_parameter_events: crate::processor_parameters::ProcessorParameterEventScratch,
    next_parameter_event_charge_bytes: usize,
}

impl PreparedNativeAudioParameterPlan {
    pub(crate) fn next_plan(&self) -> &Arc<PreparedAudioPlan> {
        &self.next_plan
    }
    pub(crate) fn resource_footprint(
        &self,
    ) -> Result<crate::AudioSessionResourceFootprint, crate::AudioResourceFootprintError> {
        charged_parameter_footprint(&self.next_plan, self.next_parameter_event_charge_bytes)
    }

    pub(crate) fn resource_buffers(
        &self,
    ) -> (
        &Vec<crate::AudioParameterEvent>,
        &Vec<std::ops::Range<usize>>,
    ) {
        self.next_parameter_events.resource_buffers()
    }
}

/// Project the immutable local weights onto the component actually charged by
/// this exact Session. Logical/other weights and the original grant are unchanged.
fn charged_parameter_footprint(
    plan: &PreparedAudioPlan,
    parameter_charge: usize,
) -> Result<crate::AudioSessionResourceFootprint, crate::AudioResourceFootprintError> {
    let mut footprint = plan.session_resource_footprint()?;
    footprint.fixed_resident_bytes = footprint
        .fixed_resident_bytes
        .checked_sub(footprint.parameter_event_bytes)
        .and_then(|bytes| bytes.checked_add(parameter_charge))
        .ok_or(crate::AudioResourceFootprintError::ExtentOverflow)?;
    footprint.parameter_event_bytes = parameter_charge;
    Ok(footprint)
}

#[derive(Debug, Clone, Copy)]
enum SessionContinuity {
    Unentered,
    Active {
        epoch: AudioContinuityEpoch,
        // None is unbound until the first accepted window; Some(None) binds
        // an explicitly unspecified transport for this continuity epoch.
        transport_activity: Option<Option<mondrian_core::TimelineTransportActivity>>,
        next_public_sample: i64,
        next_execution_sample: i64,
        alignment_primed: bool,
    },
    Poisoned {
        epoch: AudioContinuityEpoch,
    },
}

impl AudioRenderSession {
    pub(crate) fn installed_native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        if matches!(self.continuity, SessionContinuity::Poisoned { .. }) {
            return mondrian_core::NativeChildSettlementRequirement::Unavailable;
        }
        self.processor_host.installed_native_child_settlement_requirement(&self.plan.schedule)
    }

    pub(crate) fn validate_native_child_settlement_domain(
        &self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        if let Some(current) = &self.native_child_settlement
            && !current.same_domain(registrar)
        {
            current.mark_unavailable();
            registrar.mark_unavailable();
            return Err(mondrian_core::NativeChildSettlementError::ForeignDomain);
        }
        Ok(())
    }

    pub(crate) fn attach_native_child_settlement(
        &mut self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        self.validate_native_child_settlement_domain(registrar)?;
        if self.native_child_settlement.is_some() {
            return Ok(());
        }
        self.native_child_settlement = Some(registrar.try_fork()?);
        Ok(())
    }

    fn mark_native_child_settlement_unavailable(&self) {
        if let Some(registrar) = &self.native_child_settlement {
            registrar.mark_unavailable();
        }
    }

    fn bind_current_native_child_settlement(
        &mut self,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        if let Some(registrar) = &self.native_child_settlement {
            self.processor_host
                .bind_native_child_settlement(&self.plan.schedule, registrar)?;
        }
        Ok(())
    }

    pub(crate) fn bind_native_child_settlement(
        &mut self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        self.attach_native_child_settlement(registrar)?;
        self.bind_current_native_child_settlement()
    }

    #[cfg(test)]
    pub(crate) fn is_unentered_for_owned_note_control(&self) -> bool {
        matches!(self.continuity, SessionContinuity::Unentered)
    }

    /// Allocate all scratch storage before realtime execution begins.
    pub fn new(plan: Arc<PreparedAudioPlan>) -> Result<Self, AudioExecutionError> {
        Self::allocate_storage(plan)?.populate()
    }

    /// Allocate actual buffers and validate their canonical PDC/scratch envelope
    /// without creating, entering, replacing or observing a native processor.
    pub(crate) fn allocate_storage(
        plan: Arc<PreparedAudioPlan>,
    ) -> Result<PreparedAudioSessionStorage, AudioExecutionError> {
        let parameter_event_charge_bytes = plan
            .session_resource_footprint()
            .map_err(|_| AudioExecutionError::BufferTooLarge)?
            .parameter_event_bytes;
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
        let sidechain_delay_lines = plan
            .schedule
            .processor_auxiliary_buses
            .iter()
            .flat_map(|bus| bus.bindings.clone().map(move |index| (bus, index)))
            .map(|(bus, index)| {
                let binding = plan
                    .schedule
                    .sidechain_bindings
                    .get(index)
                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
                FixedDelayLine::new(
                    binding.compensation_delay_frames,
                    bus.channel_layout.channel_count(),
                )
            })
            .collect::<Result<Vec<_>, AudioExecutionError>>()?;
        let processor_delay_lines = plan
            .schedule
            .processors
            .iter()
            .map(|processor| {
                FixedDelayLine::new(
                    processor.main_compensation_delay_frames,
                    contract.channel_count(),
                )
            })
            .collect::<Result<Vec<_>, AudioExecutionError>>()?;
        let sidechain_source_buffers = (0..plan.schedule.sidechain_sources.len())
            .map(|_| vec![0.0; samples])
            .collect::<Vec<_>>();
        let processor_output_buffers = (0..plan.schedule.summary.processor_output_buffer_count)
            .map(|_| vec![0.0; samples])
            .collect::<Vec<_>>();
        let processor_output_samples = processor_output_buffers
            .iter()
            .try_fold(0_usize, |sum, pcm| sum.checked_add(pcm.capacity()))
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        if processor_output_buffers.capacity()
            != plan.schedule.summary.processor_output_buffer_count
            || processor_output_samples
                != plan
                    .schedule
                    .summary
                    .processor_output_buffer_count
                    .checked_mul(samples)
                    .ok_or(AudioExecutionError::BufferTooLarge)?
        {
            return Err(AudioExecutionError::InvalidPreparedSchedule);
        }
        let compensation_delay_line_count = contribution_delay_lines
            .iter()
            .chain(route_delay_lines.iter())
            .chain(sidechain_delay_lines.iter())
            .chain(processor_delay_lines.iter())
            .filter(|line| line.sample_capacity() > 0)
            .count();
        let compensation_delay_samples = contribution_delay_lines
            .iter()
            .chain(route_delay_lines.iter())
            .chain(sidechain_delay_lines.iter())
            .chain(processor_delay_lines.iter())
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
        let processor_host = PreparedProcessorHost::allocate_storage(&plan.schedule, contract)?;
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
            sidechain_source_buffers: sidechain_source_buffers.len(),
            sidechain_source_samples: sidechain_source_buffers
                .len()
                .checked_mul(samples)
                .ok_or(AudioExecutionError::BufferTooLarge)?,
            processor_auxiliary_buses: plan.schedule.processor_auxiliary_buses.len(),
            maximum_processor_auxiliary_buses: plan
                .schedule
                .summary
                .maximum_processor_auxiliary_buses,
            processor_auxiliary_scratch_samples: plan
                .schedule
                .summary
                .maximum_processor_auxiliary_channels
                .checked_mul(contract.max_block_frames)
                .ok_or(AudioExecutionError::BufferTooLarge)?,
            processor_output_buffers: processor_output_buffers.len(),
            processor_output_samples,
            sidechain_bindings: plan.schedule.sidechain_bindings.len(),
            compensation_delay_line_count,
            compensation_delay_samples,
            processor_occurrences: plan.schedule.processors.len(),
            maximum_processor_parameter_lanes: processor_host.maximum_parameter_lanes(),
            parameter_event_capacity: processor_host.parameter_event_capacity(),
            public_output_lookahead_frames: plan.public_output_lookahead_frames(),
            processor_session_scratch_bytes: processor_host.session_scratch_bytes(),
            meter_channel_state_count: meter_bank.channel_state_count(),
            meter_target_count: meter_bank.target_count(),
        };
        Ok(PreparedAudioSessionStorage {
            session: Self {
                plan,
                parameter_event_charge_bytes,
                node_buffers,
                scratch: RenderScratch::new(contract.max_block_frames, samples, source_samples),
                contribution_delay_lines,
                route_delay_lines,
                sidechain_delay_lines,
                processor_delay_lines,
                sidechain_source_buffers,
                processor_output_buffers,
                processor_host,
                meter_bank,
                meter_block_serial: 0,
                session_id: mondrian_core::AudioRenderSessionId::new(),
                pending_reprepare_ticket: None,
                continuity: SessionContinuity::Unentered,
                capacity,
                native_child_settlement: None,
            },
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
        self.validate_vst3_note_entry(entry)?;
        self.validate_note_entry_context(entry, false)?;
        self.processor_host.set_native_entry_context(None, self.native_owner_identity());
        self.preflight_native_control(true)?;
        self.enter_state_admitted(entry, None)
    }

    pub(crate) fn validate_state_entry(
        &self,
        entry: AudioStateEntry,
    ) -> Result<(), AudioExecutionError> {
        let previous_epoch = match self.continuity {
            SessionContinuity::Unentered => None,
            SessionContinuity::Active { epoch, .. } | SessionContinuity::Poisoned { epoch } => {
                Some(epoch)
            }
        };
        if previous_epoch == Some(entry.epoch) {
            return Err(AudioExecutionError::ReusedContinuityEpoch(entry.epoch));
        }
        Ok(())
    }

    pub(crate) fn enter_state_admitted(
        &mut self,
        entry: AudioStateEntry,
        cancellation: Option<&ExecutionCancellationToken>,
    ) -> Result<(), AudioExecutionError> {
        let result = (|| {
            self.validate_state_entry(entry)?;
            self.validate_vst3_note_entry(entry)?;
            self.processor_host.validate_note_entry(entry)?;
            // Runtime may already have revoked all tree bindings. Once entry
            // is admitted, any later context/Bind failure consumes this epoch.
            self.continuity = SessionContinuity::Poisoned { epoch: entry.epoch };
            self.processor_host.arm_current_note_reentry(
                self.plan.program(),
                entry,
                cancellation,
            )?;
            // State entry can reset several delay/processor instances before one
            // later instance fails. Consume and poison the new epoch first so a
            // partially reset Session can never resume either old or new history.
            self.processor_host.revoke_native_bindings_for_entry();
            for delay in self
                .contribution_delay_lines
                .iter_mut()
                .chain(self.route_delay_lines.iter_mut())
                .chain(self.sidechain_delay_lines.iter_mut())
                .chain(self.processor_delay_lines.iter_mut())
            {
                delay.reset();
            }
            self.processor_host.enter_state(
                self.plan.program(),
                &self.plan.schedule.processors,
                entry,
                cancellation,
            )?;
            self.processor_host.ensure_note_entry_permits_consumed()?;
            self.continuity = SessionContinuity::Active {
                epoch: entry.epoch,
                transport_activity: None,
                next_public_sample: entry.start_sample,
                next_execution_sample: entry.start_sample,
                alignment_primed: self.public_output_lookahead_frames() == 0,
            };
            Ok(())
        })();
        self.processor_host.clear_note_entry_permits();
        result
    }

    pub(crate) fn poison_note_entry(&mut self, entry: AudioStateEntry) {
        if !self.plan.schedule.notes.is_empty() {
            self.continuity = SessionContinuity::Poisoned { epoch: entry.epoch };
            self.processor_host.clear_note_entry_permits();
        }
    }

    pub(crate) fn validate_vst3_note_entry(
        &self,
        entry: AudioStateEntry,
    ) -> Result<(), AudioExecutionError> {
        if !self.plan.schedule.notes.is_empty()
            && matches!(self.continuity, SessionContinuity::Poisoned { .. })
        {
            return Err(crate::AudioProcessorHostError::PoisonedInstance.into());
        }
        self.processor_host.validate_vst3_note_entry(self.plan.program(), entry)?;
        if !self.plan.schedule.notes.is_empty() {
            self.validate_state_entry(entry)?;
            self.processor_host.validate_note_cold_capacity(&self.plan.schedule, entry)?;
        }
        Ok(())
    }

    pub(crate) fn validate_note_entry_context(
        &self,
        entry: AudioStateEntry,
        installed_root: bool,
    ) -> Result<(), AudioExecutionError> {
        self.processor_host.validate_note_entry_context(entry, installed_root)
    }

    /// Exact existing Plan/Session identity; not native or installation authority.
    pub(crate) fn set_native_entry_context(
        &mut self,
        context: Option<crate::native_runtime_owner::NativeStateEntryBindingContext>,
    ) {
        let local = self.native_owner_identity();
        self.processor_host.set_native_entry_context(context, local);
    }

    pub(crate) fn arm_note_reentry(
        &mut self,
        entry: AudioStateEntry,
        context: Option<&crate::native_runtime_owner::NativeStateEntryBindingContext>,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        let context = context.cloned().map(|context| (context, self.native_owner_identity()));
        let result = self.processor_host.arm_note_reentry(
            self.plan.program(),
            entry,
            context.as_ref(),
            Some(cancellation),
        );
        if result.is_err() {
            self.processor_host.clear_note_entry_permits();
        }
        result
    }

    pub(crate) fn revoke_native_bindings_for_entry(&mut self) {
        self.processor_host.revoke_native_bindings_for_entry();
    }

    pub(crate) fn bind_native_observation_owners(
        &mut self,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<(), crate::AudioNativeOwnerError> {
        self.processor_host.bind_native_observation_owners(cancellation)
    }

    pub(crate) fn native_observation_sources(&self) -> usize {
        self.processor_host.native_observation_sources()
    }

    pub(crate) fn poll_native_observation_at(
        &mut self,
        index: usize,
        root: crate::AudioSessionCursorIdentity,
        sequence: mondrian_core::SequenceId,
        installed_serial: Option<std::num::NonZeroU64>,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<Option<mondrian_core::NativeAudioObservationBatch>, crate::AudioProcessorHostError>
    {
        let local = self.native_owner_identity();
        self.processor_host.poll_native_observation_at(
            index,
            root,
            local,
            sequence,
            installed_serial,
            cancellation,
        )
    }

    pub(crate) fn validate_native_author_source_closed(
        &mut self,
        root: crate::AudioSessionCursorIdentity,
        installed_serial: std::num::NonZeroU64,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<(), crate::NativeAudioAuthorAdmissionError> {
        let local = self.native_owner_identity();
        self.processor_host.validate_native_author_source_closed(
            root,
            local,
            installed_serial,
            cancellation,
        )
    }

    pub(crate) fn prepare_native_author_parameter_plan(
        &self,
        next_program: Arc<crate::CompiledAudioProgram>,
        locator: mondrian_core::NativeAudioEditorLocator,
    ) -> Result<PreparedNativeAudioParameterPlan, crate::NativeAudioParameterHandoffError> {
        let next_plan =
            Arc::new(self.plan.prepare_native_author_parameters(next_program, locator)?);
        Ok(self.prepare_parameter_plan_storage(next_plan)?)
    }

    pub(crate) fn prepare_terminal_author_parameter_plan(
        &self,
        next_program: Arc<crate::CompiledAudioProgram>,
        locators: &[mondrian_core::NativeAudioEditorLocator],
    ) -> Result<PreparedNativeAudioParameterPlan, crate::NativeAudioParameterHandoffError> {
        let next_plan =
            Arc::new(self.plan.prepare_terminal_author_parameters(next_program, locators)?);
        Ok(self.prepare_parameter_plan_storage(next_plan)?)
    }

    fn prepare_parameter_plan_storage(
        &self,
        next_plan: Arc<PreparedAudioPlan>,
    ) -> Result<PreparedNativeAudioParameterPlan, crate::AudioRuntimeBuildError> {
        let maximum_lanes = next_plan
            .schedule
            .processors
            .iter()
            .map(|processor| processor.parameter_ids.len())
            .max()
            .unwrap_or(0);
        let events = next_plan.schedule.summary.maximum_parameter_events_per_block;
        let next_parameter_events =
            crate::processor_parameters::ProcessorParameterEventScratch::new(maximum_lanes, events);
        let (event_owner, lane_owner) = next_parameter_events.resource_buffers();
        let event_bytes = event_owner
            .host_allocation()
            .map_err(crate::AudioRuntimeBuildError::from)?
            .layout()
            .size();
        let lane_bytes = lane_owner
            .host_allocation()
            .map_err(crate::AudioRuntimeBuildError::from)?
            .layout()
            .size();
        let next_parameter_event_charge_bytes = event_bytes.checked_add(lane_bytes).ok_or(
            crate::AudioRuntimeBuildError::ResourceFootprint(
                crate::AudioResourceFootprintError::ExtentOverflow,
            ),
        )?;
        Ok(PreparedNativeAudioParameterPlan {
            current: self.native_owner_identity(),
            next_plan,
            next_parameter_events,
            next_parameter_event_charge_bytes,
        })
    }

    pub(crate) fn prepare_host_output_fader_plan(
        &self,
        next_program: Arc<crate::CompiledAudioProgram>,
    ) -> Result<PreparedNativeAudioParameterPlan, crate::AudioHostFaderHandoffError> {
        let next_plan = Arc::new(self.plan.prepare_host_output_fader(next_program)?);
        Ok(self.prepare_parameter_plan_storage(next_plan)?)
    }

    pub(crate) fn host_fader_frontier(
        &self,
    ) -> Result<crate::AudioSessionCursorEvidence, crate::AudioExecutionError> {
        if self.pending_reprepare_ticket.is_some() {
            return Err(crate::AudioExecutionError::InvalidPreparedSchedule);
        }
        let SessionContinuity::Active {
            epoch, next_public_sample, next_execution_sample, alignment_primed: true, ..
        } = self.continuity else {
            return Err(crate::AudioExecutionError::InvalidPreparedSchedule);
        };
        let sample_rate = AudioSampleRate::new(self.plan.contract().sample_rate)?;
        Ok(crate::AudioSessionCursorEvidence {
            identity: self.native_owner_identity(),
            sample_rate,
            public_output_lookahead_frames: self.public_output_lookahead_frames(),
            state: crate::AudioSessionCursorState::Active {
                epoch,
                next_public_sample: mondrian_core::AudioSamplePosition::new(next_public_sample, sample_rate),
                next_execution_sample: mondrian_core::AudioSamplePosition::new(next_execution_sample, sample_rate),
                alignment_primed: true,
            },
        })
    }

    pub(crate) fn prepare_host_fader_bindings(
        &mut self,
        next: crate::AudioSessionCursorIdentity,
        serial: std::num::NonZeroU64,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<crate::processor_host::host_output_fader::PreparedHostFaderBindings, crate::NativeAudioAuthorAdmissionError> {
        self.processor_host.prepare_host_fader_bindings(
            self.native_owner_identity(), next, serial, self.plan.program(), &self.plan.schedule, cancellation,
        )
    }

    pub(crate) fn validate_host_fader_bindings(
        &mut self,
        bindings: &crate::processor_host::host_output_fader::PreparedHostFaderBindings,
        serial: std::num::NonZeroU64,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<(), crate::NativeAudioAuthorAdmissionError> {
        self.processor_host.validate_host_fader_bindings(
            bindings, self.native_owner_identity(), serial, self.plan.program(), &self.plan.schedule, cancellation,
        )
    }

    pub(crate) fn publish_host_fader_bindings(
        &mut self,
        bindings: &crate::processor_host::host_output_fader::PreparedHostFaderBindings,
        serial: std::num::NonZeroU64,
    ) {
        self.processor_host.publish_host_fader_bindings(bindings, serial);
    }

    pub(crate) fn retain_native_author_proposal(
        &self,
        proposal: &mondrian_core::NativeAudioAuthorProposal,
        root: crate::AudioSessionCursorIdentity,
        serial: std::num::NonZeroU64,
    ) -> Result<(), crate::AudioNativeOwnerError> {
        self.processor_host.retains_native_author_proposal(
            proposal,
            root,
            self.native_owner_identity(),
            serial,
        )
    }

    pub(crate) fn continuing_rescan_cohort(
        &self,
        proposal: &mondrian_core::NativeAudioAuthorProposal,
        root: crate::AudioSessionCursorIdentity,
        serial: std::num::NonZeroU64,
    ) -> Result<Option<crate::NativeRequestCohort>, crate::NativeAudioAuthorAdmissionError> {
        self.processor_host.continuing_rescan_cohort(
            proposal,
            root,
            self.native_owner_identity(),
            serial,
            self.plan.program(),
        )
    }

    pub(crate) fn allocate_parameter_native_bindings(
        &self,
    ) -> crate::processor_host::native_author::PreparedParameterNativeBindings {
        self.processor_host
            .allocate_parameter_native_bindings(self.native_owner_identity())
    }

    pub(crate) fn prepare_parameter_native_bindings(
        &mut self,
        prepared: &mut crate::processor_host::native_author::PreparedParameterNativeBindings,
        context: crate::processor_host::native_author::ParameterNativeBindingContext<'_>,
    ) -> Result<(), crate::NativeAudioAuthorAdmissionError> {
        if context.current_local != self.native_owner_identity() {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
        }
        self.processor_host.prepare_parameter_native_bindings(
            prepared,
            context,
            self.plan.program(),
        )
    }

    pub(crate) fn preflight_terminal_author_sources(
        &mut self,
        context: crate::processor_host::native_author::ParameterNativeBindingContext<'_>,
        proposals: &[mondrian_core::NativeAudioAuthorProposal],
    ) -> Result<(), crate::NativeAudioAuthorAdmissionError> {
        if context.current_local != self.native_owner_identity() {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
        }
        self.processor_host.preflight_terminal_author_sources(
            context,
            proposals,
            self.plan.program(),
        )
    }

    pub(crate) fn prepare_terminal_author_bindings(
        &mut self,
        prepared: &mut crate::processor_host::native_author::PreparedParameterNativeBindings,
        context: crate::processor_host::native_author::ParameterNativeBindingContext<'_>,
        proposals: &[mondrian_core::NativeAudioAuthorProposal],
    ) -> Result<(), crate::NativeAudioAuthorAdmissionError> {
        if context.current_local != self.native_owner_identity() {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
        }
        self.processor_host.prepare_terminal_author_bindings(
            prepared,
            context,
            proposals,
            self.plan.program(),
        )
    }

    pub(crate) fn validate_terminal_author_bindings(
        &mut self,
        prepared: &crate::processor_host::native_author::PreparedParameterNativeBindings,
        context: crate::processor_host::native_author::ParameterNativeBindingContext<'_>,
        proposals: &[mondrian_core::NativeAudioAuthorProposal],
    ) -> Result<(), crate::NativeAudioAuthorAdmissionError> {
        if context.current_local != self.native_owner_identity() {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable.into());
        }
        self.processor_host.validate_terminal_author_bindings(
            prepared,
            context,
            proposals,
            self.plan.program(),
        )
    }

    pub(crate) fn validate_parameter_native_bindings(
        &mut self,
        prepared: &crate::processor_host::native_author::PreparedParameterNativeBindings,
        proposal: &mondrian_core::NativeAudioAuthorProposal,
    ) -> Result<(), crate::NativeAudioAuthorAdmissionError> {
        let current = self.native_owner_identity();
        self.processor_host.validate_parameter_native_bindings(
            prepared,
            current,
            proposal,
            self.plan.program(),
            self.pending_reprepare_ticket,
        )
    }

    pub(crate) fn publish_parameter_native_bindings(
        &mut self,
        prepared: &crate::processor_host::native_author::PreparedParameterNativeBindings,
        serial: std::num::NonZeroU64,
    ) {
        self.processor_host.publish_parameter_native_bindings(prepared, serial)
    }

    pub(crate) fn acknowledge_parameter_native_author(
        &mut self,
        proposal: &mondrian_core::NativeAudioAuthorProposal,
        serial: std::num::NonZeroU64,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<mondrian_core::NativeAudioEditorTarget, crate::NativeAudioAuthorAdmissionError>
    {
        // All fallible identity checks precede the irreversible native Ack.
        let original = self.processor_host.parameter_author_request_cohort(proposal, serial)?;
        let (target, observations) = self.processor_host.acknowledge_parameter_native_author(
            proposal,
            serial,
            cancellation,
        )?;
        if let Some(original) = original
            && original.serial != 0
            && !original.exhausted()
            && matches!(
                original.request,
                mondrian_core::NativeAudioReprepareRequest::Vst3Restart { flags: 4 | 16 }
            )
        {
            let occurrence = crate::native_runtime_owner::editor_target_address(target).occurrence;
            let epoch = match self.continuity {
                SessionContinuity::Unentered => None,
                SessionContinuity::Active { epoch, .. } | SessionContinuity::Poisoned { epoch } => {
                    Some(epoch)
                }
            };
            let exact = crate::NativeObservedRequest::Cohort(original);
            if self.pending_reprepare_ticket.is_none_or(|ticket| {
                ticket.request == exact
                    && ticket.prepared_plan_id == self.plan.preparation_id()
                    && ticket.session_id == self.session_id
                    && ticket.occurrence == occurrence
                    && ticket.continuity_epoch == epoch
            }) {
                // A successful native Ack may observe a newer or unknown live
                // request. Copy that fact into HOLD without validation, query,
                // cancellation or any other fallible post-Ack operation.
                self.pending_reprepare_ticket = observations.pending_reprepare.map(|request| {
                    crate::AudioProcessorReprepareTicket {
                        prepared_plan_id: self.plan.preparation_id(),
                        session_id: self.session_id,
                        occurrence: crate::native_runtime_owner::editor_target_address(target)
                            .occurrence,
                        continuity_epoch: match self.continuity {
                            SessionContinuity::Unentered => None,
                            SessionContinuity::Active { epoch, .. }
                            | SessionContinuity::Poisoned { epoch } => Some(epoch),
                        },
                        request,
                    }
                });
            }
        }
        Ok(target)
    }

    pub(crate) fn validate_parameter_plan_swap(
        &self,
        next: &PreparedNativeAudioParameterPlan,
        proposal: &mondrian_core::NativeAudioAuthorProposal,
        bindings: &crate::processor_host::native_author::PreparedParameterNativeBindings,
    ) -> Result<(), crate::AudioExecutionError> {
        if self.native_owner_identity() != next.current
            || self.plan.contract() != next.next_plan.contract()
            || matches!(self.continuity, SessionContinuity::Poisoned { .. })
        {
            return Err(crate::AudioExecutionError::InvalidPreparedSchedule);
        }
        if let Some(ticket) = self.pending_reprepare_ticket {
            let prior = bindings
                .selected_record(proposal)
                .ok_or(crate::AudioExecutionError::InvalidPreparedSchedule)?;
            let original = self
                .continuing_rescan_cohort(
                    proposal,
                    prior.prior_root(),
                    proposal.target().installed_generation,
                )
                .map_err(|_| crate::AudioExecutionError::InvalidPreparedSchedule)?
                .ok_or(crate::AudioExecutionError::InvalidPreparedSchedule)?;
            let occurrence =
                crate::native_runtime_owner::editor_target_address(proposal.target()).occurrence;
            if ticket.prepared_plan_id != self.plan.preparation_id()
                || ticket.session_id != self.session_id
                || ticket.occurrence != occurrence
                || !matches!(self.continuity, SessionContinuity::Active { epoch, .. }
                    if ticket.continuity_epoch == Some(epoch))
                || !matches!(ticket.request, crate::NativeObservedRequest::Cohort(live)
                    if live.serial >= original.serial && !live.exhausted() && live.request == original.request)
            {
                return Err(crate::AudioExecutionError::InvalidPreparedSchedule);
            }
        }
        Ok(())
    }

    pub(crate) fn validate_terminal_parameter_plan_swap(
        &self,
        next: &PreparedNativeAudioParameterPlan,
        proposals: &[mondrian_core::NativeAudioAuthorProposal],
        bindings: &crate::processor_host::native_author::PreparedParameterNativeBindings,
    ) -> Result<(), crate::AudioExecutionError> {
        if self.native_owner_identity() != next.current
            || self.plan.contract() != next.next_plan.contract()
            || matches!(self.continuity, SessionContinuity::Poisoned { .. })
        {
            return Err(crate::AudioExecutionError::InvalidPreparedSchedule);
        }
        if let Some(ticket) = self.pending_reprepare_ticket {
            // One Session owns one actual ticket. Find its member once; never
            // apply that one ticket to each unrelated selected native owner.
            let mut members = proposals.iter().filter(|proposal| {
                crate::native_runtime_owner::editor_target_address(proposal.target()).occurrence
                    == ticket.occurrence
            });
            let proposal =
                members.next().ok_or(crate::AudioExecutionError::InvalidPreparedSchedule)?;
            if members.next().is_some() {
                return Err(crate::AudioExecutionError::InvalidPreparedSchedule);
            }
            let prior = bindings
                .selected_record(proposal)
                .ok_or(crate::AudioExecutionError::InvalidPreparedSchedule)?;
            let original = self
                .continuing_rescan_cohort(
                    proposal,
                    prior.prior_root(),
                    proposal.target().installed_generation,
                )
                .map_err(|_| crate::AudioExecutionError::InvalidPreparedSchedule)?
                .ok_or(crate::AudioExecutionError::InvalidPreparedSchedule)?;
            if ticket.prepared_plan_id != self.plan.preparation_id()
                || ticket.session_id != self.session_id
                || !matches!(self.continuity, SessionContinuity::Active { epoch, .. }
                    if ticket.continuity_epoch == Some(epoch))
                || !matches!(ticket.request, crate::NativeObservedRequest::Cohort(live)
                    if live.serial >= original.serial && !live.exhausted() && live.request == original.request)
            {
                return Err(crate::AudioExecutionError::InvalidPreparedSchedule);
            }
        }
        Ok(())
    }

    pub(crate) fn swap_parameter_plan(&mut self, prepared: &mut PreparedNativeAudioParameterPlan) {
        // The exclusive Runtime already validated all tree owners and capacity.
        // Retain old immutable and scratch owners in this prepared slot, so no
        // destructor, allocation or native call runs inside the final guards.
        let maximum_lanes = prepared
            .next_plan
            .schedule
            .processors
            .iter()
            .map(|processor| processor.parameter_ids.len())
            .max()
            .unwrap_or(0);
        let event_capacity = prepared.next_plan.schedule.summary.maximum_parameter_events_per_block;
        self.capacity.maximum_processor_parameter_lanes = maximum_lanes;
        self.capacity.parameter_event_capacity = event_capacity;
        std::mem::swap(&mut self.plan, &mut prepared.next_plan);
        if let Some(ticket) = &mut self.pending_reprepare_ticket {
            // Existing request identity/epoch/occurrence survives the single
            // Source publication. Only its immutable plan address moves.
            ticket.prepared_plan_id = self.plan.preparation_id();
        }
        std::mem::swap(
            &mut self.parameter_event_charge_bytes,
            &mut prepared.next_parameter_event_charge_bytes,
        );
        self.processor_host.swap_parameter_events(
            &mut prepared.next_parameter_events,
            maximum_lanes,
            event_capacity,
        );
    }

    pub(crate) fn current_resource_footprint(
        &self,
    ) -> Result<crate::AudioSessionResourceFootprint, crate::AudioResourceFootprintError> {
        charged_parameter_footprint(&self.plan, self.parameter_event_charge_bytes)
    }

    pub(crate) fn shared_resource_plan(&self) -> &Arc<PreparedAudioPlan> {
        &self.plan
    }

    pub(crate) fn resource_plan(&self) -> &PreparedAudioPlan {
        self.plan.as_ref()
    }

    pub(crate) fn native_owner_identity(&self) -> crate::AudioSessionCursorIdentity {
        crate::AudioSessionCursorIdentity {
            prepared_plan_id: self.plan.preparation_id(),
            session_id: self.session_id,
        }
    }

    pub(crate) fn revoke_native_bindings(&mut self) {
        self.processor_host.revoke_native_bindings();
    }

    pub(crate) fn prepare_native_bindings(
        &mut self,
        root: crate::AudioSessionCursorIdentity,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<usize, crate::AudioNativeOwnerError> {
        if matches!(self.continuity, SessionContinuity::Poisoned { .. }) {
            return Err(crate::AudioNativeOwnerError::ContinuityUnavailable);
        }
        let local = self.native_owner_identity();
        self.processor_host.prepare_native_bindings(root, local, cancellation)
    }

    pub(crate) fn publish_native_bindings(&mut self, installed_root_serial: std::num::NonZeroU64) {
        self.processor_host.publish_native_bindings(installed_root_serial);
    }

    pub(crate) fn validate_prepared_native_bindings(
        &self,
        root: crate::AudioSessionCursorIdentity,
    ) -> Result<usize, crate::AudioNativeOwnerError> {
        if matches!(self.continuity, SessionContinuity::Poisoned { .. }) {
            return Err(crate::AudioNativeOwnerError::ContinuityUnavailable);
        }
        self.processor_host
            .validate_prepared_native_bindings(root, self.native_owner_identity())
    }

    pub(crate) fn native_owner_lease(
        &self,
        address: crate::AudioNativeProcessorAddress,
    ) -> Option<crate::AudioNativeProcessorOwnerLease> {
        if matches!(self.continuity, SessionContinuity::Poisoned { .. }) {
            return None;
        }
        self.processor_host.native_owner_lease(address)
    }

    pub(crate) fn resolve_native_editor_target(
        &self,
        locator: mondrian_core::NativeAudioEditorLocator,
        root: crate::AudioSessionCursorIdentity,
        installed_serial: std::num::NonZeroU64,
    ) -> Result<Option<mondrian_core::NativeAudioEditorTarget>, crate::AudioNativeOwnerError> {
        self.processor_host.resolve_native_editor_target(
            locator,
            root,
            self.native_owner_identity(),
            installed_serial,
        )
    }

    pub(crate) fn validate_native_owner(
        &mut self,
        lease: crate::AudioNativeProcessorOwnerLease,
    ) -> Result<crate::NativeAudioObjectObservation, crate::AudioNativeOwnerError> {
        self.processor_host.validate_native_owner(lease)
    }

    pub(crate) fn retained_native_close_lease(
        &self,
        previous: mondrian_core::NativeAudioEditorTarget,
        root: crate::AudioSessionCursorIdentity,
        serial: std::num::NonZeroU64,
    ) -> Option<crate::AudioNativeProcessorOwnerLease> {
        self.processor_host.retained_native_close_lease(
            previous,
            root,
            self.native_owner_identity(),
            serial,
        )
    }

    pub(crate) fn query_native_parameter(
        &mut self,
        lease: crate::AudioNativeProcessorOwnerLease,
        parameter: &mondrian_core::ParameterId,
    ) -> Result<mondrian_core::NativeAudioParameterQueryOutcome, crate::AudioNativeOwnerError> {
        use mondrian_core::{
            NativeAudioParameterQueryOutcome as Out, NativeAudioParameterQueryRefusal as Refusal,
        };
        if self.native_owner_lease(lease.address()) != Some(lease) {
            return Ok(Out::Refused(Refusal::OwnerChanged));
        }
        // Reuse the exact installed plan's writable lanes. This is admission,
        // not a second parameter directory, default map or UI selection cache.
        if !self.plan.schedule.processors.iter().any(|processor| {
            processor.occurrence == lease.address().occurrence
                && processor.parameter_ids.contains(parameter)
        }) {
            return Ok(Out::Refused(Refusal::ParameterUnavailable));
        }
        let Some(native_id) = parameter
            .as_str()
            .strip_prefix("vst3.param.")
            .and_then(|id| id.parse::<u32>().ok())
        else {
            return Ok(Out::Refused(Refusal::UnsupportedAdapter));
        };
        self.processor_host.query_native_parameter(lease, parameter, native_id)
    }
    pub(crate) fn control_native_editor(
        &mut self,
        lease: crate::AudioNativeProcessorOwnerLease,
        title: Option<mondrian_core::NativeAudioEditorTitle>,
    ) -> Result<crate::NativeAudioEditorOutcome, crate::AudioNativeOwnerError> {
        if self.native_owner_lease(lease.address()) != Some(lease) {
            return Err(crate::AudioNativeOwnerError::OwnerUnavailable);
        }
        let values = if title.is_some() {
            let processor =
                crate::schedule::processor(self.plan.program(), lease.address().occurrence)
                    .map_err(|_| crate::AudioNativeOwnerError::OwnerUnavailable)?;
            let prepared = self
                .plan
                .schedule
                .processors
                .iter()
                .find(|prepared| prepared.occurrence == lease.address().occurrence)
                .ok_or(crate::AudioNativeOwnerError::OwnerUnavailable)?;
            native_editor_constant_values(processor, &prepared.parameter_ids)?
        } else {
            Vec::new()
        };
        self.processor_host.control_native_editor(lease, title, values)
    }

    /// Visit actual generated cursor facts without DSP, allocation or state changes.
    ///
    /// Public output, lookahead execution and local processor signal coordinates
    /// remain distinct. These facts are not a DSP checkpoint or resume capability.
    pub fn visit_continuity_evidence(
        &self,
        visitor: &mut impl crate::AudioContinuityEvidenceVisitor,
    ) -> Result<(), AudioExecutionError> {
        let sample_rate = AudioSampleRate::new(self.plan.contract().sample_rate)?;
        let identity = crate::AudioSessionCursorIdentity {
            prepared_plan_id: self.plan.preparation_id(),
            session_id: self.session_id,
        };
        let state = if !self.requires_state_entry() {
            crate::AudioSessionCursorState::Stateless
        } else {
            match self.continuity {
                SessionContinuity::Unentered => crate::AudioSessionCursorState::Unentered,
                SessionContinuity::Poisoned { epoch } => {
                    crate::AudioSessionCursorState::Poisoned { epoch }
                }
                SessionContinuity::Active {
                    epoch,
                    next_public_sample,
                    next_execution_sample,
                    alignment_primed,
                    ..
                } => crate::AudioSessionCursorState::Active {
                    epoch,
                    next_public_sample: mondrian_core::AudioSamplePosition::new(
                        next_public_sample,
                        sample_rate,
                    ),
                    next_execution_sample: mondrian_core::AudioSamplePosition::new(
                        next_execution_sample,
                        sample_rate,
                    ),
                    alignment_primed,
                },
            }
        };
        visitor.session(crate::AudioSessionCursorEvidence {
            identity,
            sample_rate,
            public_output_lookahead_frames: self.public_output_lookahead_frames(),
            state,
        })?;
        self.processor_host.visit_continuity_evidence(identity, sample_rate, visitor)
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
        self.preflight_transport_activity(request)?;
        self.reject_empty_note_request(request)?;
        self.preflight_native_control(false)?;
        self.render_into_admitted(source, request, destination, None)
    }

    /// A changed context cannot reuse cached or prefetched epoch-owned history.
    /// Refusal is pure: no foreign callback, PCM write or continuity mutation.
    pub(crate) fn preflight_transport_activity(
        &self,
        request: AudioRenderRequest,
    ) -> Result<(), AudioExecutionError> {
        if let SessionContinuity::Active {
            transport_activity: Some(expected), ..
        } = self.continuity
        {
            if expected != request.transport_activity {
                return Err(AudioExecutionError::TransportActivityChanged);
            }
        }
        Ok(())
    }

    pub(crate) fn preflight_output_shape(
        &self,
        request: AudioRenderRequest,
        destination_len: usize,
    ) -> Result<(), AudioExecutionError> {
        if request.frames > self.plan.contract().max_block_frames {
            return Err(AudioExecutionError::BlockTooLarge);
        }
        let samples = request.frames.checked_mul(self.plan.contract().channel_count())
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        if destination_len != samples {
            return Err(AudioExecutionError::OutputSizeMismatch);
        }
        checked_advance_sample(request.start_sample, request.frames)?;
        Ok(())
    }

    pub(crate) fn preflight_native_control(
        &mut self,
        entering: bool,
    ) -> Result<(), AudioExecutionError> {
        // The exact old Session remains blocked after child request flags are
        // consumed. Only real replacement installs a new schedule/PDC owner.
        if let Some(ticket) = self.pending_reprepare_ticket {
            return Err(AudioExecutionError::ReprepareRequired(ticket));
        }
        // Stateful and mixed Sessions retain their existing epoch recovery and
        // error ordering. A wholly stateless failed instance can only be rebuilt;
        // reject before a preceding producer or native query is replayed.
        if !self.requires_state_entry() && self.processor_host.has_failed_instance() {
            return Err(crate::AudioProcessorHostError::PoisonedInstance.into());
        }
        if let Some((occurrence, request)) = self
            .processor_host
            .preflight_native_control(entering.then_some(self.plan.program()))?
        {
            let continuity_epoch = match self.continuity {
                SessionContinuity::Unentered => None,
                SessionContinuity::Active { epoch, .. } | SessionContinuity::Poisoned { epoch } => {
                    Some(epoch)
                }
            };
            let ticket = crate::AudioProcessorReprepareTicket {
                prepared_plan_id: self.plan.preparation_id(),
                session_id: self.session_id,
                occurrence,
                continuity_epoch,
                request,
            };
            self.pending_reprepare_ticket = Some(ticket);
            return Err(AudioExecutionError::ReprepareRequired(ticket));
        }
        Ok(())
    }

    pub(crate) fn native_reprepare_lane_count(
        &self,
        ticket: crate::AudioProcessorReprepareTicket,
    ) -> Result<usize, crate::NativeAudioReprepareError> {
        use crate::NativeAudioReprepareError as Error;
        if self.pending_reprepare_ticket != Some(ticket)
            || self.native_owner_identity()
                != (crate::AudioSessionCursorIdentity {
                    prepared_plan_id: ticket.prepared_plan_id,
                    session_id: ticket.session_id,
                })
            || self.has_notes()
            || !matches!(self.continuity, SessionContinuity::Active { epoch, .. } if Some(epoch) == ticket.continuity_epoch)
        {
            return Err(Error::Admission(
                "restart ticket is not this active root Session",
            ));
        }
        self.plan
            .schedule
            .processors
            .iter()
            .find(|processor| processor.occurrence == ticket.occurrence)
            .map(|processor| processor.parameter_ids.len())
            .ok_or(Error::Admission("restart occurrence absent"))
    }
    pub(crate) fn inspect_native_reprepare(
        &mut self,
        ticket: crate::AudioProcessorReprepareTicket,
        serial: std::num::NonZeroU64,
        cancellation: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<
        crate::processor_host::native_reprepare::ReprepareSelection,
        crate::NativeAudioReprepareError,
    > {
        self.processor_host.inspect_native_reprepare(
            &self.plan.schedule,
            self.plan.program(),
            ticket,
            serial,
            cancellation,
        )
    }
    pub(crate) fn service_native_reprepare(
        &mut self,
        selected: &crate::processor_host::native_reprepare::ReprepareSelection,
        token: std::num::NonZeroU64,
        limit: usize,
        grant: usize,
    ) -> Result<crate::NativeConfigurationControlOutcome, crate::NativeAudioReprepareError> {
        self.processor_host.service_native_reprepare(selected, token, limit, grant)
    }

    /// Retained exact local ticket; other native pending markers remain unconsumed.
    pub fn pending_reprepare_ticket(&self) -> Option<crate::AudioProcessorReprepareTicket> {
        self.pending_reprepare_ticket
    }

    pub(crate) fn has_notes(&self) -> bool {
        !self.plan.schedule.notes.is_empty()
    }

    /// Reject an unsupported empty Note request before any native control I/O.
    pub(crate) fn reject_empty_note_request(
        &self,
        request: AudioRenderRequest,
    ) -> Result<(), AudioExecutionError> {
        if self.has_notes() && request.frames == 0 {
            return Err(crate::AudioProcessorHostError::NoteExecutionRefused(
                "Note score zero-frame processing is unsupported",
            )
            .into());
        }
        Ok(())
    }

    /// Admit the entire prospective DSP window before any source, delay, meter
    /// or caller PCM changes. No IDs, records or cursors are committed here.
    pub(crate) fn preflight_note_window(
        &mut self,
        request: AudioRenderRequest,
    ) -> Result<(), AudioExecutionError> {
        if !self.has_notes() {
            return Ok(());
        }
        self.reject_empty_note_request(request)?;
        let contract = self.plan.contract();
        if request.frames > contract.max_block_frames {
            return Err(AudioExecutionError::BlockTooLarge);
        }
        checked_advance_sample(request.start_sample, request.frames)?;
        let (execution_start, primed) = match self.continuity {
            SessionContinuity::Unentered => return Err(AudioExecutionError::StateEntryRequired),
            SessionContinuity::Poisoned { epoch } => {
                return Err(AudioExecutionError::ContinuityPoisoned(epoch))
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
            SessionContinuity::Active { next_execution_sample, alignment_primed, .. } => {
                (next_execution_sample, alignment_primed)
            }
        };
        let priming_frames = if primed || request.frames == 0 {
            0
        } else {
            self.public_output_lookahead_frames()
        };
        let frames = priming_frames
            .checked_add(request.frames)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        self.processor_host.preflight_notes(
            &self.plan.schedule,
            execution_start,
            frames,
            priming_frames,
        )
    }

    pub(crate) fn render_into_admitted(
        &mut self,
        source: &mut impl AudioPcmSource,
        request: AudioRenderRequest,
        destination: &mut [f32],
        cancellation: Option<&ExecutionCancellationToken>,
    ) -> Result<(), AudioExecutionError> {
        self.preflight_transport_activity(request)?;
        self.preflight_output_shape(request, destination.len())?;
        let contract = self.plan.contract();
        let samples = destination.len();
        self.preflight_note_window(request)?;
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
                    AudioRenderRequest { transport_activity: request.transport_activity, start_sample: execution_start_sample, frames },
                    None,
                    cancellation,
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
                transport_activity: request.transport_activity,
                start_sample: execution_start_sample,
                frames: request.frames,
            },
            Some(request.start_sample),
            cancellation,
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
                transport_activity: Some(request.transport_activity),
                next_public_sample,
                next_execution_sample: checked_advance_sample(
                    execution_start_sample,
                    request.frames,
                )?,
                alignment_primed,
            };
        } else if let SessionContinuity::Active { transport_activity, .. } = &mut self.continuity {
            // Explicitly entered stateless Sessions also retain the supplied
            // context. Unentered stateless evaluation remains random-access.
            *transport_activity = Some(request.transport_activity);
        }
        Ok(())
    }

    fn render_internal(
        &mut self,
        source: &mut impl AudioPcmSource,
        request: AudioRenderRequest,
        meter_start_sample: Option<i64>,
        cancellation: Option<&ExecutionCancellationToken>,
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
        for output in &mut self.processor_output_buffers {
            output[..samples].fill(0.0);
        }
        for &(node_slot, stage) in &schedule.execution_order {
            let node = &schedule.nodes[node_slot];
            let scratch_slot = node.scratch_slot;
            if stage == AudioChannelSignalStage::PreFader {
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
                        &self.sidechain_source_buffers,
                        &mut self.sidechain_delay_lines,
                        &mut self.processor_delay_lines,
                        &mut self.processor_output_buffers,
                        cancellation,
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
                            &self.processor_output_buffers,
                            delay_line,
                            &mut self.scratch,
                        )?;
                    }
                }
            }
            process_strip(
                stage,
                schedule,
                node,
                request,
                contract.sample_rate,
                contract.channel_layout,
                backend,
                &mut self.node_buffers[scratch_slot],
                &mut self.scratch,
                &mut self.processor_host,
                &self.sidechain_source_buffers,
                &mut self.sidechain_delay_lines,
                &mut self.processor_delay_lines,
                &mut self.processor_output_buffers,
                cancellation,
            )?;
            for source_slot in &node.sidechain_source_buffers {
                let source = schedule
                    .sidechain_sources
                    .get(*source_slot)
                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
                if AudioChannelSignalStage::producing(source.port) != stage {
                    continue;
                }
                let destination = self
                    .sidechain_source_buffers
                    .get_mut(*source_slot)
                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
                let pcm = if let Some(output) = source.processor_output {
                    self.processor_output_buffers
                        .get(output.buffer_slot)
                        .and_then(|pcm| pcm.get(..samples))
                        .ok_or(AudioExecutionError::InvalidPreparedSchedule)?
                } else {
                    self.node_buffers[scratch_slot].port(source.port, samples)
                };
                destination[..samples].copy_from_slice(pcm);
            }
            if stage == AudioChannelSignalStage::PostFader
                && meter_start_sample.is_some()
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

/// Validate the checked end of one signed public sample window.
pub(crate) fn checked_advance_sample(
    start_sample: i64,
    frames: usize,
) -> Result<i64, AudioExecutionError> {
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
    sidechain_source_buffers: &[Vec<f32>],
    sidechain_delay_lines: &mut [FixedDelayLine],
    processor_delay_lines: &mut [FixedDelayLine],
    processor_output_buffers: &mut [Vec<f32>],
    cancellation: Option<&ExecutionCancellationToken>,
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
            &schedule.notes,
            &schedule.processor_auxiliary_buses,
            &schedule.processor_output_buses,
            processor_output_buffers,
            &schedule.sidechain_bindings,
            AudioRenderRequest {
                transport_activity: request.transport_activity,
                start_sample: execution_start_sample,
                frames: execution_frame_count,
            },
            sample_rate,
            channel_layout,
            backend,
            &mut scratch.contribution_processed[execution_sample_start..execution_sample_end],
            sidechain_source_buffers,
            sidechain_delay_lines,
            processor_delay_lines,
            execution_frames.start,
            cancellation,
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
        // A compensated Scope can execute before its first authored signal
        // reaches the final processor. Its warmup must not publish pre-Clip audio.
        if edit_signal_start_sample < contribution.sequence_start_sample {
            let leading_frames = usize::try_from(
                contribution
                    .sequence_start_sample
                    .checked_sub(edit_signal_start_sample)
                    .ok_or(AudioExecutionError::BufferTooLarge)?,
            )
            .map_err(|_| AudioExecutionError::BufferTooLarge)?
            .min(execution_frame_count);
            let leading_samples = leading_frames
                .checked_mul(channels)
                .ok_or(AudioExecutionError::BufferTooLarge)?;
            scratch.contribution_sample_gains[..leading_samples].fill(0.0);
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
    processor_output_buffers: &[Vec<f32>],
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
    if let Some(output) = route.processor_output {
        let source = processor_output_buffers
            .get(output.buffer_slot)
            .and_then(|pcm| pcm.get(..samples))
            .ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
        let destination =
            &mut buffers[schedule.nodes[route.destination_slot].scratch_slot].input[..samples];
        return sum_route_signal(
            route,
            source,
            destination,
            automated_gains,
            backend,
            delay_line,
        );
    }
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
    stage: AudioChannelSignalStage,
    schedule: &PreparedAudioSchedule,
    node: &PreparedNode,
    request: AudioRenderRequest,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    backend: crate::AudioKernelBackend,
    buffers: &mut NodeBuffers,
    scratch: &mut RenderScratch,
    processor_host: &mut PreparedProcessorHost,
    sidechain_source_buffers: &[Vec<f32>],
    sidechain_delay_lines: &mut [FixedDelayLine],
    processor_delay_lines: &mut [FixedDelayLine],
    processor_output_buffers: &mut [Vec<f32>],
    cancellation: Option<&ExecutionCancellationToken>,
) -> Result<(), AudioExecutionError> {
    let channels = channel_layout.channel_count();
    let samples = request
        .frames
        .checked_mul(channels)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let sample_rate = AudioSampleRate::new(sample_rate)?;
    if stage == AudioChannelSignalStage::PreFader {
        dsp::multiply_constant_into(
            backend,
            &mut buffers.pre_fader[..samples],
            &buffers.input[..samples],
            dsp::db_to_linear(node.strip.input_trim_db),
        );
        processor_host.process_rack(
            &node.pre_rack,
            &schedule.processors,
            &schedule.notes,
            &schedule.processor_auxiliary_buses,
            &schedule.processor_output_buses,
            processor_output_buffers,
            &schedule.sidechain_bindings,
            request,
            sample_rate,
            channel_layout,
            backend,
            &mut buffers.pre_fader[..samples],
            sidechain_source_buffers,
            sidechain_delay_lines,
            processor_delay_lines,
            0,
            cancellation,
        )?;

        return Ok(());
    }

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
        &schedule.notes,
        &schedule.processor_auxiliary_buses,
        &schedule.processor_output_buses,
        processor_output_buffers,
        &schedule.sidechain_bindings,
        request,
        sample_rate,
        channel_layout,
        backend,
        &mut buffers.post_fader_pre_mute[..samples],
        sidechain_source_buffers,
        sidechain_delay_lines,
        processor_delay_lines,
        0,
        cancellation,
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
    if let Some((duration, curve)) = contribution.semantic.fade_in
        && duration > TimelineTime::ZERO
        && clip_local < duration
    {
        gain *= rising_curve(clip_local.to_f64() / duration.to_f64(), curve);
    }
    if let Some((duration, curve)) = contribution.semantic.fade_out
        && duration > TimelineTime::ZERO
    {
        let remaining = contribution.semantic.sequence_range.duration.checked_sub(clip_local)?;
        if remaining < duration {
            gain *= falling_curve(
                (remaining.to_f64() / duration.to_f64()).clamp(0.0, 1.0),
                curve,
            );
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
    /// An entered generation must not reuse history for a different transport.
    #[error("audio transport activity changed within an entered continuity epoch")]
    TransportActivityChanged,
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
    /// Root preflight deferred before any source, DSP, meter or caller-PCM mutation.
    #[error("{0}")]
    ReprepareRequired(crate::AudioProcessorReprepareTicket),
    /// Actual native callbacks await canonical same-live Plan installation.
    /// Deferred before every source/DSP/meter/caller-PCM mutation; the current
    /// Session, sample cursor and continuity epoch remain exact and unchanged.
    #[error("native author source is pending for {0:?}")]
    NativeAuthorPending(Box<crate::NativeAudioAuthorExecutionHold>),
    /// The exact installed native view needs host-frame cleanup before DSP.
    /// No source, PCM, continuity, configuration or author state has advanced.
    #[error("native editor cleanup is pending for {0:?}")]
    EditorCleanupRequired(Box<mondrian_core::NativeAudioEditorTarget>),
    /// Cold native Enter/Bind lost the actual renderer slot or cancellation authority.
    #[error(transparent)]
    NativeOwner(Box<crate::AudioNativeOwnerError>),
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

impl From<crate::AudioNativeOwnerError> for AudioExecutionError {
    fn from(error: crate::AudioNativeOwnerError) -> Self {
        Self::NativeOwner(Box::new(error))
    }
}

impl From<mondrian_core::AutomationError> for AudioExecutionError {
    fn from(error: mondrian_core::AutomationError) -> Self {
        Self::Automation(error.to_string())
    }
}

#[cfg(test)]
mod native_child_settlement_tests {
    use super::*;
    use crate::{
        AudioCompileRequest, AudioParameterEventBatch, AudioProcessor, AudioProcessorAudioIo,
        AudioProcessorExecutionContract, AudioProcessorFactory, AudioProcessorHostError,
        AudioProcessorPrepareRequest, AudioProcessorProcessContext, AudioProcessorResolver,
        BuiltInAudioProcessorResolver,
    };
    use mondrian_core::{
        NativeChildSettlement as Settlement, NativeChildSettlementError as Error,
        NativeChildSettlementRegistrar as Registrar, NativeChildSettlementReporter as Reporter,
        NativeChildSettlementRequirement as Requirement,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct Counts {
        scopes: AtomicUsize,
        children: AtomicUsize,
        bad: AtomicBool,
        refuse_scopes: AtomicBool,
    }
    impl Reporter for Counts {
        fn begin_scope(&self) -> Result<(), Error> {
            if self.refuse_scopes.load(Ordering::SeqCst) {
                self.unavailable();
                return Err(Error::Unavailable);
            }
            self.scopes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn end_scope(&self) {
            self.scopes.fetch_sub(1, Ordering::SeqCst);
        }
        fn begin_child(&self) -> Result<(), Error> {
            self.children.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn end_child(&self, result: Settlement) {
            self.children.fetch_sub(1, Ordering::SeqCst);
            if result == Settlement::Unconfirmed {
                self.unavailable();
            }
        }
        fn unavailable(&self) {
            self.bad.store(true, Ordering::SeqCst);
        }
    }
    struct MissingProcessor;
    impl AudioProcessor for MissingProcessor {
        fn enter_state(&mut self, _: i64) -> Result<(), AudioProcessorHostError> {
            Ok(())
        }
        fn process(
            &mut self,
            _: AudioProcessorProcessContext,
            _: &mut dyn AudioProcessorAudioIo,
            _: AudioParameterEventBatch<'_>,
        ) -> Result<(), AudioProcessorHostError> {
            Ok(())
        }
    }
    struct Factory {
        inner: Arc<dyn AudioProcessorFactory>,
        requirement: Requirement,
        missing: bool,
        creates: Arc<AtomicUsize>,
    }
    impl AudioProcessorFactory for Factory {
        fn execution_contract(&self) -> AudioProcessorExecutionContract {
            self.inner.execution_contract()
        }
        fn native_child_settlement_requirement(&self) -> Requirement {
            self.requirement
        }
        fn create(
            &self,
            native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
        ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            if self.missing {
                Ok(Box::new(MissingProcessor))
            } else {
                self.inner.create(native_child_settlement)
            }
        }
    }
    struct Resolver {
        requirement: Requirement,
        missing: bool,
        creates: Arc<AtomicUsize>,
    }
    impl AudioProcessorResolver for Resolver {
        fn prepare(
            &self,
            request: AudioProcessorPrepareRequest<'_>,
        ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
            Ok(Arc::new(Factory {
                inner: BuiltInAudioProcessorResolver.prepare(request)?,
                requirement: self.requirement,
                missing: self.missing,
                creates: self.creates.clone(),
            }))
        }
    }
    fn plan(
        requirement: Requirement,
        missing: bool,
        creates: Arc<AtomicUsize>,
    ) -> Arc<PreparedAudioPlan> {
        use mondrian_timeline::{
            audio::{AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID},
            Sequence,
        };
        let mut sequence = Sequence::new("child settlement forwarding");
        sequence.settings.audio_channel_layout = AudioChannelLayout::Stereo;
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(
            AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1),
        );
        let output = sequence.audio_program.outputs[0].id;
        let compiled =
            crate::compile_audio_program(&sequence, AudioCompileRequest::program(output)).unwrap();
        let contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            max_block_frames: 4,
            processing_mode: crate::AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 1024,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        Arc::new(
            PreparedAudioPlan::prepare_with_processor_resolver(
                Arc::new(compiled),
                contract,
                crate::AudioKernelBackend::ScalarReference,
                &Resolver { requirement, missing, creates },
            )
            .unwrap(),
        )
    }

    #[test]
    fn installed_child_requirement_reads_actual_factory_without_creating_or_binding() {
        for requirement in [
            Requirement::NotApplicable,
            Requirement::Required,
            Requirement::Unavailable,
        ] {
            let creates = Arc::new(AtomicUsize::new(0));
            let plan = plan(requirement, false, creates.clone());
            let session = AudioRenderSession::new(plan).unwrap();
            assert_eq!(session.installed_native_child_settlement_requirement(), requirement);
            assert_eq!(session.installed_native_child_settlement_requirement(), requirement);
            assert_eq!(creates.load(Ordering::SeqCst), 1,
                "capability observation cannot construct another instance");
            assert!(session.native_child_settlement.is_none(),
                "declaration observation does not acquire a settlement domain");
        }
    }

    #[test]
    fn installed_child_requirement_never_calls_missing_prospective_instances_not_applicable() {
        let creates = Arc::new(AtomicUsize::new(0));
        let plan = plan(Requirement::NotApplicable, false, creates.clone());
        let prospective = AudioRenderSession::allocate_storage(plan).unwrap();
        assert_eq!(prospective.session.installed_native_child_settlement_requirement(),
            Requirement::Unavailable);
        assert_eq!(creates.load(Ordering::SeqCst), 0,
            "unpopulated owners are unknown, never filled by observation");
    }

    #[test]
    fn native_child_session_candidate_is_counted_before_create_and_factory_retains_no_domain() {
        let counts = Arc::new(Counts::default());
        let root = Registrar::try_new(counts.clone()).unwrap();
        let creates = Arc::new(AtomicUsize::new(0));
        let plan = plan(Requirement::NotApplicable, false, creates.clone());
        let mut session = AudioRenderSession::new(plan.clone()).unwrap();
        session.bind_native_child_settlement(&root).unwrap();
        session.bind_native_child_settlement(&root).unwrap();
        assert_eq!(counts.scopes.load(Ordering::SeqCst), 2);
        let mut candidate = AudioRenderSession::allocate_storage(plan.clone()).unwrap();
        candidate.inherit_native_child_settlement(&session);
        assert_eq!(counts.scopes.load(Ordering::SeqCst), 3);
        assert_eq!(
            creates.load(Ordering::SeqCst),
            1,
            "fork precedes actual candidate create"
        );
        let candidate = candidate.populate().unwrap();
        assert_eq!(creates.load(Ordering::SeqCst), 2);
        assert_eq!(
            counts.children.load(Ordering::SeqCst),
            0,
            "actual builtin is NotApplicable"
        );
        drop(session);
        assert_eq!(counts.scopes.load(Ordering::SeqCst), 2);
        drop(candidate);
        drop(root);
        assert_eq!(counts.scopes.load(Ordering::SeqCst), 0);
        assert!(!counts.bad.load(Ordering::SeqCst));
        assert_eq!(
            Arc::strong_count(&counts),
            1,
            "still-retained Factory/Plan has no domain"
        );
        assert_eq!(
            native_child_settlement_slot_bytes(),
            std::alloc::Layout::new::<Option<Registrar>>().size()
        );
    }

    #[test]
    fn native_child_late_prepared_storage_binding_survives_actual_retained_swap() {
        let counts = Arc::new(Counts::default());
        let other = Arc::new(Counts::default());
        let root = Registrar::try_new(counts.clone()).unwrap();
        let foreign = Registrar::try_new(other.clone()).unwrap();
        let creates = Arc::new(AtomicUsize::new(0));
        let plan = plan(Requirement::NotApplicable, false, creates.clone());
        let mut current = AudioRenderSession::new(plan.clone()).unwrap();
        let mut prepared = AudioRenderSession::allocate_storage(plan).unwrap();
        assert_eq!(creates.load(Ordering::SeqCst), 1);
        // Exact late-first-bind shape: both actual current and already-existing
        // prospective storage must hold their own counted scope before transfer.
        current.bind_native_child_settlement(&root).unwrap();
        prepared.validate_native_child_settlement_domain(&root).unwrap();
        prepared.bind_native_child_settlement(&root).unwrap();
        assert_eq!(counts.scopes.load(Ordering::SeqCst), 3);
        assert_eq!(prepared.session.processor_host.occurrence_count(), 0);
        prepared.swap_retained(&mut current);
        assert_eq!(current.processor_host.occurrence_count(), 1);
        assert_eq!(prepared.session.processor_host.occurrence_count(), 0);
        assert_eq!(
            creates.load(Ordering::SeqCst),
            1,
            "storage swap never creates an instance"
        );
        assert!(current.native_child_settlement.as_ref().unwrap().same_domain(&root));
        assert!(prepared.session.native_child_settlement.as_ref().unwrap().same_domain(&root));
        assert_eq!(
            current.bind_native_child_settlement(&foreign),
            Err(Error::ForeignDomain)
        );
        assert_eq!(
            prepared.validate_native_child_settlement_domain(&foreign),
            Err(Error::ForeignDomain)
        );
        assert_eq!(
            (
                counts.scopes.load(Ordering::SeqCst),
                other.scopes.load(Ordering::SeqCst)
            ),
            (3, 1)
        );
        drop(prepared);
        assert_eq!(
            counts.scopes.load(Ordering::SeqCst),
            2,
            "current remains counted after retired storage drops"
        );
        drop(current);
        assert_eq!(counts.scopes.load(Ordering::SeqCst), 1);
        assert!(counts.bad.load(Ordering::SeqCst) && other.bad.load(Ordering::SeqCst));
        assert_eq!(counts.children.load(Ordering::SeqCst), 0);
        // This is storage ownership evidence, not permission for a public Source
        // commit, successful StateEntry, DSP, or a native plugin incarnation.
    }

    #[test]
    fn native_child_required_cannot_be_washed_by_actual_na_or_default_missing_binding() {
        for (requirement, missing) in [
            (Requirement::Required, false),
            (Requirement::Required, true),
            (Requirement::NotApplicable, true),
            (Requirement::Unavailable, false),
        ] {
            let counts = Arc::new(Counts::default());
            let root = Registrar::try_new(counts.clone()).unwrap();
            let mut session =
                AudioRenderSession::new(plan(requirement, missing, Arc::new(AtomicUsize::new(0))))
                    .unwrap();
            assert_eq!(
                session.bind_native_child_settlement(&root),
                Err(Error::Unavailable)
            );
            assert!(counts.bad.load(Ordering::SeqCst));
            assert_eq!(
                counts.scopes.load(Ordering::SeqCst),
                2,
                "failed evidence still owns its scope"
            );
            assert_eq!(counts.children.load(Ordering::SeqCst), 0);
            drop(session);
            drop(root);
            assert_eq!(counts.scopes.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn native_child_session_foreign_rebind_invalidates_both_without_migration() {
        let a = Arc::new(Counts::default());
        let b = Arc::new(Counts::default());
        let first = Registrar::try_new(a.clone()).unwrap();
        let foreign = Registrar::try_new(b.clone()).unwrap();
        let mut session = AudioRenderSession::new(plan(
            Requirement::NotApplicable,
            false,
            Arc::new(AtomicUsize::new(0)),
        ))
        .unwrap();
        session.bind_native_child_settlement(&first).unwrap();
        assert_eq!(
            session.bind_native_child_settlement(&foreign),
            Err(Error::ForeignDomain)
        );
        assert!(a.bad.load(Ordering::SeqCst) && b.bad.load(Ordering::SeqCst));
        assert_eq!(
            (
                a.scopes.load(Ordering::SeqCst),
                b.scopes.load(Ordering::SeqCst)
            ),
            (2, 1)
        );
        assert!(session.native_child_settlement.as_ref().unwrap().same_domain(&first));
        session.bind_native_child_settlement(&first).unwrap();
        assert!(
            a.bad.load(Ordering::SeqCst),
            "same-domain retry cannot erase the prior failure"
        );
    }

    struct ColdResolver {
        counts: Arc<Counts>,
        prepared: Arc<AtomicUsize>,
        created: Arc<AtomicUsize>,
        known: bool,
        refuse: bool,
        scope_available: bool,
    }
    struct ColdFactory {
        inner: Arc<dyn AudioProcessorFactory>,
        counts: Arc<Counts>,
        created: Arc<AtomicUsize>,
        scope_available: bool,
    }
    impl AudioProcessorResolver for ColdResolver {
        fn native_child_settlement_requirement(&self) -> Requirement {
            if self.known {
                Requirement::NotApplicable
            } else {
                Requirement::Unavailable
            }
        }
        fn prepare(
            &self,
            request: AudioProcessorPrepareRequest<'_>,
        ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
            assert!(request.native_child_settlement_registrar().is_some());
            assert_eq!(self.counts.bad.load(Ordering::SeqCst), !self.known);
            self.prepared.fetch_add(1, Ordering::SeqCst);
            if self.refuse {
                return Err(AudioProcessorHostError::Unavailable(
                    "exact original cold resolve refusal".into(),
                ));
            }
            Ok(Arc::new(ColdFactory {
                inner: BuiltInAudioProcessorResolver.prepare(request)?,
                counts: self.counts.clone(),
                created: self.created.clone(),
                scope_available: self.scope_available,
            }))
        }
    }
    impl AudioProcessorFactory for ColdFactory {
        fn native_child_settlement_requirement(&self) -> Requirement {
            Requirement::NotApplicable
        }
        fn execution_contract(&self) -> AudioProcessorExecutionContract {
            self.inner.execution_contract()
        }
        fn create(
            &self,
            registrar: Option<&Registrar>,
        ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
            assert_eq!(registrar.is_some(), self.scope_available);
            assert_eq!(
                self.counts.scopes.load(Ordering::SeqCst),
                if self.scope_available { 2 } else { 1 }
            );
            self.created.fetch_add(1, Ordering::SeqCst);
            self.inner.create(registrar)
        }
    }
    fn cold_build(
        resolver: &dyn AudioProcessorResolver,
        registrar: &Registrar,
    ) -> Result<crate::AudioProgramRuntime, crate::AudioRuntimeBuildError> {
        use mondrian_timeline::{
            audio::{AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID},
            Sequence,
        };
        struct NoMedia;
        impl crate::AudioMediaResolver for NoMedia {
            fn resolve(
                &self,
                _: mondrian_core::AssetId,
                _: mondrian_core::AudioSourceComponentId,
                _: u32,
            ) -> Result<crate::ResolvedAudioSource, String> {
                panic!("this root has no media");
            }
        }
        let mut sequence = Sequence::new("observed cold construction");
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(
            AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1),
        );
        let contract = AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: sequence.settings.audio_channel_layout,
            max_block_frames: 4,
            processing_mode: crate::AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 1024,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        };
        crate::AudioProgramRuntime::build_observing_native_child_settlement(
            &sequence,
            &[],
            &NoMedia,
            resolver,
            contract,
            AudioCompileRequest::program(sequence.audio_program.outputs[0].id),
            crate::AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 64 * 1024 * 1024),
            Some(registrar),
        )
    }
    #[test]
    fn cold_runtime_observes_prepare_and_counts_session_before_single_create() {
        let counts = Arc::new(Counts::default());
        let root = Registrar::try_new(counts.clone()).unwrap();
        let prepared = Arc::new(AtomicUsize::new(0));
        let created = Arc::new(AtomicUsize::new(0));
        let resolver = ColdResolver {
            counts: counts.clone(),
            prepared: prepared.clone(),
            created: created.clone(),
            known: true,
            refuse: false,
            scope_available: true,
        };
        let runtime = cold_build(&resolver, &root).unwrap();
        assert_eq!(prepared.load(Ordering::SeqCst), 1);
        assert_eq!(created.load(Ordering::SeqCst), 1);
        assert!(!counts.bad.load(Ordering::SeqCst));
        drop(runtime);
        assert_eq!(counts.scopes.load(Ordering::SeqCst), 1);
        assert_eq!(counts.children.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn cold_unknown_resolver_preserves_single_original_error_after_sticky_observation_failure() {
        let counts = Arc::new(Counts::default());
        let root = Registrar::try_new(counts.clone()).unwrap();
        let prepared = Arc::new(AtomicUsize::new(0));
        let created = Arc::new(AtomicUsize::new(0));
        let resolver = ColdResolver {
            counts: counts.clone(),
            prepared: prepared.clone(),
            created: created.clone(),
            known: false,
            refuse: true,
            scope_available: true,
        };
        let error = cold_build(&resolver, &root).err().expect("original resolver refusal");
        assert!(
            matches!(error, crate::AudioRuntimeBuildError::Compile(crate::AudioCompileError::ProcessorPreparation(AudioProcessorHostError::Unavailable(ref detail))) if detail == "exact original cold resolve refusal")
        );
        assert_eq!(prepared.load(Ordering::SeqCst), 1);
        assert_eq!(created.load(Ordering::SeqCst), 0);
        assert!(counts.bad.load(Ordering::SeqCst));
        assert_eq!(counts.scopes.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn cold_registration_failure_keeps_successful_construction_and_never_becomes_not_applicable() {
        let counts = Arc::new(Counts::default());
        let root = Registrar::try_new(counts.clone()).unwrap();
        counts.refuse_scopes.store(true, Ordering::SeqCst);
        let prepared = Arc::new(AtomicUsize::new(0));
        let created = Arc::new(AtomicUsize::new(0));
        let resolver = ColdResolver {
            counts: counts.clone(),
            prepared: prepared.clone(),
            created: created.clone(),
            known: true,
            refuse: false,
            scope_available: false,
        };
        let runtime = cold_build(&resolver, &root)
            .expect("evidence refusal does not refuse DSP construction");
        assert_eq!(prepared.load(Ordering::SeqCst), 1);
        assert_eq!(created.load(Ordering::SeqCst), 1);
        assert!(counts.bad.load(Ordering::SeqCst));
        assert_eq!(counts.scopes.load(Ordering::SeqCst), 1);
        drop(runtime);
        assert_eq!(counts.scopes.load(Ordering::SeqCst), 1);
    }
}

/// Derive only time-invariant canonical ownership from the actual local Plan.
/// A one-key curve still belongs to Timeline evaluation, even if lowering can
/// optimize it as constant. No playhead/default projection is invented here.
fn native_editor_constant_values(
    processor: &crate::plan::CompiledProcessor,
    parameter_ids: &[mondrian_core::ParameterId],
) -> Result<Vec<(u32, u64)>, crate::AudioProcessorHostError> {
    if !matches!(
        processor.definition,
        mondrian_timeline::audio::AudioProcessorDefinitionRef::Vst3 { .. }
            | mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap { .. }
    ) {
        return Ok(Vec::new());
    }
    let invalid = || {
        crate::AudioProcessorHostError::InvalidContract(
            "native editor prepared parameter lane identity is invalid".to_owned(),
        )
    };
    // The exact prepared order owns processing lanes. Validate its complete
    // correspondence without allocating a second ID map or interpreting any
    // adapter's canonical-ID syntax. None and keyed lanes retain their index.
    if parameter_ids.len() != processor.parameters.len()
        || parameter_ids.len() > crate::native_audio_editor::NATIVE_EDITOR_STATIC_PARAMETER_VALUES
        || parameter_ids.iter().enumerate().any(|(index, id)| {
            parameter_ids[..index].contains(id)
                || processor.parameters.get(id).is_none_or(|parameter| {
                    parameter.schema.parameter_id != *id
                        || parameter
                            .automation
                            .as_ref()
                            .is_some_and(|curve| curve.parameter_id != *id)
                })
        })
    {
        return Err(invalid());
    }
    let count = processor
        .parameters
        .values()
        .filter(|parameter| {
            parameter.automation.as_ref().is_some_and(|curve| curve.keyframes.is_empty())
        })
        .count();
    crate::native_audio_editor::static_parameter_value_layout(0, count)?;
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|_| {
        crate::AudioProcessorHostError::InvalidContract(
            "native editor constant request allocation failed".to_owned(),
        )
    })?;
    crate::native_audio_editor::static_parameter_value_layout(0, values.capacity())?;
    for (lane, id) in parameter_ids.iter().enumerate() {
        let parameter = processor.parameters.get(id).ok_or_else(invalid)?;
        let Some(curve) = parameter.automation.as_ref().filter(|curve| curve.keyframes.is_empty())
        else {
            continue;
        };
        if !curve.default_value.is_finite() {
            return Err(crate::AudioProcessorHostError::InvalidContract(
                "native editor constant is not finite".to_owned(),
            ));
        }
        values.push((
            u32::try_from(lane).map_err(|_| invalid())?,
            curve.default_value.to_bits(),
        ));
    }
    Ok(values)
}
#[cfg(test)]
mod native_editor_constant_tests {
    use super::*;
    use crate::processor_isolation::Vst3ParameterDescriptor;
    use mondrian_core::{AudioProcessorInstanceId, ExactAutomationKeyframe};
    use mondrian_timeline::audio::{AudioProcessorDefinitionRef, AudioProcessorParameter};
    #[test]
    fn native_editor_constants_use_only_actual_empty_key_some_and_preserve_none_and_signed_zero() {
        let mut parameters = std::collections::BTreeMap::new();
        for id in 0..4 {
            let descriptor = Vst3ParameterDescriptor {
                id,
                name: format!("P{id}"),
                default_normalized: 0.5,
                step_count: 0,
                can_automate: id != 3,
                read_only: false,
                flags: if id == 3 { 0 } else { 1 },
            };
            let mut parameter =
                AudioProcessorParameter::from_schema(descriptor.authoring_schema().unwrap())
                    .unwrap();
            // Shared control code cannot assume an adapter-specific ID syntax.
            let canonical = mondrian_core::ParameterId::new(format!("owned.lane.{id}")).unwrap();
            parameter.schema.parameter_id = canonical.clone();
            parameter.automation.as_mut().unwrap().parameter_id = canonical;
            match id {
                0 => parameter.automation.as_mut().unwrap().default_value = -0.0,
                1 => parameter.automation = None,
                2 => {
                    parameter
                        .automation
                        .as_mut()
                        .unwrap()
                        .set_keyframe(ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.75))
                        .unwrap();
                }
                3 => parameter.automation.as_mut().unwrap().default_value = 0.25,
                _ => unreachable!(),
            }
            parameters.insert(parameter.schema.parameter_id.clone(), parameter);
        }
        let mut processor = crate::plan::CompiledProcessor {
            instance_id: AudioProcessorInstanceId::new(),
            definition: AudioProcessorDefinitionRef::Vst3 {
                class_id: "test".to_owned(),
                vendor: None,
                schema_version: 1,
                binary_sha256: None,
            },
            parameters,
            opaque_state: Some(vec![1, 2, 3]),
        };
        let before = processor.clone();
        let ids: Vec<_> = processor.parameters.keys().cloned().collect();
        // Deliberately permute the prepared order away from canonical map order.
        // A keyed first lane and a None third lane do not compress the selection.
        let prepared = [
            ids[2].clone(),
            ids[0].clone(),
            ids[1].clone(),
            ids[3].clone(),
        ];
        assert_eq!(
            native_editor_constant_values(&processor, &prepared).unwrap(),
            vec![(1, (-0.0_f64).to_bits()), (3, 0.25_f64.to_bits())]
        );
        assert_eq!(
            processor, before,
            "derivation cannot manufacture Some/default ownership or rewrite opaque state"
        );
        for invalid in [
            prepared[..3].to_vec(),
            vec![
                ids[0].clone(),
                ids[0].clone(),
                ids[2].clone(),
                ids[3].clone(),
            ],
            vec![
                ids[0].clone(),
                mondrian_core::ParameterId::new("native.absent").unwrap(),
                ids[2].clone(),
                ids[3].clone(),
            ],
            vec![ids[0].clone(); 4097],
        ] {
            assert!(native_editor_constant_values(&processor, &invalid).is_err());
        }
        // Validate complete schema identity, including lanes that send no value.
        processor.parameters.get_mut(&ids[1]).unwrap().schema.parameter_id = ids[0].clone();
        assert!(native_editor_constant_values(&processor, &prepared).is_err());
        processor = before.clone();
        processor
            .parameters
            .get_mut(&ids[2])
            .unwrap()
            .automation
            .as_mut()
            .unwrap()
            .parameter_id = ids[0].clone();
        assert!(native_editor_constant_values(&processor, &prepared).is_err());
        processor = before;
        processor.definition = AudioProcessorDefinitionRef::Clap {
            plugin_id: "test".to_owned(),
            schema_version: 1,
            binary_sha256: None,
        };
        let clap_before = processor.clone();
        assert_eq!(
            native_editor_constant_values(&processor, &prepared).unwrap(),
            vec![(1, (-0.0_f64).to_bits()), (3, 0.25_f64.to_bits())],
            "both adapters consume the same prepared-lane static author selection"
        );
        assert_eq!(processor, clap_before);
    }
}
