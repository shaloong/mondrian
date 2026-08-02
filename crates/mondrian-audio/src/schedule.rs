//! Render-contract-specific lowering from semantic audio IR to dense execution data.

use crate::latency::{solve_prepared_latency, PreparedLatencyNodeInput, PreparedNodeLatency};
use crate::plan::{
    AudioRenderContract, CompiledAudioContribution, CompiledAudioProgram, CompiledChannelStrip,
    CompiledProcessingScope, CompiledRack, CompiledTransition,
};
use crate::processor_host::{
    default_processor_resolver, prepare_processor_factory, PreparedProcessorFactoryBinding,
};
use crate::{
    AudioCompileError, AudioProcessorInsertionPoint, AudioProcessorOccurrence,
    AudioProcessorOccurrenceOwner, AudioProcessorResolver, AudioProcessorTail,
    PreparedAudioChannelMixer,
};
use mondrian_core::{
    AudioChannelLayout, AudioChannelMixMatrix, AudioSamplePosition, AudioSampleRate,
    AudioSampleRounding, ExactAutomationCurve, ExactAutomationSegment, MixBusId, ParameterId,
    ProgramOutputId, TimelineTime, TrackId,
};
use mondrian_timeline::audio::{
    AudioChannelStripOutputPort, AudioComponentChannelMapping, AudioRouteDestination,
    AudioRouteSource, AudioTransitionCurve,
};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

/// Instance-specific source facts supplied after media and child plans resolve.
///
/// Semantic compilation deliberately cannot guess these values because plugin
/// realization and the selected Render Contract may change processor latency.
#[derive(Debug, Clone, Default)]
pub(crate) struct AudioPreparationDependencies {
    sources: BTreeMap<mondrian_core::AudioComponentEditId, PreparedSourceDependency>,
}

#[derive(Debug, Clone)]
struct PreparedSourceDependency {
    channel_mix: AudioChannelMixMatrix,
    algorithmic_latency_frames: usize,
    requires_state_entry: bool,
}

impl AudioPreparationDependencies {
    pub(crate) fn insert_source(
        &mut self,
        edit_id: mondrian_core::AudioComponentEditId,
        channel_layout: AudioChannelLayout,
        channel_mix: AudioChannelMixMatrix,
        algorithmic_latency_frames: usize,
        requires_state_entry: bool,
    ) -> Result<(), AudioCompileError> {
        if channel_mix.source_layout() != channel_layout {
            return Err(AudioCompileError::InvalidPreparedGraph(format!(
                "source dependency {edit_id} matrix does not accept its resolved layout"
            )));
        }
        if self
            .sources
            .insert(
                edit_id,
                PreparedSourceDependency {
                    channel_mix,
                    algorithmic_latency_frames,
                    requires_state_entry,
                },
            )
            .is_some()
        {
            return Err(AudioCompileError::InvalidPreparedGraph(format!(
                "duplicate source dependency for contribution {edit_id}"
            )));
        }
        Ok(())
    }

    fn source(
        &self,
        contribution: &CompiledAudioContribution,
        destination_layout: AudioChannelLayout,
    ) -> Result<PreparedSourceDependency, AudioCompileError> {
        if let Some(source) = self.sources.get(&contribution.edit_id) {
            if source.channel_mix.destination_layout() != destination_layout {
                return Err(AudioCompileError::InvalidPreparedGraph(format!(
                    "source dependency {} matrix does not target the Render Contract layout",
                    contribution.edit_id
                )));
            }
            return Ok(source.clone());
        }
        match contribution.source {
            crate::CompiledAudioSource::Media { .. } => {
                let channel_layout = match &contribution.channel_mapping {
                    AudioComponentChannelMapping::Standard => destination_layout,
                    AudioComponentChannelMapping::Explicit(matrix) => matrix.source_layout(),
                };
                Ok(PreparedSourceDependency {
                    channel_mix: resolve_channel_mapping(
                        &contribution.channel_mapping,
                        channel_layout,
                        destination_layout,
                    )?,
                    algorithmic_latency_frames: 0,
                    requires_state_entry: false,
                })
            }
            crate::CompiledAudioSource::NestedOutput { .. } => {
                Err(AudioCompileError::InvalidPreparedGraph(format!(
                    "nested contribution {} has no prepared child-output dependency",
                    contribution.edit_id
                )))
            }
        }
    }
}

pub(crate) fn resolve_channel_mapping(
    mapping: &AudioComponentChannelMapping,
    source_layout: AudioChannelLayout,
    destination_layout: AudioChannelLayout,
) -> Result<AudioChannelMixMatrix, AudioCompileError> {
    match mapping {
        AudioComponentChannelMapping::Standard => {
            AudioChannelMixMatrix::standard(source_layout, destination_layout).map_err(|error| {
                AudioCompileError::InvalidPreparedGraph(format!(
                    "standard Component channel mapping is unavailable: {error}"
                ))
            })
        }
        AudioComponentChannelMapping::Explicit(matrix)
            if matrix.source_layout() == source_layout
                && matrix.destination_layout() == destination_layout =>
        {
            Ok(matrix.clone())
        }
        AudioComponentChannelMapping::Explicit(_) => Err(AudioCompileError::InvalidPreparedGraph(
            "explicit Component channel mapping does not match the resolved source and destination layouts"
                .to_owned(),
        )),
    }
}

/// Prepared DSP kernel selected for one immutable plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioKernelBackend {
    /// Normative scalar implementation used for parity and diagnosis.
    ScalarReference,
    /// Runtime-dispatched SIMD implementation with a scalar CPU fallback.
    #[default]
    RuntimeVectorized,
}

