//! Render-contract-specific lowering from semantic audio IR to dense execution data.

use crate::plan::{
    AudioRenderContract, CompiledAudioContribution, CompiledAudioProgram, CompiledChannelStrip,
    CompiledProcessingScope, CompiledProcessor, CompiledRack, CompiledTransition,
};
use crate::AudioCompileError;
use mondrian_core::{
    AudioSamplePosition, AudioSampleRate, AudioSampleRounding, ExactAutomationCurve,
    ExactAutomationSegment, MixBusId, ProgramOutputId, TimelineTime, TrackId,
};
use mondrian_timeline::audio::{
    AudioChannelStripOutputPort, AudioRouteDestination, AudioRouteSource, AudioTransitionCurve,
};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

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
    /// Validated non-constant automation curves lowered into the schedule.
    pub automation_curve_count: usize,
    /// Exact interpolation spans selected before Session execution.
    pub automation_event_span_count: usize,
    /// Scratch slots after interval-liveness reuse.
    pub scratch_slot_count: usize,
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
        if contract.sample_rate == 0 || contract.channels == 0 || contract.max_block_frames == 0 {
            return Err(AudioCompileError::InvalidRenderContract);
        }
        let schedule = PreparedAudioSchedule::build(program.as_ref(), contract)?;
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
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedAudioSchedule {
    pub(crate) nodes: Vec<PreparedNode>,
    pub(crate) routes: Vec<PreparedRoute>,
    pub(crate) contributions: Vec<PreparedContribution>,
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
    pub(crate) constant_pre_gain: Option<f32>,
    pub(crate) constant_post_gain: Option<f32>,
    pub(crate) pre_rack_automation: Vec<PreparedAutomationCurve>,
    pub(crate) fader_automation: Option<PreparedAutomationCurve>,
    pub(crate) post_rack_automation: Vec<PreparedAutomationCurve>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PreparedRoute {
    pub(crate) source_slot: usize,
    pub(crate) destination_slot: usize,
    pub(crate) source_port: AudioChannelStripOutputPort,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedContribution {
    pub(crate) semantic: CompiledAudioContribution,
    pub(crate) track_slot: usize,
    pub(crate) scope_slot: usize,
    pub(crate) transitions: Range<usize>,
    pub(crate) sequence_start_sample: i64,
    pub(crate) sequence_end_sample: i64,
    pub(crate) constant_gain_pan: Option<(f32, f64)>,
    pub(crate) scope_input_automation: Option<PreparedAutomationCurve>,
    pub(crate) scope_rack_automation: Vec<PreparedAutomationCurve>,
    pub(crate) volume_automation: Option<PreparedAutomationCurve>,
    pub(crate) pan_automation: Option<PreparedAutomationCurve>,
}

/// One author curve lowered into exact sample-grid event spans.
#[derive(Debug, Clone)]
pub(crate) struct PreparedAutomationCurve {
    owner_time_offset: TimelineTime,
    segments: Vec<PreparedAutomationSegment>,
    constant_value: f64,
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
        Ok(Self { owner_time_offset, segments, constant_value })
    }

    pub(crate) fn event_span_count(&self) -> usize {
        self.segments.len().saturating_add(1)
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
    ) -> Result<Self, AudioCompileError> {
        let sample_rate = AudioSampleRate::new(contract.sample_rate).map_err(|error| {
            AudioCompileError::InvalidPreparedGraph(format!(
                "invalid prepared Evaluation Grid: {error}"
            ))
        })?;
        let track_count = program.track_channels.len();
        let bus_count = program.bus_order.len();
        let mut nodes = Vec::with_capacity(track_count.saturating_add(bus_count).saturating_add(1));
        let mut track_slots = BTreeMap::new();
        let mut bus_slots = BTreeMap::new();

        for (track_id, channel) in &program.track_channels {
            let slot = nodes.len();
            track_slots.insert(*track_id, slot);
            let (constant_pre_gain, constant_post_gain) =
                prepared_strip_constant_gains(&channel.strip);
            let automation = prepared_strip_automation(&channel.strip, sample_rate)?;
            nodes.push(PreparedNode {
                origin: PreparedNodeOrigin::Track(*track_id),
                strip: channel.strip.clone(),
                muted: channel.muted,
                incoming: 0..0,
                contributions: 0..0,
                scratch_slot: 0,
                constant_pre_gain,
                constant_post_gain,
                pre_rack_automation: automation.pre_rack,
                fader_automation: automation.fader,
                post_rack_automation: automation.post_rack,
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
            let (constant_pre_gain, constant_post_gain) = prepared_strip_constant_gains(strip);
            let automation = prepared_strip_automation(strip, sample_rate)?;
            nodes.push(PreparedNode {
                origin: PreparedNodeOrigin::Bus(*bus_id),
                strip: strip.clone(),
                muted: false,
                incoming: 0..0,
                contributions: 0..0,
                scratch_slot: 0,
                constant_pre_gain,
                constant_post_gain,
                pre_rack_automation: automation.pre_rack,
                fader_automation: automation.fader,
                post_rack_automation: automation.post_rack,
            });
        }
        let output_slot = nodes.len();
        let (constant_pre_gain, constant_post_gain) =
            prepared_strip_constant_gains(&program.output);
        let automation = prepared_strip_automation(&program.output, sample_rate)?;
        nodes.push(PreparedNode {
            origin: PreparedNodeOrigin::Output(program.output_id),
            strip: program.output.clone(),
            muted: false,
            incoming: 0..0,
            contributions: 0..0,
            scratch_slot: 0,
            constant_pre_gain,
            constant_post_gain,
            pre_rack_automation: automation.pre_rack,
            fader_automation: automation.fader,
            post_rack_automation: automation.post_rack,
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
            let constant_gain_pan = prepared_contribution_constant_gain_pan(
                &semantic,
                &scopes[scope_slot],
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
            let scope_rack_automation =
                prepare_rack_automation(&scopes[scope_slot].rack, scope_time_offset, sample_rate)?;
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
            contributions.push(PreparedContribution {
                semantic,
                track_slot,
                scope_slot,
                transitions: transition_start..transition_end,
                sequence_start_sample,
                sequence_end_sample,
                constant_gain_pan,
                scope_input_automation,
                scope_rack_automation,
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
                    routes.push(PreparedRoute { source_slot, destination_slot, source_port });
                }
            }
            node.incoming = start..routes.len();
        }

        let mut last_consumer = (0..nodes.len()).collect::<Vec<_>>();
        for route in &routes {
            last_consumer[route.source_slot] =
                last_consumer[route.source_slot].max(route.destination_slot);
        }
        let scratch_slot_count = assign_liveness_scratch(&mut nodes, &last_consumer);
        let automation_curves = nodes
            .iter()
            .flat_map(|node| {
                node.pre_rack_automation
                    .iter()
                    .chain(node.fader_automation.iter())
                    .chain(node.post_rack_automation.iter())
            })
            .chain(contributions.iter().flat_map(|contribution| {
                contribution
                    .scope_input_automation
                    .iter()
                    .chain(contribution.scope_rack_automation.iter())
                    .chain(contribution.volume_automation.iter())
                    .chain(contribution.pan_automation.iter())
            }))
            .collect::<Vec<_>>();
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
            scratch_slot_count,
        };
        Ok(Self {
            nodes,
            routes,
            contributions,
            scopes,
            transitions,
            output_slot,
            summary,
        })
    }
}

struct PreparedStripAutomation {
    pre_rack: Vec<PreparedAutomationCurve>,
    fader: Option<PreparedAutomationCurve>,
    post_rack: Vec<PreparedAutomationCurve>,
}

fn prepared_strip_automation(
    strip: &CompiledChannelStrip,
    sample_rate: AudioSampleRate,
) -> Result<PreparedStripAutomation, AudioCompileError> {
    Ok(PreparedStripAutomation {
        pre_rack: prepare_rack_automation(&strip.pre_fader, TimelineTime::ZERO, sample_rate)?,
        fader: prepare_optional_curve(
            strip.fader_automation.as_ref(),
            TimelineTime::ZERO,
            sample_rate,
        )?,
        post_rack: prepare_rack_automation(&strip.post_fader, TimelineTime::ZERO, sample_rate)?,
    })
}

fn prepare_rack_automation(
    rack: &CompiledRack,
    owner_time_offset: TimelineTime,
    sample_rate: AudioSampleRate,
) -> Result<Vec<PreparedAutomationCurve>, AudioCompileError> {
    rack.processors
        .iter()
        .map(|processor| match processor {
            CompiledProcessor::Gain { automation } => automation,
        })
        .map(|curve| PreparedAutomationCurve::build(curve, owner_time_offset, sample_rate))
        .collect()
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

fn prepared_strip_constant_gains(strip: &CompiledChannelStrip) -> (Option<f32>, Option<f32>) {
    let pre = constant_rack_gain_db(&strip.pre_fader)
        .map(|rack_db| db_to_linear(strip.input_trim_db + rack_db));
    let fader = constant_curve_value(strip.fader_automation.as_ref(), strip.fader_db);
    let post = fader
        .zip(constant_rack_gain_db(&strip.post_fader))
        .map(|(fader_db, rack_db)| db_to_linear(fader_db + rack_db));
    (pre, post)
}

fn prepared_contribution_constant_gain_pan(
    contribution: &CompiledAudioContribution,
    scope: &CompiledProcessingScope,
    has_no_transitions: bool,
) -> Option<(f32, f64)> {
    if contribution.fade_in.is_some() || contribution.fade_out.is_some() || !has_no_transitions {
        return None;
    }
    let scope_db = constant_curve_value(scope.input_gain_automation.as_ref(), scope.input_gain_db)?
        + constant_rack_gain_db(&scope.rack)?;
    let volume_db = constant_curve_value(
        contribution.volume_automation.as_ref(),
        contribution.volume_db,
    )?;
    let pan = constant_curve_value(contribution.pan_automation.as_ref(), contribution.pan)?
        .clamp(-1.0, 1.0);
    Some((db_to_linear(scope_db + volume_db), pan))
}

fn constant_rack_gain_db(rack: &CompiledRack) -> Option<f64> {
    rack.processors.iter().try_fold(0.0, |sum, processor| match processor {
        CompiledProcessor::Gain { automation } => {
            Some(sum + constant_curve_value(Some(automation), 0.0)?)
        }
    })
}

fn constant_curve_value(curve: Option<&ExactAutomationCurve>, fallback: f64) -> Option<f64> {
    match curve {
        None => Some(fallback),
        Some(curve) if curve.keyframes.is_empty() => Some(curve.default_value),
        Some(curve) if curve.keyframes.len() == 1 => Some(curve.keyframes[0].value),
        Some(_) => None,
    }
}

fn db_to_linear(db: f64) -> f32 {
    10.0_f64.powf(db / 20.0) as f32
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
