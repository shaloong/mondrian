//! Render-contract-specific lowering from semantic audio IR to dense execution data.

mod host_output_fader;
mod native_author;
#[cfg(test)]
pub(crate) use prepared_notes::PreparedNoteTarget;
pub(crate) use prepared_notes::{PreparedNoteEvent, PreparedNotePhase, PreparedNoteScore};
mod prepared_notes;
pub(crate) use native_author::processor;
pub(crate) use prepared_notes::requested_note_lowering_peak_bytes;
use prepared_notes::ProjectedNoteScore;

use crate::latency::{solve_prepared_latency, PreparedLatencyGraph, PreparedNodeLatency};
use crate::plan::{
    AudioRenderContract, CompiledAudioContribution, CompiledAudioGraph, CompiledAudioProgram,
    CompiledChannelStrip, CompiledProcessingScope, CompiledRack, CompiledTransition,
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
    AudioChannelSignalStage, AudioChannelStripOutputPort, AudioComponentChannelMapping,
    AudioProcessorDefinitionRef, AudioRouteDestination, AudioRouteSource, AudioTransitionCurve,
};
use std::collections::{BTreeMap, BTreeSet};
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
    /// Track, Bus, and Output nodes in stable main-route storage order.
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
    /// Prepared processor auxiliary buses, including unconnected silent buses.
    pub processor_auxiliary_bus_count: usize,
    /// All additional outputs plus referenced raw Main snapshots.
    pub processor_output_buffer_count: usize,
    /// Largest auxiliary-bus set exposed by one processor occurrence.
    pub maximum_processor_auxiliary_buses: usize,
    /// Enabled typed sidechain bindings materialized per processor occurrence.
    pub sidechain_binding_count: usize,
    /// Unique Track/Bus ports snapshotted for downstream sidechain consumers.
    pub sidechain_source_count: usize,
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
    preparation_id: mondrian_core::AudioPreparedPlanId,
    program: Arc<CompiledAudioProgram>,
    contract: AudioRenderContract,
    kernel_backend: AudioKernelBackend,
    pub(crate) schedule: PreparedAudioSchedule,
}

impl PreparedAudioPlan {
    #[cfg(test)]
    pub(crate) fn with_owned_note_score_for_test(
        &self,
        program: Arc<CompiledAudioProgram>,
    ) -> Result<Self, AudioCompileError> {
        let mut scoreless = program.as_ref().clone();
        scoreless.note_lanes.clear();
        if &scoreless != self.program() || !self.program().note_lanes.is_empty() {
            return Err(AudioCompileError::InvalidPreparedGraph(
                "owned Note control changed the prepared base graph".to_owned(),
            ));
        }
        let rate = AudioSampleRate::new(self.contract.sample_rate)
            .map_err(|_| AudioCompileError::InvalidRenderContract)?;
        let notes = ProjectedNoteScore::project(&program.note_lanes, rate)?
            .resolve_targets(program.as_ref())?
            .bind(&self.schedule.processors, rate)?;
        if notes.is_empty() {
            return Err(AudioCompileError::InvalidPreparedGraph(
                "owned Note control needs a nonempty actual authored score".to_owned(),
            ));
        }
        let mut next = self.clone();
        next.preparation_id = mondrian_core::AudioPreparedPlanId::new();
        next.program = program;
        next.schedule.notes = notes;
        next.schedule.summary.requires_state_entry = true;
        Ok(next)
    }