/// Stable structural facts about one dense Prepared Audio Schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedAudioScheduleSummary {
    /// Topologically ordered Track, Bus, and Output nodes.
    pub node_count: usize,
    /// Dense Track nodes.
    pub track_count: usize,
    /// Dense Bus nodes.
    pub bus_count: usize,
    /// Generated PCM contributions grouped by Track slot.
    pub contribution_count: usize,
    /// Largest native source layout admitted by any Contribution.
    pub maximum_source_channels: usize,
    /// Non-zero coefficients across all prepared Component matrices.
    pub channel_mix_coefficient_count: usize,
    /// Incoming routes stored in contiguous destination ranges.
    pub route_count: usize,
    /// Contribution-local Transition bindings.
    pub transition_binding_count: usize,
    /// Validated automation curves lowered into the schedule.
    pub automation_curve_count: usize,
    /// Exact interpolation spans selected before Session execution.
    pub automation_event_span_count: usize,
    /// Generated processor occurrences with independent Session state.
    pub processor_occurrence_count: usize,
    /// Stable parameter lanes across all generated processor occurrences.
    pub processor_parameter_lane_count: usize,
    /// Largest sample-accurate event batch required by one processor block.
    pub maximum_parameter_events_per_block: usize,
    /// Processor-private Session bytes admitted during preparation.
    pub processor_session_scratch_bytes: usize,
    /// Scratch slots after interval-liveness reuse.
    pub scratch_slot_count: usize,
    /// Interleaved sample storage retained by all PDC delay lines.
    pub compensation_delay_samples: usize,
    /// Internal lookahead needed to return a Timeline-aligned Program Output.
    pub public_output_lookahead_frames: usize,
    /// Largest compensation delay inserted at any prepared summing input.
    pub maximum_compensation_frames: usize,
    /// Whether mutable DSP history requires explicit continuity entry.
    pub requires_state_entry: bool,
}

/// Conservative immutable/fixed allocation evidence for one prepared Session.
///
/// The byte values are logical admission sizes, not allocator telemetry. They
/// deliberately include every preallocated sample/event payload plus stable
/// conservative weights for the immutable prepared graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioSessionResourceFootprint {
    /// Immutable prepared-plan logical bytes.
    pub prepared_logical_bytes: usize,
    /// Total fixed bytes retained by one live Session.
    pub fixed_resident_bytes: usize,
    /// Node buffers and block-local reusable render scratch.
    pub render_scratch_bytes: usize,
    /// Processor-declared private Session state.
    pub processor_session_bytes: usize,
    /// Interleaved PDC delay-line samples.
    pub compensation_delay_bytes: usize,
    /// Preallocated sample-accurate parameter-event storage.
    pub parameter_event_bytes: usize,
}

/// Failure to calculate a conservative resource footprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AudioResourceFootprintError {
    /// A validated extent cannot be represented by this target architecture.
    #[error("audio resource footprint exceeds the supported address range")]
    ExtentOverflow,
}

/// Immutable context-specific plan. Mutable buffers and processor instances do not live here.
#[derive(Debug, Clone)]
pub struct PreparedAudioPlan {
    program: Arc<CompiledAudioProgram>,
    contract: AudioRenderContract,
    kernel_backend: AudioKernelBackend,
    pub(crate) schedule: PreparedAudioSchedule,
}

impl PreparedAudioPlan {
    /// Prepare one semantic program for a concrete Render Contract.
    pub fn prepare(
        program: Arc<CompiledAudioProgram>,
        contract: AudioRenderContract,
    ) -> Result<Self, AudioCompileError> {
        Self::prepare_with_backend(program, contract, AudioKernelBackend::default())
    }

    /// Prepare with an explicit backend for reference parity and workload gates.
    pub fn prepare_with_backend(
        program: Arc<CompiledAudioProgram>,
        contract: AudioRenderContract,
        kernel_backend: AudioKernelBackend,
    ) -> Result<Self, AudioCompileError> {
        Self::prepare_with_processor_resolver(
            program,
            contract,
            kernel_backend,
            default_processor_resolver(),
        )
    }

    /// Prepare with one explicit non-realtime processor resolver.
    pub fn prepare_with_processor_resolver(
        program: Arc<CompiledAudioProgram>,
        contract: AudioRenderContract,
        kernel_backend: AudioKernelBackend,
        processor_resolver: &dyn AudioProcessorResolver,
    ) -> Result<Self, AudioCompileError> {
        Self::prepare_with_dependencies(
            program,
            contract,
            kernel_backend,
            &AudioPreparationDependencies::default(),
            processor_resolver,
        )
    }

    pub(crate) fn prepare_with_dependencies(
        program: Arc<CompiledAudioProgram>,
        contract: AudioRenderContract,
        kernel_backend: AudioKernelBackend,
        dependencies: &AudioPreparationDependencies,
        processor_resolver: &dyn AudioProcessorResolver,
    ) -> Result<Self, AudioCompileError> {
        if contract.sample_rate == 0
            || contract.max_block_frames == 0
            || u32::try_from(contract.max_block_frames).is_err()
        {
            return Err(AudioCompileError::InvalidRenderContract);
        }
        let schedule = PreparedAudioSchedule::build(
            program.as_ref(),
            contract,
            dependencies,
            processor_resolver,
        )?;
        Ok(Self { program, contract, kernel_backend, schedule })
    }

    /// Concrete Render Contract used by Sessions.
    pub fn contract(&self) -> AudioRenderContract {
        self.contract
    }

    /// Exact semantic Program from which this plan was prepared.
    pub fn program(&self) -> &CompiledAudioProgram {
        self.program.as_ref()
    }

    /// DSP backend selected before Session construction.
    pub fn kernel_backend(&self) -> AudioKernelBackend {
        self.kernel_backend
    }

    /// Dense schedule facts suitable for diagnostics and workload matrices.
    pub fn schedule_summary(&self) -> PreparedAudioScheduleSummary {
        self.schedule.summary
    }

    /// Calculate the allocation envelope before constructing a mutable Session.
    ///
    /// This is used by closure-wide admission so nested Sessions cannot each
    /// consume an independent copy of the per-Session Render Contract budget.
    pub fn session_resource_footprint(
        &self,
    ) -> Result<AudioSessionResourceFootprint, AudioResourceFootprintError> {
        session_resource_footprint(self.contract, self.schedule.summary)
    }

    /// Internal lookahead needed to return Timeline-aligned public PCM.
    pub fn public_output_lookahead_frames(&self) -> usize {
        self.schedule.summary.public_output_lookahead_frames
    }

    /// Whether Sessions must enter a continuity epoch before rendering.
    pub fn requires_state_entry(&self) -> bool {
        self.schedule.summary.requires_state_entry
    }
}

