//! Render-contract-specific lowering from semantic audio IR to dense execution data.

use crate::latency::{solve_prepared_latency, PreparedLatencyNodeInput, PreparedNodeLatency};
use crate::plan::{
    AudioRenderContract, CompiledAudioContribution, CompiledAudioProgram, CompiledChannelStrip,
    CompiledProcessingScope, CompiledProcessorOperation, CompiledRack, CompiledTransition,
};
use crate::AudioCompileError;
use mondrian_core::{
    AudioComponentEditId, AudioProcessingScopeId, AudioProcessorInstanceId, AudioSamplePosition,
    AudioSampleRate, AudioSampleRounding, ExactAutomationCurve, ExactAutomationSegment, MixBusId,
    ParameterId, ProgramOutputId, TimelineTime, TrackId,
};
use mondrian_timeline::audio::{
    AudioChannelStripOutputPort, AudioRouteDestination, AudioRouteSource, AudioTransitionCurve,
};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

/// Instance-specific facts supplied only after child render plans are prepared.
///
/// Semantic compilation deliberately cannot guess these values because plugin
/// realization and the selected Render Contract may change processor latency.
#[derive(Debug, Clone, Default)]
pub(crate) struct AudioPreparationDependencies {
    nested_sources: BTreeMap<mondrian_core::AudioComponentEditId, PreparedNestedSource>,
}

#[derive(Debug, Clone, Copy)]
struct PreparedNestedSource {
    latency_frames: usize,
    requires_state_entry: bool,
}

impl AudioPreparationDependencies {
    pub(crate) fn insert_nested_source(
        &mut self,
        edit_id: mondrian_core::AudioComponentEditId,
        latency_frames: usize,
        requires_state_entry: bool,
    ) -> Result<(), AudioCompileError> {
        if self
            .nested_sources
            .insert(
                edit_id,
                PreparedNestedSource { latency_frames, requires_state_entry },
            )
            .is_some()
        {
            return Err(AudioCompileError::InvalidPreparedGraph(format!(
                "duplicate nested latency dependency for contribution {edit_id}"
            )));
        }
        Ok(())
    }

    fn nested_source(
        &self,
        contribution: &CompiledAudioContribution,
    ) -> Result<Option<PreparedNestedSource>, AudioCompileError> {
        match contribution.source {
            crate::CompiledAudioSource::Media { .. } => Ok(None),
            crate::CompiledAudioSource::NestedOutput { .. } => self
                .nested_sources
                .get(&contribution.edit_id)
                .copied()
                .map(Some)
                .ok_or_else(|| {
                    AudioCompileError::InvalidPreparedGraph(format!(
                        "nested contribution {} has no prepared child-output latency",
                        contribution.edit_id
                    ))
                }),
        }
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
    /// Scratch slots after interval-liveness reuse.
    pub scratch_slot_count: usize,
    /// Total intrinsic/PDC latency at the selected Program Output.
    pub output_latency_frames: usize,
    /// Largest compensation delay inserted at any prepared summing input.
    pub maximum_compensation_frames: usize,
    /// Whether mutable DSP history requires explicit continuity entry.
    pub requires_state_entry: bool,
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
        Self::prepare_with_dependencies(
            program,
            contract,
            kernel_backend,
            &AudioPreparationDependencies::default(),
        )
    }