    /// Visit actual factory bindings in this local prepared Plan without DSP or
    /// new instance creation. Shared factory Arc payloads keep one identity so
    /// the caller can deduplicate aliases in its existing local union.
    ///
    /// This observes only immutable factory/binding storage; Plan, schedule,
    /// program, Session and foreign process owners remain separate inventories.
    /// Arc/platform coverage is not silently accepted as a complete allocation.
    pub fn visit_factory_host_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            crate::AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), crate::AudioProcessorFactoryStorageError>,
    ) -> Result<(), crate::AudioProcessorFactoryStorageError> {
        for processor in &self.schedule.processors {
            processor.factory.visit_host_owned_storage(visitor)?;
        }
        Ok(())
    }

    /// Visit the actual immutable prepared Note Vec/String payload separately
    /// from factory owners. This does not inventory Session ledgers/reservations,
    /// enclosing Plan Arc overhead or grant executable Note capability.
    pub fn visit_note_host_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            crate::AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), crate::AudioProcessorFactoryStorageError>,
    ) -> Result<(), crate::AudioProcessorFactoryStorageError> {
        self.schedule.notes.visit_host_owned_storage(visitor)
    }

    pub(crate) fn visit_native_host_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            crate::AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), crate::AudioProcessorFactoryStorageError>,
    ) -> Result<(), crate::AudioProcessorFactoryStorageError> {
        self.visit_factory_host_owned_storage(visitor)?;
        self.schedule.visit_output_descriptor_storage(visitor)?;
        // Only the newly introduced authored endpoint payload is inventoried here;
        // this is not a claim to inventory all existing Program allocations.
        for source in self
            .program
            .routes
            .iter()
            .map(|route| &route.source)
            .chain(self.program.sidechain_routes.iter().map(|route| &route.source))
        {
            if let AudioRouteSource::ProcessorOutput {
                port: mondrian_timeline::AudioProcessorOutputPort::Auxiliary { bus_key },
                ..
            } = source
            {
                visitor(crate::AudioProcessorFactoryStorageBorrow::complete(
                    bus_key,
                )?)?;
            }
        }
        self.visit_note_host_owned_storage(visitor)
    }

    pub(crate) const fn preparation_id(&self) -> mondrian_core::AudioPreparedPlanId {
        self.preparation_id
    }

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
            None,
        )
    }

    pub(crate) fn prepare_with_dependencies(
        program: Arc<CompiledAudioProgram>,
        contract: AudioRenderContract,
        kernel_backend: AudioKernelBackend,
        dependencies: &AudioPreparationDependencies,
        processor_resolver: &dyn AudioProcessorResolver,
        native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Self, AudioCompileError> {
        contract.validate()?;
        program.graph().admit_musical_transport(program.musical_time_reference)?;
        crate::processor::musical_tempo(program.musical_time_reference)
            .map_err(|error| AudioCompileError::InvalidPreparedGraph(error.to_string()))?;
        // Preserve the accepted allocation-free endpoint/error precedence until
        // Note preparation has its actual preallocation grant and runtime gate.
        validate_selected_note_preparation(program.as_ref(), contract.sample_rate)?;
        let rate = AudioSampleRate::new(contract.sample_rate)
            .map_err(|_| AudioCompileError::InvalidRenderContract)?;
        let projected_notes = ProjectedNoteScore::project(&program.note_lanes, rate)?;
        // No runtime Note permission is granted by IR/port preparation. Keep
        // the original typed refusal before ANY factory until Root connects
        // whole-root admission, actual Hosted ledger and capacity charges.
        projected_notes.require_runtime_support()?;
        let requested_notes = projected_notes.resolve_targets(program.as_ref())?;
        let schedule = PreparedAudioSchedule::build(
            program.as_ref(),
            contract,
            dependencies,
            processor_resolver,
            requested_notes,
            native_child_settlement,
        )?;
        Ok(Self {
            preparation_id: mondrian_core::AudioPreparedPlanId::new(),
            program,
            contract,
            kernel_backend,
            schedule,
        })
    }

    // Re-run the canonical schedule/PDC solver over this sole unchanged Program.
    // The local resolver borrows existing immutable Factories, replacing exactly
    // one occurrence; it never touches a catalog, native process or author state.
    pub(crate) fn prepare_native_reprepare_plan(
        &self,
        index: usize,
        replacement: Arc<dyn crate::AudioProcessorFactory>,
    ) -> Result<Self, AudioCompileError> {
        struct Retained<'a> {
            old: &'a PreparedAudioSchedule,
            selected: usize,
            replacement: Arc<dyn crate::AudioProcessorFactory>,
        }
        impl AudioProcessorResolver for Retained<'_> {
            fn prepare(
                &self,
                request: crate::AudioProcessorPrepareRequest<'_>,
            ) -> Result<Arc<dyn crate::AudioProcessorFactory>, crate::AudioProcessorHostError>
            {
                let (index, old) = self
                    .old
                    .processors
                    .iter()
                    .enumerate()
                    .find(|(_, p)| p.occurrence == request.occurrence())
                    .ok_or_else(|| {
                        crate::AudioProcessorHostError::InvalidContract(
                            "restart occurrence is absent from the installed schedule".to_owned(),
                        )
                    })?;
                if index == self.selected {
                    Ok(Arc::clone(&self.replacement))
                } else {
                    Ok(old.factory.shared_factory())
                }
            }
        }
        if index >= self.schedule.processors.len() || !self.schedule.notes.is_empty() {
            return Err(AudioCompileError::InvalidPreparedGraph(
                "restart Plan is outside the fixed root domain".to_owned(),
            ));
        }
        let mut dependencies = AudioPreparationDependencies::default();
        for contribution in &self.schedule.contributions {
            let layout = contribution.channel_mixer.source_layout();
            dependencies.insert_source(
                contribution.semantic.edit_id,
                layout,
                resolve_channel_mapping(
                    &contribution.semantic.channel_mapping,
                    layout,
                    self.contract.channel_layout,
                )?,
                contribution.source_algorithmic_latency_frames,
                contribution.source_requires_state_entry,
            )?;
        }
        Self::prepare_with_dependencies(
            Arc::clone(&self.program),
            self.contract,
            self.kernel_backend,
            &dependencies,
            &Retained { old: &self.schedule, selected: index, replacement },
            None,
        )
    }

    // Caller retains the fully formed Plan before any of these fallible checks.
    pub(crate) fn validate_native_reprepare_plan(
        &self,
        next: &Self,
        index: usize,
    ) -> Result<(), AudioCompileError> {
        for (slot, (old, prepared)) in
            self.schedule.processors.iter().zip(&next.schedule.processors).enumerate()
        {
            if old.occurrence != prepared.occurrence
                || old.parameter_ids != prepared.parameter_ids
                || old.factory.auxiliary_inputs() != prepared.factory.auxiliary_inputs()
                || old.factory.auxiliary_outputs() != prepared.factory.auxiliary_outputs()
                || old.factory.note_inputs() != prepared.factory.note_inputs()
                || (slot != index && old.factory.contract() != prepared.factory.contract())
            {
                return Err(AudioCompileError::InvalidPreparedGraph(
                    "restart changed fixed processor schema or ports".to_owned(),
                ));
            }
        }
        if next.schedule.processors.len() != self.schedule.processors.len() {
            return Err(AudioCompileError::InvalidPreparedGraph(
                "restart changed processor topology".to_owned(),
            ));
        }
        Ok(())
    }

    // A policy bound before PDC allocation: original node topology bounds all
    // liveness slots; the original Render Contract bounds the delay-line bytes.
    // This can conservatively refuse a wide contract under a smaller Root grant.
    pub(crate) fn native_reprepare_session_bound(
        &self,
    ) -> Result<AudioSessionResourceFootprint, AudioResourceFootprintError> {
        let mut summary = self.schedule.summary;
        summary.scratch_slot_count = summary.node_count;
        summary.compensation_delay_samples =
            self.contract.compensation_delay_scratch_budget_bytes / std::mem::size_of::<f32>();
        let mut bound = session_resource_footprint(
            self.contract,
            summary,
            self.schedule.output_metadata_bytes()?,
        )?;
        bound.prepared_logical_bytes = Self::requested_base_logical_bytes(&self.program)
            .map_err(|_| AudioResourceFootprintError::ExtentOverflow)?;
        Ok(bound)
    }

    /// Read immutable author targets only; no allocation or native query.
    /// Include VST even without Some so its cold refusal precedes dependency IO.
    /// Other formats keep their original refusal family.
    pub(crate) fn has_native_note_targets(program: &CompiledAudioProgram) -> bool {
        program.note_lanes.iter().filter(|lane| !lane.notes.is_empty()).any(|lane| {
            program.track_channels.get(&lane.target.track_id).is_some_and(|channel| {
                channel
                    .strip
                    .pre_fader
                    .processors
                    .iter()
                    .chain(&channel.strip.post_fader.processors)
                    .any(|processor| {
                        processor.instance_id == lane.target.processor_id
                            && matches!(
                                &processor.definition,
                                AudioProcessorDefinitionRef::Clap { .. }
                                    | AudioProcessorDefinitionRef::Vst3 { .. }
                            )
                    })
            })
        })
    }

    /// Preflight every selected native Note target before dependency/factory IO.
    /// Runtime also requires direct root, no window/precompiled, and the original
    /// ledger receipt. This is not an execution permit or installed ownership.
    pub(crate) fn validate_admitted_root_note_preparation(
        program: &CompiledAudioProgram,
        contract: AudioRenderContract,
    ) -> Result<(), AudioCompileError> {
        contract.validate()?;
        validate_selected_note_endpoints(program, contract.sample_rate)?;
        let first =
            program.note_lanes.iter().find(|lane| !lane.notes.is_empty()).ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(
                    "admitted root Note preparation requires a nonempty score".to_owned(),
                )
            })?;
        if program.contributions().iter().any(|contribution| {
            matches!(
                contribution.source,
                crate::CompiledAudioSource::NestedOutput { .. }
            )
        }) {
            return Err(AudioCompileError::NoteExecutionNotSupported {
                lane_id: first.id,
                processor_id: first.target.processor_id,
            });
        }
        for lane in program.note_lanes.iter().filter(|lane| !lane.notes.is_empty()) {
            let target = program.track_channels.get(&lane.target.track_id).and_then(|channel| {
                let mut targets = channel
                    .strip
                    .pre_fader
                    .processors
                    .iter()
                    .chain(&channel.strip.post_fader.processors)
                    .filter(|processor| processor.instance_id == lane.target.processor_id);
                let target = targets.next()?;
                if targets.next().is_some() {
                    None
                } else {
                    Some(target)
                }
            });
            if !target.is_some_and(|processor| {
                prepared_notes::admits_note_format(
                    &processor.definition,
                    program.musical_time_reference,
                )
            }) {
                return Err(AudioCompileError::NoteExecutionNotSupported {
                    lane_id: lane.id,
                    processor_id: lane.target.processor_id,
                });
            }
        }
        Ok(())
    }

    /// Private direct-root consumer. Original public preparation keeps both
    /// Unsupported guards; only the existing ledger builder calls this route.
    pub(crate) fn prepare_admitted_root_notes(
        program: Arc<CompiledAudioProgram>,
        contract: AudioRenderContract,
        dependencies: &AudioPreparationDependencies,
        processor_resolver: &dyn AudioProcessorResolver,
        native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Self, AudioCompileError> {
        Self::validate_admitted_root_note_preparation(&program, contract)?;
        let rate = AudioSampleRate::new(contract.sample_rate)
            .map_err(|_| AudioCompileError::InvalidRenderContract)?;
        let requested_notes = ProjectedNoteScore::project(&program.note_lanes, rate)?
            .resolve_targets(program.as_ref())?;
        let schedule = PreparedAudioSchedule::build(
            program.as_ref(),
            contract,
            dependencies,
            processor_resolver,
            requested_notes,
            native_child_settlement,
        )?;
        Ok(Self {
            preparation_id: mondrian_core::AudioPreparedPlanId::new(),
            program,
            contract,
            kernel_backend: AudioKernelBackend::default(),
            schedule,
        })
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

    /// Read the existing non-Note logical policy before any graph preparation.
    ///
    /// Successful projection reads immutable author facts without allocation or
    /// factory IO. Sidechain rows are a pre-probe upper bound; existing native
    /// bus validation and final actual <= reserved verification remain required.
    /// This does not admit Notes execution or prove a physical allocation peak.
    pub(crate) fn requested_base_logical_bytes(
        program: &CompiledAudioProgram,
    ) -> Result<usize, AudioCompileError> {
        Self::requested_base_logical_bytes_for_graph(program.graph())
    }

    pub(crate) fn requested_base_logical_bytes_for_graph(
        graph: CompiledAudioGraph<'_>,
    ) -> Result<usize, AudioCompileError> {
        let shape = PreparedLogicalShape::requested_graph(graph)
            .map_err(|error| AudioCompileError::InvalidPreparedGraph(error.to_string()))?;
        prepared_logical_bytes_for_shape(shape)
            .map_err(|error| AudioCompileError::InvalidPreparedGraph(error.to_string()))
    }

    /// Calculate the allocation envelope before constructing a mutable Session.
    ///
    /// This is used by closure-wide admission so nested Sessions cannot each
    /// consume an independent copy of the per-Session Render Contract budget.
    pub fn session_resource_footprint(
        &self,
    ) -> Result<AudioSessionResourceFootprint, AudioResourceFootprintError> {
        let mut footprint = session_resource_footprint(
            self.contract,
            self.schedule.summary,
            self.schedule.output_metadata_bytes()?,
        )?;
        footprint.prepared_logical_bytes = footprint
            .prepared_logical_bytes
            .checked_add(route_output_key_bytes(self.program.graph())?)
            .ok_or(AudioResourceFootprintError::ExtentOverflow)?;
        footprint.prepared_logical_bytes = footprint
            .prepared_logical_bytes
            .checked_add(self.schedule.notes.storage_bytes()?)
            .ok_or(AudioResourceFootprintError::ExtentOverflow)?;
        footprint.prepared_logical_bytes = footprint
            .prepared_logical_bytes
            .checked_add(self.program.compiled_note_payload_bytes()?)
            .ok_or(AudioResourceFootprintError::ExtentOverflow)?;
        footprint.fixed_resident_bytes = footprint
            .fixed_resident_bytes
            .checked_add(
                crate::processor_host::PreparedProcessorHost::note_storage_bytes(&self.schedule)?,
            )
            .ok_or(AudioResourceFootprintError::ExtentOverflow)?;
        Ok(footprint)
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

fn validate_selected_note_preparation(
    program: &CompiledAudioProgram,
    sample_rate: u32,
) -> Result<(), AudioCompileError> {
    validate_selected_note_endpoints(program, sample_rate)?;
    if let Some(lane) = program.note_lanes.iter().find(|lane| !lane.notes.is_empty()) {
        return Err(AudioCompileError::NoteExecutionNotSupported {
            lane_id: lane.id,
            processor_id: lane.target.processor_id,
        });
    }
    Ok(())
}

fn validate_selected_note_endpoints(
    program: &CompiledAudioProgram,
    sample_rate: u32,
) -> Result<(), AudioCompileError> {
    let rate =
        AudioSampleRate::new(sample_rate).map_err(|_| AudioCompileError::InvalidRenderContract)?;
    let mut event_count = 0_usize;
    for lane in &program.note_lanes {
        let count = lane
            .notes
            .len()
            .checked_mul(2)
            .ok_or(AudioCompileError::NoteEventCountOverflow)?;
        event_count = event_count
            .checked_add(count)
            .ok_or(AudioCompileError::NoteEventCountOverflow)?;
        for note in &lane.notes {
            let project = |time| {
                AudioSamplePosition::from_timeline_time(time, rate, AudioSampleRounding::Ceil)
                    .map(|position| position.sample())
                    .map_err(|source| AudioCompileError::NoteSampleProjection {
                        lane_id: lane.id,
                        note_id: note.id,
                        source,
                    })
            };
            let start_sample = project(note.start)?;
            let end_sample = project(note.end)?;
            if end_sample <= start_sample {
                return Err(AudioCompileError::CollapsedNoteDuration {
                    lane_id: lane.id,
                    note_id: note.id,
                    start_sample,
                    end_sample,
                });
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PreparedLogicalShape {
    nodes: usize,
    routes: usize,
    sidechain_bindings: usize,
    contributions: usize,
    processors: usize,
    automation_curves: usize,
    automation_spans: usize,
    transition_bindings: usize,
    route_output_key_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
enum PreparedLogicalShapeError {
    #[error("requested prepared logical shape overflowed")]
    ExtentOverflow,
    #[error("requested prepared Bus is absent from semantic IR")]
    MissingBus,
    #[error("requested prepared Contribution Scope is absent")]
    MissingScope,
}

impl PreparedLogicalShape {
    fn from_summary(summary: PreparedAudioScheduleSummary) -> Self {
        Self {
            nodes: summary.node_count,
            routes: summary.route_count,
            sidechain_bindings: summary.sidechain_binding_count,
            contributions: summary.contribution_count,
            processors: summary.processor_occurrence_count,
            automation_curves: summary.automation_curve_count,
            automation_spans: summary.automation_event_span_count,
            transition_bindings: summary.transition_binding_count,
            route_output_key_bytes: 0,
        }
    }

    // No allocation, native/factory query, key copy, endpoint lowering, or
    // prepared_segments() occurs here. This is shape, not graph validation.
    #[cfg(test)]
    fn requested(program: &CompiledAudioProgram) -> Result<Self, PreparedLogicalShapeError> {
        Self::requested_graph(program.graph())
    }

    fn requested_graph(program: CompiledAudioGraph<'_>) -> Result<Self, PreparedLogicalShapeError> {
        let mut shape = Self {
            nodes: program
                .track_channels
                .len()
                .checked_add(program.bus_order.len())
                .and_then(|count| count.checked_add(1))
                .ok_or(PreparedLogicalShapeError::ExtentOverflow)?,
            routes: program.routes.len(),
            route_output_key_bytes: route_output_key_bytes(program)
                .map_err(|_| PreparedLogicalShapeError::ExtentOverflow)?,
            contributions: program.contributions.len(),
            ..Self::default()
        };
        for channel in program.track_channels.values() {
            shape.add_strip(&channel.strip, program)?;
        }
        // Actual build consumes bus_order, not all BTreeMap values.
        for bus_id in program.bus_order {
            let strip = program.buses.get(bus_id).ok_or(PreparedLogicalShapeError::MissingBus)?;
            shape.add_strip(strip, program)?;
        }
        shape.add_strip(program.output, program)?;
        for route in program.routes {
            shape.add_curve(route.gain_automation.as_ref())?;
        }
        for contribution in program.contributions {
            let scope = program
                .processing_scopes
                .get(&contribution.processing_scope)
                .ok_or(PreparedLogicalShapeError::MissingScope)?;
            // Shared author Scope != shared execution occurrence.
            shape.add_curve(scope.input_gain_automation.as_ref())?;
            shape.add_curve(contribution.volume_automation.as_ref())?;
            shape.add_curve(contribution.pan_automation.as_ref())?;
            shape.add_rack(&scope.rack, program)?;
            for transition in program.transitions {
                // Exactly append_transition_bindings' if/else-if consumer:
                // the equal left/right edit contributes one binding, not two.
                if transition.left == contribution.edit_id
                    || transition.right == contribution.edit_id
                {
                    Self::add(&mut shape.transition_bindings, 1)?;
                }
            }
        }
        Ok(shape)
    }

    fn add(value: &mut usize, delta: usize) -> Result<(), PreparedLogicalShapeError> {
        *value = value.checked_add(delta).ok_or(PreparedLogicalShapeError::ExtentOverflow)?;
        Ok(())
    }
    fn add_curve(
        &mut self,
        curve: Option<&ExactAutomationCurve>,
    ) -> Result<(), PreparedLogicalShapeError> {
        if let Some(curve) = curve {
            Self::add(&mut self.automation_curves, 1)?;
            // Core prepared_segments: windows(2), then prepared span_count + 1.
            Self::add(&mut self.automation_spans, curve.keyframes.len().max(1))?;
        }
        Ok(())
    }
    fn add_strip(
        &mut self,
        strip: &CompiledChannelStrip,
        program: CompiledAudioGraph<'_>,
    ) -> Result<(), PreparedLogicalShapeError> {
        self.add_curve(strip.fader_automation.as_ref())?;
        self.add_rack(&strip.pre_fader, program)?;
        self.add_rack(&strip.post_fader, program)
    }
    fn add_rack(
        &mut self,
        rack: &CompiledRack,
        program: CompiledAudioGraph<'_>,
    ) -> Result<(), PreparedLogicalShapeError> {
        Self::add(&mut self.processors, rack.processors.len())?;
        for processor in &rack.processors {
            for parameter in processor.parameters.values() {
                self.add_curve(parameter.automation.as_ref())?;
            }
            // Auxiliary contracts have unique bus keys. This upper bound adds
            // every processor-ID match once for the occurrence; different
            // occurrences can expose different buses, so actual <= requested.
            for route in program
                .sidechain_routes
                .iter()
                .filter(|route| route.processor_id == processor.instance_id)
            {
                Self::add(&mut self.sidechain_bindings, 1)?;
                self.add_curve(route.gain_automation.as_ref())?;
            }
        }
        Ok(())
    }
}

fn route_output_key_bytes(
    graph: CompiledAudioGraph<'_>,
) -> Result<usize, AudioResourceFootprintError> {
    graph
        .routes
        .iter()
        .map(|route| &route.source)
        .chain(graph.sidechain_routes.iter().map(|route| &route.source))
        .try_fold(0_usize, |bytes, source| {
            let additional = match source {
                AudioRouteSource::ProcessorOutput {
                    port: mondrian_timeline::AudioProcessorOutputPort::Auxiliary { bus_key },
                    ..
                } => bus_key.capacity(),
                _ => 0,
            };
            bytes.checked_add(additional).ok_or(AudioResourceFootprintError::ExtentOverflow)
        })
}

fn prepared_logical_bytes_for_shape(
    shape: PreparedLogicalShape,
) -> Result<usize, AudioResourceFootprintError> {
    // Prospective shape and actual prepared summary share this existing logical
    // policy. These weights are not allocator or native resource telemetry.
    const PREPARED_BASE_BYTES: usize = 4 * 1024;
    const PREPARED_NODE_BYTES: usize = 2 * 1024;
    const PREPARED_ROUTE_BYTES: usize = 512;
    const PREPARED_CONTRIBUTION_BYTES: usize = 4 * 1024;
    const PREPARED_PROCESSOR_BYTES: usize = 4 * 1024;
    const PREPARED_AUTOMATION_CURVE_BYTES: usize = 512;
    const PREPARED_AUTOMATION_SPAN_BYTES: usize = 64;
    const PREPARED_TRANSITION_BYTES: usize = 512;
    checked_sum([
        PREPARED_BASE_BYTES,
        checked_product([shape.nodes, PREPARED_NODE_BYTES])?,
        checked_product([shape.routes, PREPARED_ROUTE_BYTES])?,
        checked_product([shape.sidechain_bindings, PREPARED_ROUTE_BYTES])?,
        checked_product([shape.contributions, PREPARED_CONTRIBUTION_BYTES])?,
        checked_product([shape.processors, PREPARED_PROCESSOR_BYTES])?,
        checked_product([shape.automation_curves, PREPARED_AUTOMATION_CURVE_BYTES])?,
        checked_product([shape.automation_spans, PREPARED_AUTOMATION_SPAN_BYTES])?,
        checked_product([shape.transition_bindings, PREPARED_TRANSITION_BYTES])?,
        shape.route_output_key_bytes,
    ])
}

fn session_resource_footprint(
    contract: AudioRenderContract,
    summary: PreparedAudioScheduleSummary,
    output_metadata_bytes: usize,
) -> Result<AudioSessionResourceFootprint, AudioResourceFootprintError> {
    const PROCESSOR_INSTANCE_METADATA_BYTES: usize = 256;
    const METER_CHANNEL_STATE_BYTES: usize = 64;
    const SESSION_BASE_BYTES: usize = 4 * 1024;

    let prepared_logical_bytes =
        prepared_logical_bytes_for_shape(PreparedLogicalShape::from_summary(summary))?;

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
    let processor_auxiliary_pcm = checked_product([
        summary.maximum_processor_auxiliary_buses,
        block_samples,
        std::mem::size_of::<f32>(),
    ])?;
    let processor_sidechain_lanes = if summary.sidechain_binding_count == 0 {
        0
    } else {
        checked_sum([
            checked_product([block_samples, std::mem::size_of::<f32>()])?,
            checked_product([contract.max_block_frames, std::mem::size_of::<f64>()])?,
        ])?
    };
    let gain_parameter_lanes =
        checked_product([contract.max_block_frames, 2, std::mem::size_of::<f64>()])?;
    let render_scratch_bytes = checked_sum([
        node_buffers,
        checked_product([
            summary.sidechain_source_count,
            block_samples,
            std::mem::size_of::<f32>(),
        ])?,
        source_frames,
        source_pcm,
        reusable_pcm,
        processor_auxiliary_pcm,
        output_metadata_bytes,
        checked_product([
            summary.processor_output_buffer_count,
            std::mem::size_of::<Vec<f32>>(),
        ])?,
        checked_product([
            summary.processor_output_buffer_count,
            block_samples,
            std::mem::size_of::<f32>(),
        ])?,
        processor_sidechain_lanes,
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
        crate::render::native_child_settlement_slot_bytes(),
        render_scratch_bytes,
        summary.processor_session_scratch_bytes,
        compensation_delay_bytes,
        parameter_event_bytes,
        checked_product([
            summary.processor_occurrence_count,
            checked_sum([
                PROCESSOR_INSTANCE_METADATA_BYTES,
                crate::processor_host::native_reprepare::native_reprepare_seed_slot_bytes(),
            ])?,
        ])?,
        checked_product([summary.node_count, channels, METER_CHANNEL_STATE_BYTES])?,
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
    pub(crate) musical_time_reference: Option<mondrian_core::MusicalTimeReference>,
    pub(crate) notes: PreparedNoteScore,
    pub(crate) nodes: Vec<PreparedNode>,
    pub(crate) execution_order: Vec<(usize, AudioChannelSignalStage)>,
    pub(crate) routes: Vec<PreparedRoute>,
    pub(crate) contributions: Vec<PreparedContribution>,
    pub(crate) processors: Vec<PreparedProcessor>,
    pub(crate) processor_auxiliary_buses: Vec<PreparedProcessorAuxiliaryBus>,
    pub(crate) processor_output_buses: Vec<PreparedProcessorOutputBus>,
    pub(crate) sidechain_bindings: Vec<PreparedSidechainBinding>,
    pub(crate) sidechain_sources: Vec<PreparedSidechainSource>,
    pub(crate) scopes: Vec<CompiledProcessingScope>,
    pub(crate) transitions: Vec<PreparedTransitionBinding>,
    pub(crate) output_slot: usize,
    pub(crate) summary: PreparedAudioScheduleSummary,
}

struct PreparedAutomationSummary {
    curve_count: usize,
    span_count: usize,
    parameter_lane_count: usize,
    maximum_parameter_events: usize,
}

fn automation_summary(
    nodes: &[PreparedNode],
    contributions: &[PreparedContribution],
    routes: &[PreparedRoute],
    sidechains: &[PreparedSidechainBinding],
    processors: &[PreparedProcessor],
    contract: AudioRenderContract,
) -> Result<PreparedAutomationSummary, AudioCompileError> {
    let overflow = || {
        AudioCompileError::InvalidPreparedGraph(
            "prepared automation storage extent overflowed".to_owned(),
        )
    };
    let mut curves = nodes
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
        .chain(sidechains.iter().flat_map(|binding| binding.gain_automation.iter()))
        .chain(
            processors
                .iter()
                .flat_map(|processor| processor.parameter_curves.iter().flatten()),
        );
    let (curve_count, span_count) =
        curves.try_fold((0_usize, 0_usize), |(count, spans), curve| {
            Ok::<_, AudioCompileError>((
                count.checked_add(1).ok_or_else(overflow)?,
                spans.checked_add(curve.event_span_count()).ok_or_else(overflow)?,
            ))
        })?;
    let mut parameter_lane_count = 0_usize;
    let mut maximum_parameter_events = 0;
    for processor in processors {
        parameter_lane_count = parameter_lane_count
            .checked_add(processor.parameter_curves.len())
            .ok_or_else(overflow)?;
        let events =
            processor.parameter_curves.iter().flatten().try_fold(0_usize, |count, curve| {
                count
                    .checked_add(if curve.is_constant() {
                        1
                    } else {
                        contract.max_block_frames
                    })
                    .ok_or_else(overflow)
            })?;
        maximum_parameter_events = maximum_parameter_events.max(events);
    }
    Ok(PreparedAutomationSummary {
        curve_count,
        span_count,
        parameter_lane_count,
        maximum_parameter_events,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
    pub(crate) sidechain_source_buffers: Vec<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedRoute {
    pub(crate) source_slot: usize,
    pub(crate) destination_slot: usize,
    pub(crate) source_port: AudioChannelStripOutputPort,
    pub(crate) processor_output: Option<PreparedProcessorOutputSource>,
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
    pub(crate) parameter_curves: Vec<Option<PreparedAutomationCurve>>,
    pub(crate) owner_time_offset: TimelineTime,
    pub(crate) main_compensation_delay_frames: usize,
    pub(crate) input_signal_delay_frames: usize,
    pub(crate) auxiliary_buses: Range<usize>,
    pub(crate) output_buses: Range<usize>,
    pub(crate) main_output_buffer: Option<usize>,
    pub(crate) factory: PreparedProcessorFactoryBinding,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedProcessorAuxiliaryBus {
    pub(crate) bus_key: String,
    pub(crate) bindings: Range<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedSidechainBinding {
    pub(crate) source_buffer_slot: usize,
    pub(crate) constant_gain: Option<f32>,
    pub(crate) gain_automation: Option<PreparedAutomationCurve>,
    pub(crate) compensation_delay_frames: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreparedSidechainSource {
    pub(crate) node_slot: usize,
    pub(crate) port: AudioChannelStripOutputPort,
    pub(crate) processor_output: Option<PreparedProcessorOutputSource>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedProcessorOutputBus {
    pub(crate) bus_key: String,
    pub(crate) buffer_slot: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PreparedProcessorOutputSource {
    pub(crate) processor_slot: usize,
    pub(crate) buffer_slot: usize,
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
    fn visit_output_descriptor_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            crate::AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), crate::AudioProcessorFactoryStorageError>,
    ) -> Result<(), crate::AudioProcessorFactoryStorageError> {
        use crate::AudioProcessorFactoryStorageBorrow as Storage;
        visitor(Storage::complete(&self.processor_output_buses)?)?;
        for bus in &self.processor_output_buses {
            visitor(Storage::complete(&bus.bus_key)?)?;
        }
        Ok(())
    }

    // Query current owners, not cached byte counts: Vec/String Clone may shrink
    // spare capacity and native-author seeds can retain equal values in new storage.
    pub(crate) fn output_metadata_bytes(&self) -> Result<usize, AudioResourceFootprintError> {
        let mut bytes = 0_usize;
        let mut visitor = |owner: crate::AudioProcessorFactoryStorageBorrow<'_>| {
            bytes = bytes.checked_add(owner.layout().size()).ok_or(
                crate::AudioProcessorFactoryStorageError::Owner(
                    crate::NativeHostLedgerError::ExtentOverflow,
                ),
            )?;
            Ok(())
        };
        for processor in &self.processors {
            processor
                .factory
                .visit_auxiliary_output_storage(&mut visitor)
                .map_err(|_| AudioResourceFootprintError::ExtentOverflow)?;
        }
        self.visit_output_descriptor_storage(&mut visitor)
            .map_err(|_| AudioResourceFootprintError::ExtentOverflow)?;
        Ok(bytes)
    }

    fn build(
        program: &CompiledAudioProgram,
        contract: AudioRenderContract,
        dependencies: &AudioPreparationDependencies,
        processor_resolver: &dyn AudioProcessorResolver,
        requested_notes: prepared_notes::RequestedNoteScore,
        native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
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
                program.musical_time_reference,
                native_child_settlement,
                processor_resolver,
                &requested_notes,
                &mut processors,
            )?;
            let post_rack = prepare_rack(
                &channel.strip.post_fader,
                owner,
                AudioProcessorInsertionPoint::PostFader,
                TimelineTime::ZERO,
                sample_rate,
                contract,
                program.musical_time_reference,
                native_child_settlement,
                processor_resolver,
                &requested_notes,
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
                sidechain_source_buffers: Vec::new(),
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
                program.musical_time_reference,
                native_child_settlement,
                processor_resolver,
                &requested_notes,
                &mut processors,
            )?;
            let post_rack = prepare_rack(
                &strip.post_fader,
                owner,
                AudioProcessorInsertionPoint::PostFader,
                TimelineTime::ZERO,
                sample_rate,
                contract,
                program.musical_time_reference,
                native_child_settlement,
                processor_resolver,
                &requested_notes,
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
                sidechain_source_buffers: Vec::new(),
            });
        }
        let owner = AudioProcessorOccurrenceOwner::Output(program.output_id);
        let pre_rack = prepare_rack(
            &program.output.pre_fader,
            owner,
            AudioProcessorInsertionPoint::PreFader,
            TimelineTime::ZERO,
            sample_rate,
            contract,
            program.musical_time_reference,
            native_child_settlement,
            processor_resolver,
            &requested_notes,
            &mut processors,
        )?;
        let post_rack = prepare_rack(
            &program.output.post_fader,
            owner,
            AudioProcessorInsertionPoint::PostFader,
            TimelineTime::ZERO,
            sample_rate,
            contract,
            program.musical_time_reference,
            native_child_settlement,
            processor_resolver,
            &requested_notes,
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
            sidechain_source_buffers: Vec::new(),
        });

        let node_order = prepared_main_path_order(program, &nodes)?;
        let mut nodes_by_origin =
            nodes.into_iter().map(|node| (node.origin, node)).collect::<BTreeMap<_, _>>();
        nodes = node_order
            .into_iter()
            .map(|origin| {
                nodes_by_origin.remove(&origin).ok_or_else(|| {
                    AudioCompileError::InvalidPreparedGraph(
                        "prepared node order references a missing node".to_owned(),
                    )
                })
            })
            .collect::<Result<Vec<_>, AudioCompileError>>()?;
        if !nodes_by_origin.is_empty() {
            return Err(AudioCompileError::InvalidPreparedGraph(
                "prepared node order omitted a node".to_owned(),
            ));
        }
        track_slots.clear();
        bus_slots.clear();
        let mut output_slot = None;
        for (slot, node) in nodes.iter().enumerate() {
            match node.origin {
                PreparedNodeOrigin::Track(track_id) => {
                    track_slots.insert(track_id, slot);
                }
                PreparedNodeOrigin::Bus(bus_id) => {
                    bus_slots.insert(bus_id, slot);
                }
                PreparedNodeOrigin::Output(_) => output_slot = Some(slot),
            }
        }
        let output_slot = output_slot.ok_or_else(|| {
            AudioCompileError::InvalidPreparedGraph("prepared output node is missing".to_owned())
        })?;

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
                program.musical_time_reference,
                native_child_settlement,
                processor_resolver,
                &requested_notes,
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

        let mut processor_output_buses = Vec::new();
        let mut processor_output_buffer_count = 0_usize;
        for processor in &mut processors {
            let start = processor_output_buses.len();
            for bus in &processor.factory.auxiliary_outputs().buses {
                let buffer_slot = processor_output_buffer_count;
                processor_output_buffer_count =
                    processor_output_buffer_count.checked_add(1).ok_or_else(|| {
                        AudioCompileError::InvalidPreparedGraph(
                            "output buffer extent overflowed".to_owned(),
                        )
                    })?;
                processor_output_buses
                    .push(PreparedProcessorOutputBus { bus_key: bus.bus_key.clone(), buffer_slot });
            }
            processor.output_buses = start..processor_output_buses.len();
            if program.routes.iter().map(|route| &route.source).chain(program.sidechain_routes.iter().map(|route| &route.source)).any(|source| matches!(source,
                AudioRouteSource::ProcessorOutput { processor_id, port: mondrian_timeline::audio::AudioProcessorOutputPort::Main } if *processor_id == processor.occurrence.instance_id)) {
                processor.main_output_buffer = Some(processor_output_buffer_count);
                processor_output_buffer_count = processor_output_buffer_count.checked_add(1)
                    .ok_or_else(|| AudioCompileError::InvalidPreparedGraph("output buffer extent overflowed".to_owned()))?;
            }
        }
        // Resolve borrowed descriptors before mutating node incoming ranges.
        let source_bindings = program
            .routes
            .iter()
            .map(|route| {
                prepared_source(&route.source, &nodes, &processors, &processor_output_buses)
            })
            .collect::<Result<Vec<_>, _>>()?;
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
                for (binding_index, route) in program
                    .routes
                    .iter()
                    .enumerate()
                    .filter(|(_, route)| route.destination == destination)
                {
                    let source = source_bindings[binding_index];
                    let source_slot = source.node_slot;
                    let source_port = source.port;
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
                        processor_output: source.processor_output,
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

        let execution_order = prepared_execution_order(program, &nodes)?;
        let stage_positions = execution_order
            .iter()
            .enumerate()
            .map(|(position, stage)| (*stage, position))
            .collect::<BTreeMap<_, _>>();
        let mut processor_auxiliary_buses = Vec::new();
        let mut sidechain_bindings = Vec::new();
        let mut sidechain_sources = Vec::new();
        let mut sidechain_source_slots = BTreeMap::new();
        let mut matched_sidechain_routes = BTreeSet::new();
        for processor_index in 0..processors.len() {
            let processor = &processors[processor_index];
            let auxiliary_contract = processor.factory.auxiliary_inputs().clone();
            let auxiliary_start = processor_auxiliary_buses.len();
            for bus in auxiliary_contract.buses {
                let binding_start = sidechain_bindings.len();
                for route in program.sidechain_routes.iter().filter(|route| {
                    route.processor_id == processor.occurrence.instance_id
                        && route.bus_key == bus.bus_key
                }) {
                    let source = prepared_source(
                        &route.source,
                        &nodes,
                        &processors,
                        &processor_output_buses,
                    )?;
                    let source_slot = source.node_slot;
                    let source_port = source.port;
                    let target_slot = processor_target_node_slot(
                        processor,
                        &contributions,
                        &track_slots,
                        &bus_slots,
                        output_slot,
                    )?;
                    let target_stage = match processor.occurrence.insertion {
                        AudioProcessorInsertionPoint::Scope
                        | AudioProcessorInsertionPoint::PreFader => {
                            AudioChannelSignalStage::PreFader
                        }
                        AudioProcessorInsertionPoint::PostFader => {
                            AudioChannelSignalStage::PostFader
                        }
                    };
                    if stage_positions
                        [&(source_slot, AudioChannelSignalStage::producing(source_port))]
                        >= stage_positions[&(target_slot, target_stage)]
                    {
                        return Err(AudioCompileError::InvalidPreparedGraph(format!(
                            "sidechain Route {} forms a non-causal processor dependency",
                            route.id
                        )));
                    }
                    let source_buffer_slot = *sidechain_source_slots
                        .entry((source_slot, source_port, source.processor_output))
                        .or_insert_with(|| {
                            let slot = sidechain_sources.len();
                            sidechain_sources.push(PreparedSidechainSource {
                                node_slot: source_slot,
                                port: source_port,
                                processor_output: source.processor_output,
                            });
                            slot
                        });
                    let gain_automation = prepare_optional_curve(
                        route.gain_automation.as_ref(),
                        TimelineTime::ZERO,
                        sample_rate,
                    )?;
                    sidechain_bindings.push(PreparedSidechainBinding {
                        source_buffer_slot,
                        constant_gain: gain_automation
                            .is_none()
                            .then(|| crate::dsp::db_to_linear(route.gain_db)),
                        gain_automation,
                        compensation_delay_frames: 0,
                    });
                    matched_sidechain_routes.insert(route.id);
                }
                processor_auxiliary_buses.push(PreparedProcessorAuxiliaryBus {
                    bus_key: bus.bus_key,
                    bindings: binding_start..sidechain_bindings.len(),
                });
            }
            processors[processor_index].auxiliary_buses =
                auxiliary_start..processor_auxiliary_buses.len();
        }
        if matched_sidechain_routes.len() != program.sidechain_routes.len() {
            let missing = program
                .sidechain_routes
                .iter()
                .find(|route| !matched_sidechain_routes.contains(&route.id))
                .map(|route| route.id);
            return Err(AudioCompileError::InvalidPreparedGraph(format!(
                "sidechain Route {} targets an auxiliary bus absent from the realized processor contract",
                missing.map_or_else(|| "unknown".to_owned(), |id| id.to_string())
            )));
        }
        let latency = solve_prepared_latency(PreparedLatencyGraph {
            nodes: &mut nodes,
            routes: &mut routes,
            contributions: &mut contributions,
            processors: &mut processors,
            auxiliary_buses: &processor_auxiliary_buses,
            bindings: &mut sidechain_bindings,
            sources: &sidechain_sources,
            execution_order: &execution_order,
            output_slot,
        })
        .map_err(|reason| AudioCompileError::InvalidPreparedGraph(reason.to_owned()))?;
        for (contribution, frames) in
            contributions.iter_mut().zip(&latency.contribution_compensation_frames)
        {
            contribution.compensation_delay_frames = *frames;
        }
        for (route, frames) in routes.iter_mut().zip(&latency.route_compensation_frames) {
            route.compensation_delay_frames = *frames;
        }
        for (node, frames) in nodes.iter_mut().zip(&latency.node_latencies) {
            node.latency = *frames;
        }

        for (source_buffer_slot, source) in sidechain_sources.iter().enumerate() {
            nodes
                .get_mut(source.node_slot)
                .ok_or_else(|| {
                    AudioCompileError::InvalidPreparedGraph(
                        "sidechain source node is outside the prepared schedule".to_owned(),
                    )
                })?
                .sidechain_source_buffers
                .push(source_buffer_slot);
        }

        let mut last_consumer = (0..nodes.len())
            .map(|slot| stage_positions[&(slot, AudioChannelSignalStage::PostFader)])
            .collect::<Vec<_>>();
        for route in &routes {
            last_consumer[route.source_slot] = last_consumer[route.source_slot]
                .max(stage_positions[&(route.destination_slot, AudioChannelSignalStage::PreFader)]);
        }
        // The output remains live until the public output copy after all stages.
        last_consumer[output_slot] = execution_order.len();
        let scratch_slot_count =
            assign_liveness_scratch(&mut nodes, &last_consumer, &execution_order);
        let automation = automation_summary(
            &nodes,
            &contributions,
            &routes,
            &sidechain_bindings,
            &processors,
            contract,
        )?;
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
            .chain(processors.iter().map(|processor| &processor.main_compensation_delay_frames))
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
        let sidechain_compensation_delay_samples = sidechain_bindings
            .iter()
            .try_fold(0_usize, |samples, binding| {
                binding
                    .compensation_delay_frames
                    .checked_mul(contract.channel_count())
                    .and_then(|delay_samples| samples.checked_add(delay_samples))
            })
            .ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(
                    "sidechain compensation delay storage overflowed".to_owned(),
                )
            })?;
        let compensation_delay_samples = compensation_delay_samples
            .checked_add(sidechain_compensation_delay_samples)
            .ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(
                    "aggregate compensation delay storage overflowed".to_owned(),
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
        let notes = requested_notes.bind(&processors, sample_rate)?;
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
            processor_auxiliary_bus_count: processor_auxiliary_buses.len(),
            processor_output_buffer_count,
            maximum_processor_auxiliary_buses: processors
                .iter()
                .map(|processor| processor.auxiliary_buses.len())
                .max()
                .unwrap_or(0),
            sidechain_binding_count: sidechain_bindings.len(),
            sidechain_source_count: sidechain_sources.len(),
            transition_binding_count: transitions.len(),
            automation_curve_count: automation.curve_count,
            automation_event_span_count: automation.span_count,
            processor_occurrence_count: processors.len(),
            processor_parameter_lane_count: automation.parameter_lane_count,
            maximum_parameter_events_per_block: automation.maximum_parameter_events,
            processor_session_scratch_bytes,
            scratch_slot_count,
            compensation_delay_samples,
            public_output_lookahead_frames: latency.output_algorithmic_latency_frames,
            maximum_compensation_frames: sidechain_bindings
                .iter()
                .map(|binding| binding.compensation_delay_frames)
                .fold(latency.maximum_compensation_frames, usize::max),
            requires_state_entry: !notes.is_empty()
                || latency.output_algorithmic_latency_frames > 0
                || latency.maximum_compensation_frames > 0
                || sidechain_bindings.iter().any(|binding| binding.compensation_delay_frames > 0)
                || contributions.iter().any(|contribution| {
                    contribution.source_requires_state_entry
                        || contribution.scope_rack.requires_state_entry
                })
                || nodes.iter().any(|node| {
                    node.pre_rack.requires_state_entry || node.post_rack.requires_state_entry
                }),
        };
        Ok(Self {
            notes,
            musical_time_reference: program.musical_time_reference,
            nodes,
            execution_order,
            routes,
            contributions,
            processors,
            processor_auxiliary_buses,
            processor_output_buses,
            sidechain_bindings,
            sidechain_sources,
            scopes,
            transitions,
            output_slot,
            summary,
        })
    }
}

fn source_channel(
    source: &AudioRouteSource,
    nodes: &[PreparedNode],
) -> Result<(PreparedNodeOrigin, AudioChannelStripOutputPort), AudioCompileError> {
    match source {
        AudioRouteSource::Track { track_id, port } => {
            Ok((PreparedNodeOrigin::Track(*track_id), *port))
        }
        AudioRouteSource::Bus { bus_id, port } => Ok((PreparedNodeOrigin::Bus(*bus_id), *port)),
        AudioRouteSource::ProcessorOutput { processor_id, .. } => {
            for node in nodes
                .iter()
                .filter(|node| !matches!(node.origin, PreparedNodeOrigin::Output(_)))
            {
                for (rack, port) in [
                    (&node.strip.pre_fader, AudioChannelStripOutputPort::PreFader),
                    (
                        &node.strip.post_fader,
                        AudioChannelStripOutputPort::PostFaderPreMute,
                    ),
                ] {
                    if compiled_rack_contains_processor(rack, *processor_id) {
                        return Ok((node.origin, port));
                    }
                }
            }
            Err(AudioCompileError::InvalidPreparedGraph(format!(
                "processor output source {processor_id} is not an active Track/Bus rack occurrence"
            )))
        }
    }
}

fn prepared_source(
    source: &AudioRouteSource,
    nodes: &[PreparedNode],
    processors: &[PreparedProcessor],
    outputs: &[PreparedProcessorOutputBus],
) -> Result<PreparedSidechainSource, AudioCompileError> {
    let (origin, port) = source_channel(source, nodes)?;
    let invalid = || {
        AudioCompileError::InvalidPreparedGraph(
            "processor output key or occurrence is absent from the prepared contract".to_owned(),
        )
    };
    let node_slot = nodes.iter().position(|node| node.origin == origin).ok_or_else(invalid)?;
    let processor_output = if let AudioRouteSource::ProcessorOutput { processor_id, port } = source
    {
        let processor_slot = processors
            .iter()
            .position(|processor| {
                processor.occurrence.instance_id == *processor_id
                    && matches!(
                        processor.occurrence.owner,
                        AudioProcessorOccurrenceOwner::Track(_)
                            | AudioProcessorOccurrenceOwner::Bus(_)
                    )
            })
            .ok_or_else(invalid)?;
        let processor = &processors[processor_slot];
        let buffer_slot = match port {
            mondrian_timeline::audio::AudioProcessorOutputPort::Main => {
                processor.main_output_buffer.ok_or_else(invalid)?
            }
            mondrian_timeline::audio::AudioProcessorOutputPort::Auxiliary { bus_key } => {
                outputs
                    .get(processor.output_buses.clone())
                    .ok_or_else(invalid)?
                    .iter()
                    .find(|bus| bus.bus_key == *bus_key)
                    .ok_or_else(invalid)?
                    .buffer_slot
            }
        };
        Some(PreparedProcessorOutputSource { processor_slot, buffer_slot })
    } else {
        None
    };
    Ok(PreparedSidechainSource { node_slot, port, processor_output })
}

fn prepared_main_path_order(
    program: &CompiledAudioProgram,
    nodes: &[PreparedNode],
) -> Result<Vec<PreparedNodeOrigin>, AudioCompileError> {
    let origins = nodes.iter().map(|node| node.origin).collect::<BTreeSet<_>>();
    let mut outgoing = origins
        .iter()
        .copied()
        .map(|origin| (origin, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    let mut indegree = origins
        .iter()
        .copied()
        .map(|origin| (origin, 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut add_edge = |source: PreparedNodeOrigin,
                        destination: PreparedNodeOrigin|
     -> Result<(), AudioCompileError> {
        if !origins.contains(&source) || !origins.contains(&destination) {
            return Err(AudioCompileError::InvalidPreparedGraph(
                "routing dependency references a node outside the selected closure".to_owned(),
            ));
        }
        if outgoing.entry(source).or_default().insert(destination) {
            *indegree.get_mut(&destination).ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(
                    "routing dependency target is absent".to_owned(),
                )
            })? += 1;
        }
        Ok(())
    };
    for route in &program.routes {
        let (source, _) = source_channel(&route.source, nodes)?;
        let destination = match route.destination {
            AudioRouteDestination::Bus(bus_id) => PreparedNodeOrigin::Bus(bus_id),
            AudioRouteDestination::Output(output_id) => PreparedNodeOrigin::Output(output_id),
        };
        add_edge(source, destination)?;
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(origin, degree)| (*degree == 0).then_some(*origin))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(origins.len());
    while let Some(origin) = ready.pop_first() {
        order.push(origin);
        for destination in outgoing.get(&origin).into_iter().flatten() {
            let degree = indegree.get_mut(destination).ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(
                    "routing dependency target disappeared".to_owned(),
                )
            })?;
            *degree = degree.saturating_sub(1);
            if *degree == 0 {
                ready.insert(*destination);
            }
        }
    }
    if order.len() != origins.len() {
        return Err(AudioCompileError::InvalidPreparedGraph(
            "main routing contains an instantaneous cycle".to_owned(),
        ));
    }
    Ok(order)
}

fn prepared_execution_order(
    program: &CompiledAudioProgram,
    nodes: &[PreparedNode],
) -> Result<Vec<(usize, AudioChannelSignalStage)>, AudioCompileError> {
    let slots = nodes
        .iter()
        .enumerate()
        .map(|(slot, node)| (node.origin, slot))
        .collect::<BTreeMap<_, _>>();
    let mut outgoing = BTreeMap::<_, BTreeSet<_>>::new();
    for (slot, _) in nodes.iter().enumerate() {
        outgoing.insert(
            (slot, AudioChannelSignalStage::PreFader),
            BTreeSet::from([(slot, AudioChannelSignalStage::PostFader)]),
        );
        outgoing.insert((slot, AudioChannelSignalStage::PostFader), BTreeSet::new());
    }
    let source_stage = |source: &AudioRouteSource| {
        let (origin, port) = source_channel(source, nodes)?;
        slots
            .get(&origin)
            .copied()
            .map(|slot| (slot, AudioChannelSignalStage::producing(port)))
            .ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(
                    "source outside selected closure".to_owned(),
                )
            })
    };
    for route in &program.routes {
        let origin = match route.destination {
            AudioRouteDestination::Bus(id) => PreparedNodeOrigin::Bus(id),
            AudioRouteDestination::Output(id) => PreparedNodeOrigin::Output(id),
        };
        let slot = slots.get(&origin).copied().ok_or_else(|| {
            AudioCompileError::InvalidPreparedGraph(
                "destination outside selected closure".to_owned(),
            )
        })?;
        outgoing
            .entry(source_stage(&route.source)?)
            .or_default()
            .insert((slot, AudioChannelSignalStage::PreFader));
    }
    for route in &program.sidechain_routes {
        let mut targets = BTreeSet::new();
        for (slot, node) in nodes.iter().enumerate() {
            for (stage, rack) in [
                (AudioChannelSignalStage::PreFader, &node.strip.pre_fader),
                (AudioChannelSignalStage::PostFader, &node.strip.post_fader),
            ] {
                if compiled_rack_contains_processor(rack, route.processor_id) {
                    targets.insert((slot, stage));
                }
            }
        }
        for (scope_id, scope) in &program.processing_scopes {
            if compiled_rack_contains_processor(&scope.rack, route.processor_id) {
                for contribution in program
                    .contributions
                    .iter()
                    .filter(|contribution| contribution.processing_scope == *scope_id)
                {
                    let slot = slots
                        .get(&PreparedNodeOrigin::Track(contribution.track_id))
                        .copied()
                        .ok_or_else(|| {
                            AudioCompileError::InvalidPreparedGraph(
                                "Scope Track outside selected closure".to_owned(),
                            )
                        })?;
                    targets.insert((slot, AudioChannelSignalStage::PreFader));
                }
            }
        }
        if targets.is_empty() {
            return Err(AudioCompileError::InvalidPreparedGraph(
                "sidechain target outside selected closure".to_owned(),
            ));
        }
        outgoing.entry(source_stage(&route.source)?).or_default().extend(targets);
    }
    let mut indegree = outgoing
        .keys()
        .copied()
        .map(|stage| (stage, 0_usize))
        .collect::<BTreeMap<_, _>>();
    for target in outgoing.values().flatten() {
        *indegree.get_mut(target).ok_or_else(|| {
            AudioCompileError::InvalidPreparedGraph("missing stage".to_owned())
        })? += 1;
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(stage, degree)| (*degree == 0).then_some(*stage))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(outgoing.len());
    while let Some(stage) = ready.pop_first() {
        order.push(stage);
        for target in outgoing.get(&stage).into_iter().flatten() {
            let degree = indegree.get_mut(target).ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph("missing stage".to_owned())
            })?;
            *degree -= 1;
            if *degree == 0 {
                ready.insert(*target);
            }
        }
    }
    if order.len() != outgoing.len() {
        return Err(AudioCompileError::InvalidPreparedGraph(
            "main/sidechain stage dependency cycle".to_owned(),
        ));
    }
    Ok(order)
}

fn compiled_rack_contains_processor(
    rack: &CompiledRack,
    processor_id: mondrian_core::AudioProcessorInstanceId,
) -> bool {
    rack.processors.iter().any(|processor| processor.instance_id == processor_id)
}

fn processor_target_node_slot(
    processor: &PreparedProcessor,
    contributions: &[PreparedContribution],
    track_slots: &BTreeMap<TrackId, usize>,
    bus_slots: &BTreeMap<MixBusId, usize>,
    output_slot: usize,
) -> Result<usize, AudioCompileError> {
    match processor.occurrence.owner {
        AudioProcessorOccurrenceOwner::Contribution { edit_id, .. } => contributions
            .iter()
            .find(|contribution| contribution.semantic.edit_id == edit_id)
            .map(|contribution| contribution.track_slot)
            .ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(format!(
                    "processor {} targets a missing Contribution {edit_id}",
                    processor.occurrence.instance_id
                ))
            }),
        AudioProcessorOccurrenceOwner::Track(track_id) => {
            track_slots.get(&track_id).copied().ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(format!(
                    "processor {} targets a missing Track {track_id}",
                    processor.occurrence.instance_id
                ))
            })
        }
        AudioProcessorOccurrenceOwner::Bus(bus_id) => {
            bus_slots.get(&bus_id).copied().ok_or_else(|| {
                AudioCompileError::InvalidPreparedGraph(format!(
                    "processor {} targets a missing Bus {bus_id}",
                    processor.occurrence.instance_id
                ))
            })
        }
        AudioProcessorOccurrenceOwner::Output(_) => Ok(output_slot),
    }
}

fn prepare_rack(
    rack: &CompiledRack,
    owner: AudioProcessorOccurrenceOwner,
    insertion: AudioProcessorInsertionPoint,
    owner_time_offset: TimelineTime,
    sample_rate: AudioSampleRate,
    contract: AudioRenderContract,
    musical_time_reference: Option<mondrian_core::MusicalTimeReference>,
    native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    processor_resolver: &dyn AudioProcessorResolver,
    requested_notes: &prepared_notes::RequestedNoteScore,
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
        let factory = prepare_processor_factory(
            processor_resolver,
            processor,
            occurrence,
            contract,
            requested_notes.requests_input_for(occurrence),
            musical_time_reference,
            native_child_settlement,
        )?;
        let processor_algorithmic_latency_frames = factory.contract().algorithmic_latency_frames();
        let mut parameter_ids = Vec::with_capacity(processor.parameters.len());
        let mut parameter_curves = Vec::with_capacity(processor.parameters.len());
        for (parameter_id, parameter) in &processor.parameters {
            if parameter_id != &parameter.schema.parameter_id
                || parameter
                    .automation
                    .as_ref()
                    .is_some_and(|curve| parameter_id != &curve.parameter_id)
            {
                return Err(AudioCompileError::InvalidPreparedGraph(format!(
                    "processor {} parameter identity drifted during preparation",
                    processor.instance_id
                )));
            }
            parameter_ids.push(parameter_id.clone());
            parameter_curves.push(
                parameter
                    .automation
                    .as_ref()
                    .map(|curve| {
                        PreparedAutomationCurve::build(curve, owner_time_offset, sample_rate)
                    })
                    .transpose()?,
            );
        }
        destination.push(PreparedProcessor {
            occurrence,
            parameter_ids,
            parameter_curves,
            owner_time_offset,
            main_compensation_delay_frames: 0,
            input_signal_delay_frames: 0,
            auxiliary_buses: 0..0,
            output_buses: 0..0,
            main_output_buffer: None,
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

fn assign_liveness_scratch(
    nodes: &mut [PreparedNode],
    last_consumer: &[usize],
    execution_order: &[(usize, AudioChannelSignalStage)],
) -> usize {
    let mut scratch_live_until = Vec::<usize>::new();
    for (position, (node_slot, stage)) in execution_order.iter().copied().enumerate() {
        if stage != AudioChannelSignalStage::PreFader {
            continue;
        }
        let node = &mut nodes[node_slot];
        let reusable = scratch_live_until.iter().position(|live_until| *live_until < position);
        let scratch_slot = reusable.unwrap_or_else(|| {
            scratch_live_until.push(0);
            scratch_live_until.len() - 1
        });
        scratch_live_until[scratch_slot] = last_consumer[node_slot];
        node.scratch_slot = scratch_slot;
    }
    scratch_live_until.len()
}

// Prospective logical policy controls use actual Plan preparation and summaries.
// These owned tests qualify logical shape/bound behavior within their fixtures.
// These controls consume actual PreparedAudioPlan preparation and summaries.
// Metadata factories never create native or Rust DSP instances, and no Notes
// are present. Passing them would qualify only logical shape/bound behavior.

#[cfg(test)]
mod prospective_prepared_plan_parity_tests {
    use super::*;
    use crate::plan::{
        CompiledAudioSource, CompiledProcessor, CompiledProcessorSidechainRoute, CompiledRoute,
        CompiledSourceTimeMap,
    };
    use crate::{
        AudioProcessor, AudioProcessorAuxiliaryInputBusContract,
        AudioProcessorAuxiliaryInputContract, AudioProcessorExecutionContract,
        AudioProcessorFactory, AudioProcessorHostError, AudioProcessorPrepareRequest,
    };
    use mondrian_core::{
        AssetId, AudioChannelLayout, AudioComponentEditId, AudioProcessingScopeId,
        AudioProcessorInstanceId, AudioRouteId, AudioSourceComponentId, ClipId,
        ExactAutomationKeyframe, MixBusId, ProgramOutputId, SourceSamplingBoundary, TimeScale,
        TimelineTimeRange, TrackId,
    };
    use mondrian_timeline::audio::{
        gain_parameter_schema, AudioChannelStripOutputPort, AudioComponentChannelMapping,
        AudioProcessorDefinitionRef, AudioProcessorParameter, AudioTransitionCurve,
        CLIP_PAN_PARAMETER_ID, CLIP_VOLUME_DB_PARAMETER_ID, FADER_DB_PARAMETER_ID,
        GAIN_DB_PARAMETER_ID, INPUT_GAIN_DB_PARAMETER_ID, ROUTE_GAIN_DB_PARAMETER_ID,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn curve(parameter_id: &'static str, key_count: usize) -> ExactAutomationCurve {
        let mut result = ExactAutomationCurve::new(ParameterId::new_static(parameter_id), 0.0)
            .expect("finite fixture curve");
        for index in 0..key_count {
            result
                .set_keyframe(ExactAutomationKeyframe::linear(
                    TimelineTime::new(i64::try_from(index).expect("small fixture"), 1)
                        .expect("fixture time"),
                    index as f64,
                ))
                .expect("ordered fixture key");
        }
        result
    }

    fn processor(key_count: usize) -> CompiledProcessor {
        let automated_id = ParameterId::new_static(GAIN_DB_PARAMETER_ID);
        let unautomated_id = ParameterId::new_static("shape.discovered.unautomated");
        let mut unautomated_schema = gain_parameter_schema();
        unautomated_schema.parameter_id = unautomated_id.clone();
        CompiledProcessor {
            instance_id: AudioProcessorInstanceId::new(),
            definition: AudioProcessorDefinitionRef::Clap {
                plugin_id: "owned-shape-metadata-control".to_owned(),
                schema_version: 1,
                binary_sha256: None,
            },
            parameters: BTreeMap::from([
                (
                    automated_id,
                    AudioProcessorParameter {
                        schema: gain_parameter_schema(),
                        automation: Some(curve(GAIN_DB_PARAMETER_ID, key_count)),
                        ui: Default::default(),
                    },
                ),
                (
                    unautomated_id,
                    AudioProcessorParameter {
                        schema: unautomated_schema,
                        automation: None,
                        ui: Default::default(),
                    },
                ),
            ]),
            opaque_state: None,
        }
    }

    fn strip() -> CompiledChannelStrip {
        CompiledChannelStrip {
            input_trim_db: 0.0,
            pre_fader: CompiledRack { processors: vec![processor(2)] },
            fader_db: 0.0,
            fader_automation: Some(curve(FADER_DB_PARAMETER_ID, 1)),
            post_fader: CompiledRack { processors: vec![processor(3)] },
        }
    }

    fn contribution(
        track_id: TrackId,
        scope_id: AudioProcessingScopeId,
    ) -> CompiledAudioContribution {
        CompiledAudioContribution {
            edit_id: AudioComponentEditId::new(),
            clip_id: ClipId::new(),
            track_id,
            sequence_range: TimelineTimeRange::new(
                TimelineTime::ZERO,
                TimelineTime::new(3, 1).expect("fixture duration"),
            )
            .expect("fixture range"),
            source_time_map: CompiledSourceTimeMap {
                sequence_start: TimelineTime::ZERO,
                source_origin: TimelineTime::ZERO,
                scale: TimeScale::ONE,
                sampling_boundary: SourceSamplingBoundary::Covering,
            },
            source: CompiledAudioSource::Media {
                asset_id: AssetId::new(),
                component_id: AudioSourceComponentId::primary(),
            },
            channel_mapping: AudioComponentChannelMapping::Standard,
            processing_scope: scope_id,
            scope_in: TimelineTime::ZERO,
            local_time_in: TimelineTime::ZERO,
            volume_db: 0.0,
            volume_automation: Some(curve(CLIP_VOLUME_DB_PARAMETER_ID, 1)),
            pan: 0.0,
            pan_automation: Some(curve(CLIP_PAN_PARAMETER_ID, 2)),
            fade_in: None,
            fade_out: None,
        }
    }

    struct Fixture {
        program: Arc<CompiledAudioProgram>,
        auxiliary_processor_id: AudioProcessorInstanceId,
        second_edit_id: AudioComponentEditId,
    }

    fn fixture() -> Fixture {
        let main = TrackId::new();
        let detector = TrackId::new();
        let bus = MixBusId::new();
        let output = ProgramOutputId::new();
        let scope_id = AudioProcessingScopeId::new();
        let scope_processors = vec![processor(3), processor(0)];
        let auxiliary_processor_id = scope_processors[0].instance_id;
        let left = contribution(main, scope_id);
        let right = contribution(main, scope_id);
        let second_edit_id = right.edit_id;
        let source = |track_id| AudioRouteSource::Track {
            track_id,
            port: AudioChannelStripOutputPort::PostMute,
        };
        let main_route = |source, destination| CompiledRoute {
            id: AudioRouteId::new(),
            source,
            destination,
            gain_db: 0.0,
            gain_automation: Some(curve(ROUTE_GAIN_DB_PARAMETER_ID, 2)),
        };
        let transition_range = TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE)
            .expect("fixture transition range");
        let program = CompiledAudioProgram {
            musical_time_reference: None,
            output_id: output,
            transitions: vec![
                CompiledTransition {
                    left: left.edit_id,
                    right: right.edit_id,
                    sequence_range: transition_range,
                    curve: AudioTransitionCurve::ConstantGain,
                },
                // Private IR consumer edge control: author-validity of a
                // self-transition is deliberately not claimed by this fixture.
                CompiledTransition {
                    left: left.edit_id,
                    right: left.edit_id,
                    sequence_range: transition_range,
                    curve: AudioTransitionCurve::ConstantGain,
                },
            ],
            contributions: vec![left, right],
            processing_scopes: BTreeMap::from([(
                scope_id,
                CompiledProcessingScope {
                    id: scope_id,
                    input_gain_db: 0.0,
                    input_gain_automation: Some(curve(INPUT_GAIN_DB_PARAMETER_ID, 0)),
                    rack: CompiledRack { processors: scope_processors },
                },
            )]),
            track_channels: BTreeMap::from([
                (
                    main,
                    crate::plan::CompiledTrackChannel { muted: false, strip: strip() },
                ),
                (
                    detector,
                    crate::plan::CompiledTrackChannel { muted: false, strip: strip() },
                ),
            ]),
            buses: BTreeMap::from([(bus, strip())]),
            bus_order: vec![bus],
            output: strip(),
            routes: vec![
                main_route(source(main), AudioRouteDestination::Output(output)),
                main_route(source(detector), AudioRouteDestination::Bus(bus)),
                main_route(
                    AudioRouteSource::Bus {
                        bus_id: bus,
                        port: AudioChannelStripOutputPort::PostMute,
                    },
                    AudioRouteDestination::Output(output),
                ),
            ],
            sidechain_routes: vec![CompiledProcessorSidechainRoute {
                id: AudioRouteId::new(),
                source: source(detector),
                processor_id: auxiliary_processor_id,
                bus_key: "detector".to_owned(),
                gain_db: 0.0,
                gain_automation: Some(curve(ROUTE_GAIN_DB_PARAMETER_ID, 3)),
            }],
            note_lanes: Vec::new(),
        };
        Fixture {
            program: Arc::new(program),
            auxiliary_processor_id,
            second_edit_id,
        }
    }

    #[derive(Default)]
    struct Calls {
        resolves: AtomicUsize,
        creates: AtomicUsize,
    }

    struct MetadataResolver {
        auxiliary_processor_id: AudioProcessorInstanceId,
        omit_auxiliary_on_edit: Option<AudioComponentEditId>,
        calls: Arc<Calls>,
    }

    impl AudioProcessorResolver for MetadataResolver {
        fn prepare(
            &self,
            request: AudioProcessorPrepareRequest<'_>,
        ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
            self.calls.resolves.fetch_add(1, Ordering::SeqCst);
            assert!(!request.note_input_requested());
            let occurrence = request.occurrence();
            let omit = matches!(occurrence.owner,
                AudioProcessorOccurrenceOwner::Contribution { edit_id, .. }
                    if Some(edit_id) == self.omit_auxiliary_on_edit);
            Ok(Arc::new(MetadataFactory {
                layout: request.render_contract().channel_layout,
                has_auxiliary: occurrence.instance_id == self.auxiliary_processor_id && !omit,
                calls: Arc::clone(&self.calls),
            }))
        }
    }

    struct MetadataFactory {
        layout: AudioChannelLayout,
        has_auxiliary: bool,
        calls: Arc<Calls>,
    }

    impl AudioProcessorFactory for MetadataFactory {
        fn execution_contract(&self) -> AudioProcessorExecutionContract {
            AudioProcessorExecutionContract::stateless()
        }

        fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
            AudioProcessorAuxiliaryInputContract {
                buses: if self.has_auxiliary {
                    vec![AudioProcessorAuxiliaryInputBusContract {
                        bus_key: "detector".to_owned(),
                        channel_layout: self.layout,
                    }]
                } else {
                    Vec::new()
                },
            }
        }

        fn create(
            &self,
            _native_child_settlement: Option<&mondrian_core::NativeChildSettlementRegistrar>,
        ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
            self.calls.creates.fetch_add(1, Ordering::SeqCst);
            Err(AudioProcessorHostError::Unavailable(
                "shape metadata factory must never create DSP".to_owned(),
            ))
        }
    }

    fn render_contract() -> AudioRenderContract {
        AudioRenderContract {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Mono,
            max_block_frames: 16,
            processing_mode: crate::AudioProcessingMode::Offline,
            processor_session_scratch_budget_bytes: 1024 * 1024,
            public_output_lookahead_budget_frames: 1024,
            compensation_delay_scratch_budget_bytes: 1024 * 1024,
        }
    }

    fn prepare(
        fixture: &Fixture,
        omit: Option<AudioComponentEditId>,
        calls: &Arc<Calls>,
    ) -> PreparedAudioPlan {
        PreparedAudioPlan::prepare_with_processor_resolver(
            Arc::clone(&fixture.program),
            render_contract(),
            AudioKernelBackend::ScalarReference,
            &MetadataResolver {
                auxiliary_processor_id: fixture.auxiliary_processor_id,
                omit_auxiliary_on_edit: omit,
                calls: Arc::clone(calls),
            },
        )
        .expect("actual schedule preparation, metadata only")
    }

    #[test]
    fn prospective_shape_matches_actual_plan_for_repeated_scope_and_automated_sidechains() {
        let fixture = fixture();
        let calls = Arc::new(Calls::default());
        let requested =
            PreparedLogicalShape::requested(fixture.program.as_ref()).expect("borrowed shape");
        assert_eq!(calls.resolves.load(Ordering::SeqCst), 0);
        assert_eq!(calls.creates.load(Ordering::SeqCst), 0);
        let plan = prepare(&fixture, None, &calls);
        let summary = plan.schedule_summary();
        let actual = PreparedLogicalShape::from_summary(summary);
        assert_eq!(requested, actual);
        assert_eq!(
            (
                actual.nodes,
                actual.routes,
                actual.sidechain_bindings,
                actual.contributions,
                actual.processors,
                actual.automation_curves,
                actual.automation_spans,
                actual.transition_bindings
            ),
            (4, 3, 2, 2, 12, 27, 52, 3)
        );
        assert_eq!(summary.processor_parameter_lane_count, 24);
        assert_eq!(summary.maximum_parameter_events_per_block, 16);
        assert_eq!(summary.processor_auxiliary_bus_count, 2);
        assert_eq!(summary.sidechain_source_count, 1);
        assert_eq!(calls.resolves.load(Ordering::SeqCst), 12);
        assert_eq!(calls.creates.load(Ordering::SeqCst), 0);
        assert_eq!(
            prepared_logical_bytes_for_shape(requested),
            Ok(plan
                .session_resource_footprint()
                .expect("actual footprint")
                .prepared_logical_bytes)
        );
        // Equal left/right picks the Falling branch once; ordinary endpoints
        // add Falling(left) and Rising(right), for three actual bindings.
        assert_eq!(plan.schedule.transitions.len(), 3);
        assert_eq!(
            plan.schedule
                .transitions
                .iter()
                .filter(|binding| binding.direction == PreparedTransitionDirection::Falling)
                .count(),
            2
        );
        assert_eq!(
            plan.schedule
                .transitions
                .iter()
                .filter(|binding| binding.direction == PreparedTransitionDirection::Rising)
                .count(),
            1
        );
    }

    #[test]
    fn prospective_sidechain_shape_bounds_a_successful_occurrence_with_no_matching_bus() {
        let fixture = fixture();
        let calls = Arc::new(Calls::default());
        let requested = PreparedLogicalShape::requested(fixture.program.as_ref())
            .expect("borrowed upper shape");
        let plan = prepare(&fixture, Some(fixture.second_edit_id), &calls);
        let actual = PreparedLogicalShape::from_summary(plan.schedule_summary());
        assert_eq!(requested.sidechain_bindings, actual.sidechain_bindings + 1);
        assert_eq!(requested.automation_curves, actual.automation_curves + 1);
        assert_eq!(requested.automation_spans, actual.automation_spans + 3);
        assert_eq!(
            (
                requested.nodes,
                requested.routes,
                requested.contributions,
                requested.processors,
                requested.transition_bindings
            ),
            (
                actual.nodes,
                actual.routes,
                actual.contributions,
                actual.processors,
                actual.transition_bindings
            )
        );
        assert_eq!(actual.sidechain_bindings, 1);
        assert_eq!(plan.schedule_summary().processor_auxiliary_bus_count, 1);
        assert_eq!(calls.resolves.load(Ordering::SeqCst), 12);
        assert_eq!(calls.creates.load(Ordering::SeqCst), 0);
        let actual_bytes = plan
            .session_resource_footprint()
            .expect("actual footprint")
            .prepared_logical_bytes;
        let requested_bytes =
            prepared_logical_bytes_for_shape(requested).expect("checked upper footprint");
        // One sidechain route record + one curve + its three event spans.
        assert_eq!(
            requested_bytes.checked_sub(actual_bytes),
            Some(512 + 512 + 3 * 64)
        );
        assert_eq!(prepared_logical_bytes_for_shape(actual), Ok(actual_bytes));
    }

    #[test]
    fn prospective_bus_shape_consumes_bus_order_instead_of_unlisted_map_values() {
        let Fixture { program, auxiliary_processor_id, second_edit_id } = fixture();
        let mut program = program.as_ref().clone();
        // Private IR consumer edge control, not compiler validity evidence.
        program.buses.insert(MixBusId::new(), strip());
        assert_eq!(program.buses.len(), 2);
        assert_eq!(program.bus_order.len(), 1);
        let fixture = Fixture {
            program: Arc::new(program),
            auxiliary_processor_id,
            second_edit_id,
        };
        let requested = PreparedLogicalShape::requested(fixture.program.as_ref())
            .expect("borrowed selected Bus order");
        let calls = Arc::new(Calls::default());
        let plan = prepare(&fixture, None, &calls);
        assert_eq!(
            requested,
            PreparedLogicalShape::from_summary(plan.schedule_summary())
        );
        assert_eq!(requested.nodes, 4);
        assert_eq!(requested.processors, 12);
        assert_eq!(calls.resolves.load(Ordering::SeqCst), 12);
        assert_eq!(calls.creates.load(Ordering::SeqCst), 0);
    }
}

#[cfg(test)]
#[path = "schedule/compiled_note_payload_tests.rs"]
mod compiled_note_payload_tests;