fn session_resource_footprint(
    contract: AudioRenderContract,
    summary: PreparedAudioScheduleSummary,
) -> Result<AudioSessionResourceFootprint, AudioResourceFootprintError> {
    const PREPARED_BASE_BYTES: usize = 4 * 1024;
    const PREPARED_NODE_BYTES: usize = 2 * 1024;
    const PREPARED_ROUTE_BYTES: usize = 512;
    const PREPARED_CONTRIBUTION_BYTES: usize = 4 * 1024;
    const PREPARED_PROCESSOR_BYTES: usize = 4 * 1024;
    const PREPARED_AUTOMATION_CURVE_BYTES: usize = 512;
    const PREPARED_AUTOMATION_SPAN_BYTES: usize = 64;
    const PREPARED_TRANSITION_BYTES: usize = 512;
    const PROCESSOR_INSTANCE_METADATA_BYTES: usize = 256;
    const METER_CHANNEL_STATE_BYTES: usize = 64;
    const SESSION_BASE_BYTES: usize = 4 * 1024;

    let prepared_logical_bytes = checked_sum([
        PREPARED_BASE_BYTES,
        checked_product([summary.node_count, PREPARED_NODE_BYTES])?,
        checked_product([summary.route_count, PREPARED_ROUTE_BYTES])?,
        checked_product([summary.contribution_count, PREPARED_CONTRIBUTION_BYTES])?,
        checked_product([summary.processor_occurrence_count, PREPARED_PROCESSOR_BYTES])?,
        checked_product([
            summary.automation_curve_count,
            PREPARED_AUTOMATION_CURVE_BYTES,
        ])?,
        checked_product([
            summary.automation_event_span_count,
            PREPARED_AUTOMATION_SPAN_BYTES,
        ])?,
        checked_product([summary.transition_binding_count, PREPARED_TRANSITION_BYTES])?,
    ])?;

    let channels = contract.channel_count();
    let block_samples = checked_product([contract.max_block_frames, channels])?;
    let node_buffers = checked_product([
        summary.scratch_slot_count,
        block_samples,
        4,
        std::mem::size_of::<f32>(),
    ])?;
    let source_frames = checked_product([contract.max_block_frames, std::mem::size_of::<i64>()])?;
    let source_pcm = checked_product([
        contract.max_block_frames,
        summary.maximum_source_channels,
        std::mem::size_of::<f32>(),
    ])?;
    let reusable_pcm = checked_product([block_samples, 5, std::mem::size_of::<f32>()])?;
    let gain_parameter_lanes =
        checked_product([contract.max_block_frames, 2, std::mem::size_of::<f64>()])?;
    let render_scratch_bytes = checked_sum([
        node_buffers,
        source_frames,
        source_pcm,
        reusable_pcm,
        gain_parameter_lanes,
    ])?;
    let compensation_delay_bytes = checked_product([
        summary.compensation_delay_samples,
        std::mem::size_of::<f32>(),
    ])?;
    let parameter_event_bytes = checked_sum([
        checked_product([
            summary.maximum_parameter_events_per_block,
            std::mem::size_of::<crate::AudioParameterEvent>(),
        ])?,
        checked_product([
            summary.processor_parameter_lane_count,
            std::mem::size_of::<Range<usize>>(),
        ])?,
    ])?;
    let fixed_resident_bytes = checked_sum([
        SESSION_BASE_BYTES,
        render_scratch_bytes,
        summary.processor_session_scratch_bytes,
        compensation_delay_bytes,
        parameter_event_bytes,
        checked_product([
            summary.processor_occurrence_count,
            PROCESSOR_INSTANCE_METADATA_BYTES,
        ])?,
        checked_product([channels, METER_CHANNEL_STATE_BYTES])?,
    ])?;
    Ok(AudioSessionResourceFootprint {
        prepared_logical_bytes,
        fixed_resident_bytes,
        render_scratch_bytes,
        processor_session_bytes: summary.processor_session_scratch_bytes,
        compensation_delay_bytes,
        parameter_event_bytes,
    })
}

fn checked_product<const N: usize>(
    factors: [usize; N],
) -> Result<usize, AudioResourceFootprintError> {
    factors.into_iter().try_fold(1_usize, |product, factor| {
        product.checked_mul(factor).ok_or(AudioResourceFootprintError::ExtentOverflow)
    })
}