    pub(crate) fn prepare_with_dependencies(
        program: Arc<CompiledAudioProgram>,
        contract: AudioRenderContract,
        kernel_backend: AudioKernelBackend,
        dependencies: &AudioPreparationDependencies,
    ) -> Result<Self, AudioCompileError> {
        if contract.sample_rate == 0
            || contract.max_block_frames == 0
            || u32::try_from(contract.max_block_frames).is_err()
        {
            return Err(AudioCompileError::InvalidRenderContract);
        }
        let schedule = PreparedAudioSchedule::build(program.as_ref(), contract, dependencies)?;
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

    /// Total prepared Program Output latency on this plan's Evaluation Grid.
    pub fn output_latency_frames(&self) -> usize {
        self.schedule.summary.output_latency_frames
    }

    /// Whether Sessions must enter a continuity epoch before rendering.
    pub fn requires_state_entry(&self) -> bool {
        self.schedule.summary.requires_state_entry
    }
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

#[derive(Debug, Clone, Copy)]
pub(crate) struct PreparedRoute {
    pub(crate) source_slot: usize,
    pub(crate) destination_slot: usize,
    pub(crate) source_port: AudioChannelStripOutputPort,
    pub(crate) compensation_delay_frames: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedContribution {
    pub(crate) semantic: CompiledAudioContribution,
    pub(crate) track_slot: usize,
    pub(crate) scope_slot: usize,
    pub(crate) transitions: Range<usize>,
    pub(crate) sequence_start_sample: i64,
    pub(crate) sequence_end_sample: i64,
    pub(crate) constant_scope_gain: Option<f32>,
    pub(crate) constant_edit_gain_pan: Option<(f32, f64)>,
    pub(crate) scope_input_automation: Option<PreparedAutomationCurve>,
    pub(crate) scope_rack: PreparedRack,
    pub(crate) volume_automation: Option<PreparedAutomationCurve>,
    pub(crate) pan_automation: Option<PreparedAutomationCurve>,
    pub(crate) source_latency_frames: usize,
    pub(crate) source_requires_state_entry: bool,
    pub(crate) compensation_delay_frames: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedRack {
    pub(crate) processors: Range<usize>,
    pub(crate) latency_frames: usize,
    pub(crate) requires_state_entry: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedProcessor {
    pub(crate) origin: PreparedProcessorOrigin,
    pub(crate) parameter_ids: Vec<ParameterId>,
    pub(crate) parameter_curves: Vec<PreparedAutomationCurve>,
    pub(crate) operation: PreparedProcessorOperation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparedProcessorOperation {
    Gain { parameter_slot: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreparedProcessorOrigin {
    pub(crate) instance_id: AudioProcessorInstanceId,
    pub(crate) owner: PreparedProcessorOwner,
    pub(crate) insertion: PreparedProcessorInsertion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparedProcessorOwner {
    Contribution {
        edit_id: AudioComponentEditId,
        scope_id: AudioProcessingScopeId,
    },
    Node(PreparedNodeOrigin),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparedProcessorInsertion {
    Scope,
    PreFader,
    PostFader,
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
            let origin = PreparedProcessorOwner::Node(PreparedNodeOrigin::Track(*track_id));
            let pre_rack = prepare_rack(
                &channel.strip.pre_fader,
                origin,
                PreparedProcessorInsertion::PreFader,
                TimelineTime::ZERO,
                sample_rate,
                &mut processors,
            )?;
            let post_rack = prepare_rack(
                &channel.strip.post_fader,
                origin,
                PreparedProcessorInsertion::PostFader,
                TimelineTime::ZERO,
                sample_rate,
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
            let origin = PreparedProcessorOwner::Node(PreparedNodeOrigin::Bus(*bus_id));
            let pre_rack = prepare_rack(
                &strip.pre_fader,
                origin,
                PreparedProcessorInsertion::PreFader,
                TimelineTime::ZERO,
                sample_rate,
                &mut processors,
            )?;
            let post_rack = prepare_rack(
                &strip.post_fader,
                origin,
                PreparedProcessorInsertion::PostFader,
                TimelineTime::ZERO,
                sample_rate,
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
        let origin = PreparedProcessorOwner::Node(PreparedNodeOrigin::Output(program.output_id));
        let pre_rack = prepare_rack(
            &program.output.pre_fader,
            origin,
            PreparedProcessorInsertion::PreFader,
            TimelineTime::ZERO,
            sample_rate,
            &mut processors,
        )?;
        let post_rack = prepare_rack(
            &program.output.post_fader,
            origin,
            PreparedProcessorInsertion::PostFader,
            TimelineTime::ZERO,
            sample_rate,
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
                PreparedProcessorOwner::Contribution {
                    edit_id: semantic.edit_id,
                    scope_id: scopes[scope_slot].id,
                },
                PreparedProcessorInsertion::Scope,
                scope_time_offset,
                sample_rate,
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
            let nested_source = dependencies.nested_source(&semantic)?;
            contributions.push(PreparedContribution {
                source_latency_frames: nested_source.map_or(0, |source| source.latency_frames),
                source_requires_state_entry: nested_source
                    .is_some_and(|source| source.requires_state_entry),
                compensation_delay_frames: 0,
                semantic,
                track_slot,
                scope_slot,
                transitions: transition_start..transition_end,
                sequence_start_sample,
                sequence_end_sample,
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
                    routes.push(PreparedRoute {
                        source_slot,
                        destination_slot,
                        source_port,
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
                    pre_rack_latency_frames: node.pre_rack.latency_frames,
                    post_rack_latency_frames: node.post_rack.latency_frames,
                })
            })
            .collect::<Result<Vec<_>, AudioCompileError>>()?;
        let contribution_intrinsic_latency_frames = contributions
            .iter()
            .map(|contribution| {
                contribution
                    .source_latency_frames
                    .checked_add(contribution.scope_rack.latency_frames)
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
        let summary = PreparedAudioScheduleSummary {
            node_count: nodes.len(),
            track_count,
            bus_count,
            contribution_count: contributions.len(),
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
            scratch_slot_count,
            output_latency_frames: latency.output_latency_frames,
            maximum_compensation_frames: latency.maximum_compensation_frames,
            requires_state_entry: latency.maximum_compensation_frames > 0
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
    owner: PreparedProcessorOwner,
    insertion: PreparedProcessorInsertion,
    owner_time_offset: TimelineTime,
    sample_rate: AudioSampleRate,
    destination: &mut Vec<PreparedProcessor>,
) -> Result<PreparedRack, AudioCompileError> {
    let start = destination.len();
    for processor in &rack.processors {
        match &processor.operation {
            CompiledProcessorOperation::Gain { parameter_id, automation } => {
                if parameter_id != &automation.parameter_id {
                    return Err(AudioCompileError::InvalidPreparedGraph(format!(
                        "processor {} parameter identity drifted during preparation",
                        processor.instance_id
                    )));
                }
                destination.push(PreparedProcessor {
                    origin: PreparedProcessorOrigin {
                        instance_id: processor.instance_id,
                        owner,
                        insertion,
                    },
                    parameter_ids: vec![parameter_id.clone()],
                    parameter_curves: vec![PreparedAutomationCurve::build(
                        automation,
                        owner_time_offset,
                        sample_rate,
                    )?],
                    operation: PreparedProcessorOperation::Gain { parameter_slot: 0 },
                });
            }
        }
    }
    let latency_frames = rack.latency_frames().ok_or_else(|| {
        AudioCompileError::InvalidPreparedGraph("processor rack latency overflowed".to_owned())
    })?;
    Ok(PreparedRack {
        processors: start..destination.len(),
        latency_frames,
        requires_state_entry: rack.requires_state_entry(),
    })
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