fn checked_sum<const N: usize>(values: [usize; N]) -> Result<usize, AudioResourceFootprintError> {
    values.into_iter().try_fold(0_usize, |sum, value| {
        sum.checked_add(value).ok_or(AudioResourceFootprintError::ExtentOverflow)
    })
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedAudioSchedule {
    pub(crate) nodes: Vec<PreparedNode>,
    pub(crate) routes: Vec<PreparedRoute>,
    pub(crate) contributions: Vec<PreparedContribution>,
    pub(crate) processors: Vec<PreparedProcessor>,
    pub(crate) scopes: Vec<CompiledProcessingScope>,
    pub(crate) transitions: Vec<PreparedTransitionBinding>,
    pub(crate) output_slot: usize,
    pub(crate) summary: PreparedAudioScheduleSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparedNodeOrigin {
    Track(TrackId),
    Bus(MixBusId),
    Output(ProgramOutputId),
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedNode {
    pub(crate) origin: PreparedNodeOrigin,
    pub(crate) strip: CompiledChannelStrip,
    pub(crate) muted: bool,
    pub(crate) incoming: Range<usize>,
    pub(crate) contributions: Range<usize>,
    pub(crate) scratch_slot: usize,
    pub(crate) pre_rack: PreparedRack,
    pub(crate) fader_automation: Option<PreparedAutomationCurve>,
    pub(crate) post_rack: PreparedRack,
    pub(crate) latency: PreparedNodeLatency,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedRoute {
    pub(crate) source_slot: usize,
    pub(crate) destination_slot: usize,
    pub(crate) source_port: AudioChannelStripOutputPort,
    pub(crate) constant_gain: Option<f32>,
    pub(crate) gain_automation: Option<PreparedAutomationCurve>,
    pub(crate) compensation_delay_frames: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedContribution {
    pub(crate) semantic: CompiledAudioContribution,
    pub(crate) track_slot: usize,
    pub(crate) scope_slot: usize,
    pub(crate) transitions: Range<usize>,
    pub(crate) channel_mixer: PreparedAudioChannelMixer,
    pub(crate) sequence_start_sample: i64,
    pub(crate) sequence_end_sample: i64,
    /// Exclusive end of causal execution, or `None` for an infinite tail.
    pub(crate) execution_end_sample: Option<i64>,
    pub(crate) constant_scope_gain: Option<f32>,
    pub(crate) constant_edit_gain_pan: Option<(f32, f64)>,
    pub(crate) scope_input_automation: Option<PreparedAutomationCurve>,
    pub(crate) scope_rack: PreparedRack,
    pub(crate) volume_automation: Option<PreparedAutomationCurve>,
    pub(crate) pan_automation: Option<PreparedAutomationCurve>,
    pub(crate) source_algorithmic_latency_frames: usize,
    pub(crate) source_requires_state_entry: bool,
    pub(crate) compensation_delay_frames: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedRack {
    pub(crate) processors: Range<usize>,
    pub(crate) algorithmic_latency_frames: usize,
    pub(crate) tail: AudioProcessorTail,
    pub(crate) requires_state_entry: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedProcessor {
    pub(crate) occurrence: AudioProcessorOccurrence,
    pub(crate) parameter_ids: Vec<ParameterId>,
    pub(crate) parameter_curves: Vec<PreparedAutomationCurve>,
    pub(crate) rack_prefix_algorithmic_latency_frames: usize,
    pub(crate) input_signal_delay_frames: usize,
    pub(crate) factory: PreparedProcessorFactoryBinding,
}

/// One author curve lowered into exact sample-grid event spans.
#[derive(Debug, Clone)]
pub(crate) struct PreparedAutomationCurve {
    owner_time_offset: TimelineTime,
    segments: Vec<PreparedAutomationSegment>,
    constant_value: f64,
    constant: bool,
}

#[derive(Debug, Clone)]
struct PreparedAutomationSegment {
    end_sample: i64,
    evaluator: ExactAutomationSegment,
}

impl PreparedAutomationCurve {
    fn build(
        curve: &ExactAutomationCurve,
        owner_time_offset: TimelineTime,
        sample_rate: AudioSampleRate,
    ) -> Result<Self, AudioCompileError> {
        let evaluators = curve.prepared_segments().map_err(|error| {
            AudioCompileError::InvalidPreparedGraph(format!(
                "automation curve {} is invalid: {error}",
                curve.parameter_id
            ))
        })?;
        let constant_value =
            curve.keyframes.last().map_or(curve.default_value, |keyframe| keyframe.value);
        let mut segments = Vec::with_capacity(evaluators.len());
        for evaluator in evaluators {
            let sequence_end =
                evaluator.end_time().checked_sub(owner_time_offset).map_err(|error| {
                    AudioCompileError::InvalidPreparedGraph(format!(
                        "automation event cannot map into Sequence time: {error}"
                    ))
                })?;
            let end_sample = AudioSamplePosition::from_timeline_time(
                sequence_end,
                sample_rate,
                AudioSampleRounding::Ceil,
            )
            .map_err(|error| {
                AudioCompileError::InvalidPreparedGraph(format!(
                    "automation event cannot lower to the Evaluation Grid: {error}"
                ))
            })?
            .sample();
            segments.push(PreparedAutomationSegment { end_sample, evaluator });
        }
        Ok(Self {
            owner_time_offset,
            segments,
            constant_value,
            constant: curve.keyframes.len() <= 1,
        })
    }

    pub(crate) fn event_span_count(&self) -> usize {
        self.segments.len().saturating_add(1)
    }

    pub(crate) const fn is_constant(&self) -> bool {
        self.constant
    }

    pub(crate) const fn constant_value(&self) -> f64 {
        self.constant_value
    }

    pub(crate) fn initial_cursor(&self, sample: i64) -> usize {
        self.segments.partition_point(|segment| segment.end_sample <= sample)
    }

    pub(crate) fn evaluate_sample(
        &self,
        sample: i64,
        sample_rate: AudioSampleRate,
        cursor: &mut usize,
    ) -> Result<f64, mondrian_core::AutomationError> {
        while self.segments.get(*cursor).is_some_and(|segment| sample >= segment.end_sample) {
            *cursor += 1;
        }
        let Some(segment) = self.segments.get(*cursor) else {
            return Ok(self.constant_value);
        };
        let sequence_time = TimelineTime::new(sample, i64::from(sample_rate.hz()))?;
        let owner_time = sequence_time.checked_add(self.owner_time_offset)?;
        segment.evaluator.evaluate(owner_time)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PreparedTransitionBinding {
    pub(crate) sequence_range: mondrian_core::TimelineTimeRange,
    pub(crate) curve: AudioTransitionCurve,
    pub(crate) direction: PreparedTransitionDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparedTransitionDirection {
    Rising,
    Falling,
}

impl PreparedAudioSchedule {
    fn build(
        program: &CompiledAudioProgram,
        contract: AudioRenderContract,
        dependencies: &AudioPreparationDependencies,
        processor_resolver: &dyn AudioProcessorResolver,
    ) -> Result<Self, AudioCompileError> {
        let sample_rate = AudioSampleRate::new(contract.sample_rate).map_err(|error| {
            AudioCompileError::InvalidPreparedGraph(format!(
                "invalid prepared Evaluation Grid: {error}"
            ))
        })?;
        let track_count = program.track_channels.len();
        let bus_count = program.bus_order.len();
        let mut nodes = Vec::with_capacity(track_count.saturating_add(bus_count).saturating_add(1));
        let mut processors = Vec::new();
        let mut track_slots = BTreeMap::new();
        let mut bus_slots = BTreeMap::new();

        for (track_id, channel) in &program.track_channels {
            let slot = nodes.len();
            track_slots.insert(*track_id, slot);
            let owner = AudioProcessorOccurrenceOwner::Track(*track_id);
            let pre_rack = prepare_rack(
                &channel.strip.pre_fader,
                owner,
                AudioProcessorInsertionPoint::PreFader,
                TimelineTime::ZERO,
                sample_rate,
                contract,
                processor_resolver,
                &mut processors,
            )?;
            let post_rack = prepare_rack(
                &channel.strip.post_fader,
                owner,
                AudioProcessorInsertionPoint::PostFader,
                TimelineTime::ZERO,
                sample_rate,
                contract,
                processor_resolver,
                &mut processors,
            )?;
            nodes.push(PreparedNode {
                origin: PreparedNodeOrigin::Track(*track_id),
                strip: channel.strip.clone(),
                muted: channel.muted,
                incoming: 0..0,
                contributions: 0..0,
                scratch_slot: 0,
                pre_rack,
                fader_automation: prepare_optional_curve(
                    channel.strip.fader_automation.as_ref(),
                    TimelineTime::ZERO,
                    sample_rate,
                )?,
                post_rack,
                latency: PreparedNodeLatency::default(),
            });
        }
        for bus_id in &program.bus_order {
            let strip = program.buses.get(bus_id).ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(format!(
                    "topological Bus {bus_id} is absent from semantic IR"
                ))
            })?;
            let slot = nodes.len();
            bus_slots.insert(*bus_id, slot);
            let owner = AudioProcessorOccurrenceOwner::Bus(*bus_id);
            let pre_rack = prepare_rack(
                &strip.pre_fader,
                owner,
                AudioProcessorInsertionPoint::PreFader,
                TimelineTime::ZERO,
                sample_rate,
                contract,
                processor_resolver,
                &mut processors,
            )?;
            let post_rack = prepare_rack(
                &strip.post_fader,
                owner,
                AudioProcessorInsertionPoint::PostFader,
                TimelineTime::ZERO,
                sample_rate,
                contract,
                processor_resolver,
                &mut processors,
            )?;
            nodes.push(PreparedNode {
                origin: PreparedNodeOrigin::Bus(*bus_id),
                strip: strip.clone(),
                muted: false,
                incoming: 0..0,
                contributions: 0..0,
                scratch_slot: 0,
                pre_rack,
                fader_automation: prepare_optional_curve(
                    strip.fader_automation.as_ref(),
                    TimelineTime::ZERO,
                    sample_rate,
                )?,
                post_rack,
                latency: PreparedNodeLatency::default(),
            });
        }
        let output_slot = nodes.len();
        let owner = AudioProcessorOccurrenceOwner::Output(program.output_id);
        let pre_rack = prepare_rack(
            &program.output.pre_fader,
            owner,
            AudioProcessorInsertionPoint::PreFader,
            TimelineTime::ZERO,
            sample_rate,
            contract,
            processor_resolver,
            &mut processors,
        )?;
        let post_rack = prepare_rack(
            &program.output.post_fader,
            owner,
            AudioProcessorInsertionPoint::PostFader,
            TimelineTime::ZERO,
            sample_rate,
            contract,
            processor_resolver,
            &mut processors,
        )?;
        nodes.push(PreparedNode {
            origin: PreparedNodeOrigin::Output(program.output_id),
            strip: program.output.clone(),
            muted: false,
            incoming: 0..0,
            contributions: 0..0,
            scratch_slot: 0,
            pre_rack,
            fader_automation: prepare_optional_curve(
                program.output.fader_automation.as_ref(),
                TimelineTime::ZERO,
                sample_rate,
            )?,
            post_rack,
            latency: PreparedNodeLatency::default(),
        });

        let mut scope_slots = BTreeMap::new();
        let mut scopes = Vec::with_capacity(program.processing_scopes.len());
        for (scope_id, scope) in &program.processing_scopes {
            scope_slots.insert(*scope_id, scopes.len());
            scopes.push(scope.clone());
        }

        let mut semantic_contributions = program
            .contributions
            .iter()
            .map(|semantic| {
                let track_slot = track_slots.get(&semantic.track_id).copied().ok_or_else(|| {
                    AudioCompileError::InvalidPreparedGraph(format!(
                        "Contribution {} targets missing Track {}",
                        semantic.edit_id, semantic.track_id
                    ))
                })?;
                let scope_slot =
                    scope_slots.get(&semantic.processing_scope).copied().ok_or_else(|| {
                        AudioCompileError::InvalidPreparedGraph(format!(
                            "Contribution {} targets missing processing scope {}",
                            semantic.edit_id, semantic.processing_scope
                        ))
                    })?;
                Ok((track_slot, scope_slot, semantic.clone()))
            })
            .collect::<Result<Vec<_>, AudioCompileError>>()?;
        semantic_contributions
            .sort_by_key(|(track_slot, _, semantic)| (*track_slot, semantic.edit_id));

        let mut transitions = Vec::new();
        let mut contributions = Vec::with_capacity(semantic_contributions.len());
        for (track_slot, scope_slot, semantic) in semantic_contributions {
            let transition_start = transitions.len();
            append_transition_bindings(&mut transitions, &program.transitions, &semantic);
            let transition_end = transitions.len();
            let sequence_start_sample = AudioSamplePosition::from_timeline_time(
                semantic.sequence_range.start,
                sample_rate,
                AudioSampleRounding::Ceil,
            )
            .map_err(|error| {
                AudioCompileError::InvalidPreparedGraph(format!(
                    "Contribution {} start cannot be lowered to the Evaluation Grid: {error}",
                    semantic.edit_id
                ))
            })?
            .sample();
            let sequence_end_sample = AudioSamplePosition::from_timeline_time(
                semantic.sequence_range.end().map_err(|error| {
                    AudioCompileError::InvalidPreparedGraph(format!(
                        "Contribution {} end is invalid: {error}",
                        semantic.edit_id
                    ))
                })?,
                sample_rate,
                AudioSampleRounding::Ceil,
            )
            .map_err(|error| {
                AudioCompileError::InvalidPreparedGraph(format!(
                    "Contribution {} end cannot be lowered to the Evaluation Grid: {error}",
                    semantic.edit_id
                ))
            })?
            .sample();
            let constant_scope_gain = constant_curve_value(
                scopes[scope_slot].input_gain_automation.as_ref(),
                scopes[scope_slot].input_gain_db,
            )
            .map(crate::dsp::db_to_linear);
            let constant_edit_gain_pan = prepared_contribution_constant_edit_gain_pan(
                &semantic,
                transition_start == transition_end,
            );
            let scope_time_offset =
                semantic.scope_in.checked_sub(semantic.sequence_range.start).map_err(|error| {
                    AudioCompileError::InvalidPreparedGraph(format!(
                        "Contribution {} Scope time mapping is invalid: {error}",
                        semantic.edit_id
                    ))
                })?;
            let edit_time_offset = semantic
                .local_time_in
                .checked_sub(semantic.sequence_range.start)
                .map_err(|error| {
                    AudioCompileError::InvalidPreparedGraph(format!(
                        "Contribution {} Edit time mapping is invalid: {error}",
                        semantic.edit_id
                    ))
                })?;
            let scope_input_automation = prepare_optional_curve(
                scopes[scope_slot].input_gain_automation.as_ref(),
                scope_time_offset,
                sample_rate,
            )?;
            let scope_rack = prepare_rack(
                &scopes[scope_slot].rack,
                AudioProcessorOccurrenceOwner::Contribution {
                    edit_id: semantic.edit_id,
                    scope_id: scopes[scope_slot].id,
                },
                AudioProcessorInsertionPoint::Scope,
                scope_time_offset,
                sample_rate,
                contract,
                processor_resolver,
                &mut processors,
            )?;
            let volume_automation = prepare_optional_curve(
                semantic.volume_automation.as_ref(),
                edit_time_offset,
                sample_rate,
            )?;
            let pan_automation = prepare_optional_curve(
                semantic.pan_automation.as_ref(),
                edit_time_offset,
                sample_rate,
            )?;
            let source = dependencies.source(&semantic, contract.channel_layout)?;
            let intrinsic_algorithmic_latency_frames = source
                .algorithmic_latency_frames
                .checked_add(scope_rack.algorithmic_latency_frames)
                .ok_or_else(|| {
                    AudioCompileError::InvalidPreparedGraph(format!(
                        "Contribution {} total algorithmic latency overflowed",
                        semantic.edit_id
                    ))
                })?;
            let execution_extension_frames = match scope_rack.tail {
                AudioProcessorTail::None => Some(intrinsic_algorithmic_latency_frames),
                AudioProcessorTail::Finite(tail_frames) => Some(
                    intrinsic_algorithmic_latency_frames.checked_add(tail_frames).ok_or_else(
                        || {
                            AudioCompileError::InvalidPreparedGraph(format!(
                                "Contribution {} execution extent overflowed",
                                semantic.edit_id
                            ))
                        },
                    )?,
                ),
                AudioProcessorTail::Infinite => None,
            };
            let execution_end_sample = execution_extension_frames
                .map(|extension_frames| {
                    let extension_samples = i64::try_from(extension_frames).map_err(|_| {
                        AudioCompileError::InvalidPreparedGraph(format!(
                            "Contribution {} execution extent cannot fit the Evaluation Grid",
                            semantic.edit_id
                        ))
                    })?;
                    sequence_end_sample.checked_add(extension_samples).ok_or_else(|| {
                        AudioCompileError::InvalidPreparedGraph(format!(
                            "Contribution {} execution end overflowed the Evaluation Grid",
                            semantic.edit_id
                        ))
                    })
                })
                .transpose()?;
            contributions.push(PreparedContribution {
                source_algorithmic_latency_frames: source.algorithmic_latency_frames,
                source_requires_state_entry: source.requires_state_entry,
                compensation_delay_frames: 0,
                channel_mixer: PreparedAudioChannelMixer::new(source.channel_mix),
                semantic,
                track_slot,
                scope_slot,
                transitions: transition_start..transition_end,
                sequence_start_sample,
                sequence_end_sample,
                execution_end_sample,
                constant_scope_gain,
                constant_edit_gain_pan,
                scope_input_automation,
                scope_rack,
                volume_automation,
                pan_automation,
            });
        }
        let mut contribution_cursor = 0;
        for (node_slot, node) in nodes.iter_mut().enumerate() {
            let start = contribution_cursor;
            while contribution_cursor < contributions.len()
                && contributions[contribution_cursor].track_slot == node_slot
            {
                contribution_cursor += 1;
            }
            node.contributions = start..contribution_cursor;
        }

        let mut routes = Vec::with_capacity(program.routes.len());
        for (destination_slot, node) in nodes.iter_mut().enumerate() {
            let start = routes.len();
            let destination = match node.origin {
                PreparedNodeOrigin::Track(_) => None,
                PreparedNodeOrigin::Bus(bus_id) => Some(AudioRouteDestination::Bus(bus_id)),
                PreparedNodeOrigin::Output(output_id) => {
                    Some(AudioRouteDestination::Output(output_id))
                }
            };
            if let Some(destination) = destination {
                for route in program.routes.iter().filter(|route| route.destination == destination)
                {
                    let (source_slot, source_port) = match route.source {
                        AudioRouteSource::Track { track_id, port } => (
                            track_slots.get(&track_id).copied().ok_or_else(|| {
                                AudioCompileError::InvalidPreparedGraph(format!(
                                    "Route {} references missing Track {track_id}",
                                    route.id
                                ))
                            })?,
                            port,
                        ),
                        AudioRouteSource::Bus { bus_id, port } => (
                            bus_slots.get(&bus_id).copied().ok_or_else(|| {
                                AudioCompileError::InvalidPreparedGraph(format!(
                                    "Route {} references missing Bus {bus_id}",
                                    route.id
                                ))
                            })?,
                            port,
                        ),
                    };
                    if source_slot >= destination_slot {
                        return Err(AudioCompileError::InvalidPreparedGraph(format!(
                            "Route {} violates the prepared topological order",
                            route.id
                        )));
                    }
                    let gain_automation = prepare_optional_curve(
                        route.gain_automation.as_ref(),
                        TimelineTime::ZERO,
                        sample_rate,
                    )?;
                    routes.push(PreparedRoute {
                        source_slot,
                        destination_slot,
                        source_port,
                        constant_gain: gain_automation
                            .is_none()
                            .then(|| crate::dsp::db_to_linear(route.gain_db)),
                        gain_automation,
                        compensation_delay_frames: 0,
                    });
                }
            }
            node.incoming = start..routes.len();
        }

        let latency_nodes = nodes
            .iter()
            .map(|node| {
                Ok(PreparedLatencyNodeInput {
                    origin: node.origin,
                    contribution_start: node.contributions.start,
                    contribution_end: node.contributions.end,
                    route_start: node.incoming.start,
                    route_end: node.incoming.end,
                    pre_rack_latency_frames: node.pre_rack.algorithmic_latency_frames,
                    post_rack_latency_frames: node.post_rack.algorithmic_latency_frames,
                })
            })
            .collect::<Result<Vec<_>, AudioCompileError>>()?;
        let contribution_intrinsic_latency_frames = contributions
            .iter()
            .map(|contribution| {
                contribution
                    .source_algorithmic_latency_frames
                    .checked_add(contribution.scope_rack.algorithmic_latency_frames)
                    .ok_or_else(|| {
                        AudioCompileError::InvalidPreparedGraph(format!(
                            "Contribution {} total latency overflowed",
                            contribution.semantic.edit_id
                        ))
                    })
            })
            .collect::<Result<Vec<_>, AudioCompileError>>()?;
        let latency = solve_prepared_latency(
            &latency_nodes,
            &routes,
            &contribution_intrinsic_latency_frames,
            output_slot,
        )
        .map_err(|reason| AudioCompileError::InvalidPreparedGraph(reason.to_owned()))?;
        for (contribution, compensation) in contributions
            .iter_mut()
            .zip(latency.contribution_compensation_frames.iter().copied())
        {
            contribution.compensation_delay_frames = compensation;
        }
        for (route, compensation) in
            routes.iter_mut().zip(latency.route_compensation_frames.iter().copied())
        {
            route.compensation_delay_frames = compensation;
        }
        for (node, node_latency) in nodes.iter_mut().zip(latency.node_latencies.iter().copied()) {
            node.latency = node_latency;
        }
        for contribution in &contributions {
            assign_rack_processor_signal_delays(
                &mut processors,
                &contribution.scope_rack,
                contribution.source_algorithmic_latency_frames,
            )?;
        }
        for node in &nodes {
            assign_rack_processor_signal_delays(
                &mut processors,
                &node.pre_rack,
                node.latency.input_frames,
            )?;
            assign_rack_processor_signal_delays(
                &mut processors,
                &node.post_rack,
                node.latency.pre_fader_frames,
            )?;
        }

        let mut last_consumer = (0..nodes.len()).collect::<Vec<_>>();
        for route in &routes {
            last_consumer[route.source_slot] =
                last_consumer[route.source_slot].max(route.destination_slot);
        }
        let scratch_slot_count = assign_liveness_scratch(&mut nodes, &last_consumer);
        let automation_curves = nodes
            .iter()
            .flat_map(|node| node.fader_automation.iter())
            .chain(contributions.iter().flat_map(|contribution| {
                contribution
                    .scope_input_automation
                    .iter()
                    .chain(contribution.volume_automation.iter())
                    .chain(contribution.pan_automation.iter())
            }))
            .chain(routes.iter().flat_map(|route| route.gain_automation.iter()))
            .chain(processors.iter().flat_map(|processor| processor.parameter_curves.iter()))
            .collect::<Vec<_>>();
        let processor_parameter_lane_count =
            processors.iter().map(|processor| processor.parameter_curves.len()).sum();
        let maximum_parameter_events_per_block = processors
            .iter()
            .map(|processor| {
                processor.parameter_curves.iter().try_fold(0_usize, |count, parameter| {
                    count.checked_add(if parameter.is_constant() {
                        1
                    } else {
                        contract.max_block_frames
                    })
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(
                    "processor parameter event capacity overflowed".to_owned(),
                )
            })?
            .into_iter()
            .max()
            .unwrap_or(0);
        let processor_session_scratch_bytes = processors
            .iter()
            .try_fold(0_usize, |bytes, processor| {
                bytes.checked_add(processor.factory.contract().session_scratch_bytes())
            })
            .ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(
                    "processor Session scratch capacity overflowed".to_owned(),
                )
            })?;
        if processor_session_scratch_bytes > contract.processor_session_scratch_budget_bytes {
            return Err(AudioCompileError::ProcessorScratchBudgetExceeded {
                required_bytes: processor_session_scratch_bytes,
                budget_bytes: contract.processor_session_scratch_budget_bytes,
            });
        }
        if latency.output_algorithmic_latency_frames
            > contract.public_output_lookahead_budget_frames
        {
            return Err(AudioCompileError::PublicOutputLookaheadBudgetExceeded {
                required_frames: latency.output_algorithmic_latency_frames,
                budget_frames: contract.public_output_lookahead_budget_frames,
            });
        }
        let compensation_delay_samples = latency
            .contribution_compensation_frames
            .iter()
            .chain(&latency.route_compensation_frames)
            .try_fold(0_usize, |samples, frames| {
                frames
                    .checked_mul(contract.channel_count())
                    .and_then(|delay_samples| samples.checked_add(delay_samples))
            })
            .ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(
                    "compensation delay storage overflowed".to_owned(),
                )
            })?;
        let compensation_delay_scratch_bytes = compensation_delay_samples
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(
                    "compensation delay byte extent overflowed".to_owned(),
                )
            })?;
        if compensation_delay_scratch_bytes > contract.compensation_delay_scratch_budget_bytes {
            return Err(AudioCompileError::CompensationScratchBudgetExceeded {
                required_bytes: compensation_delay_scratch_bytes,
                budget_bytes: contract.compensation_delay_scratch_budget_bytes,
            });
        }
        let summary = PreparedAudioScheduleSummary {
            node_count: nodes.len(),
            track_count,
            bus_count,
            contribution_count: contributions.len(),
            maximum_source_channels: contributions
                .iter()
                .map(|contribution| contribution.channel_mixer.source_layout().channel_count())
                .max()
                .unwrap_or(contract.channel_count()),
            channel_mix_coefficient_count: contributions
                .iter()
                .map(|contribution| contribution.channel_mixer.coefficient_count())
                .sum(),
            route_count: routes.len(),
            transition_binding_count: transitions.len(),
            automation_curve_count: automation_curves.len(),
            automation_event_span_count: automation_curves
                .iter()
                .map(|curve| curve.event_span_count())
                .sum(),
            processor_occurrence_count: processors.len(),
            processor_parameter_lane_count,
            maximum_parameter_events_per_block,
            processor_session_scratch_bytes,
            scratch_slot_count,
            compensation_delay_samples,
            public_output_lookahead_frames: latency.output_algorithmic_latency_frames,
            maximum_compensation_frames: latency.maximum_compensation_frames,
            requires_state_entry: latency.output_algorithmic_latency_frames > 0
                || latency.maximum_compensation_frames > 0
                || contributions.iter().any(|contribution| {
                    contribution.source_requires_state_entry
                        || contribution.scope_rack.requires_state_entry
                })
                || nodes.iter().any(|node| {
                    node.pre_rack.requires_state_entry || node.post_rack.requires_state_entry
                }),
        };
        Ok(Self {
            nodes,
            routes,
            contributions,
            processors,
            scopes,
            transitions,
            output_slot,
            summary,
        })
    }
}

fn prepare_rack(
    rack: &CompiledRack,
    owner: AudioProcessorOccurrenceOwner,
    insertion: AudioProcessorInsertionPoint,
    owner_time_offset: TimelineTime,
    sample_rate: AudioSampleRate,
    contract: AudioRenderContract,
    processor_resolver: &dyn AudioProcessorResolver,
    destination: &mut Vec<PreparedProcessor>,
) -> Result<PreparedRack, AudioCompileError> {
    let start = destination.len();
    let mut algorithmic_latency_frames = 0_usize;
    for processor in &rack.processors {
        let occurrence = AudioProcessorOccurrence {
            instance_id: processor.instance_id,
            owner,
            insertion,
        };
        let factory =
            prepare_processor_factory(processor_resolver, processor, occurrence, contract)?;
        let processor_algorithmic_latency_frames = factory.contract().algorithmic_latency_frames();
        let mut parameter_ids = Vec::with_capacity(processor.parameters.len());
        let mut parameter_curves = Vec::with_capacity(processor.parameters.len());
        for (parameter_id, parameter) in &processor.parameters {
            if parameter_id != &parameter.schema.parameter_id
                || parameter_id != &parameter.automation.parameter_id
            {
                return Err(AudioCompileError::InvalidPreparedGraph(format!(
                    "processor {} parameter identity drifted during preparation",
                    processor.instance_id
                )));
            }
            parameter_ids.push(parameter_id.clone());
            parameter_curves.push(PreparedAutomationCurve::build(
                &parameter.automation,
                owner_time_offset,
                sample_rate,
            )?);
        }
        destination.push(PreparedProcessor {
            occurrence,
            parameter_ids,
            parameter_curves,
            rack_prefix_algorithmic_latency_frames: algorithmic_latency_frames,
            input_signal_delay_frames: 0,
            factory,
        });
        algorithmic_latency_frames = algorithmic_latency_frames
            .checked_add(processor_algorithmic_latency_frames)
            .ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(
                    "processor rack algorithmic latency overflowed".to_owned(),
                )
            })?;
    }
    let prepared = &destination[start..];
    let tail = prepared.iter().try_fold(AudioProcessorTail::None, |accumulated, processor| {
        accumulate_sequential_tail(accumulated, processor.factory.contract().tail())
    })?;
    Ok(PreparedRack {
        processors: start..destination.len(),
        algorithmic_latency_frames,
        tail,
        requires_state_entry: prepared
            .iter()
            .any(|processor| processor.factory.contract().requires_state_entry()),
    })
}

fn assign_rack_processor_signal_delays(
    processors: &mut [PreparedProcessor],
    rack: &PreparedRack,
    rack_input_signal_delay_frames: usize,
) -> Result<(), AudioCompileError> {
    let rack_processors = processors.get_mut(rack.processors.clone()).ok_or_else(|| {
        AudioCompileError::InvalidPreparedGraph(
            "processor Rack range is outside the prepared schedule".to_owned(),
        )
    })?;
    for processor in rack_processors {
        processor.input_signal_delay_frames = rack_input_signal_delay_frames
            .checked_add(processor.rack_prefix_algorithmic_latency_frames)
            .ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(
                    "processor input signal delay overflowed".to_owned(),
                )
            })?;
    }
    Ok(())
}

fn accumulate_sequential_tail(
    accumulated: AudioProcessorTail,
    next: AudioProcessorTail,
) -> Result<AudioProcessorTail, AudioCompileError> {
    match (accumulated, next) {
        (AudioProcessorTail::Infinite, _) | (_, AudioProcessorTail::Infinite) => {
            Ok(AudioProcessorTail::Infinite)
        }
        (AudioProcessorTail::None, tail) | (tail, AudioProcessorTail::None) => Ok(tail),
        (AudioProcessorTail::Finite(left), AudioProcessorTail::Finite(right)) => {
            left.checked_add(right).map(AudioProcessorTail::Finite).ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(
                    "processor rack tail extent overflowed".to_owned(),
                )
            })
        }
    }
}

fn prepare_optional_curve(
    curve: Option<&ExactAutomationCurve>,
    owner_time_offset: TimelineTime,
    sample_rate: AudioSampleRate,
) -> Result<Option<PreparedAutomationCurve>, AudioCompileError> {
    curve
        .map(|curve| PreparedAutomationCurve::build(curve, owner_time_offset, sample_rate))
        .transpose()
}

fn prepared_contribution_constant_edit_gain_pan(
    contribution: &CompiledAudioContribution,
    has_no_transitions: bool,
) -> Option<(f32, f64)> {
    if contribution.fade_in.is_some() || contribution.fade_out.is_some() || !has_no_transitions {
        return None;
    }
    let volume_db = constant_curve_value(
        contribution.volume_automation.as_ref(),
        contribution.volume_db,
    )?;
    let pan = constant_curve_value(contribution.pan_automation.as_ref(), contribution.pan)?
        .clamp(-1.0, 1.0);
    Some((crate::dsp::db_to_linear(volume_db), pan))
}

fn constant_curve_value(curve: Option<&ExactAutomationCurve>, fallback: f64) -> Option<f64> {
    match curve {
        None => Some(fallback),
        Some(curve) if curve.keyframes.is_empty() => Some(curve.default_value),
        Some(curve) if curve.keyframes.len() == 1 => Some(curve.keyframes[0].value),
        Some(_) => None,
    }
}

fn append_transition_bindings(
    destination: &mut Vec<PreparedTransitionBinding>,
    transitions: &[CompiledTransition],
    contribution: &CompiledAudioContribution,
) {
    for transition in transitions {
        let direction = if transition.left == contribution.edit_id {
            Some(PreparedTransitionDirection::Falling)
        } else if transition.right == contribution.edit_id {
            Some(PreparedTransitionDirection::Rising)
        } else {
            None
        };
        if let Some(direction) = direction {
            destination.push(PreparedTransitionBinding {
                sequence_range: transition.sequence_range,
                curve: transition.curve,
                direction,
            });
        }
    }
}

fn assign_liveness_scratch(nodes: &mut [PreparedNode], last_consumer: &[usize]) -> usize {
    let mut scratch_live_until = Vec::<usize>::new();
    for (node_slot, node) in nodes.iter_mut().enumerate() {
        let reusable = scratch_live_until.iter().position(|live_until| *live_until < node_slot);
        let scratch_slot = reusable.unwrap_or_else(|| {
            scratch_live_until.push(0);
            scratch_live_until.len() - 1
        });
        scratch_live_until[scratch_slot] = last_consumer[node_slot];
        node.scratch_slot = scratch_slot;
    }
    scratch_live_until.len()
}
