use crate::dependency::{
    select_program_window, selected_contribution_source_window, AudioDependencyClosure,
    DependencyWindow,
};
use crate::processor_host::default_processor_resolver;
use crate::schedule::{resolve_channel_mapping, AudioKernelBackend, AudioPreparationDependencies};
use crate::{
    compile_audio_program, AudioCompileRequest, AudioContinuityEpoch, AudioExecutionError,
    AudioPcmSource, AudioProcessorResolver, AudioProgramExecutionDemand, AudioRenderContract,
    AudioRenderRequest, AudioRenderSession, AudioSessionResourceFootprint, AudioStateEntry,
    CompiledAudioSource, PreparedAudioPlan,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioComponentEditId, AudioSourceComponentId,
    ExecutionCancellationToken, ProgramOutputId, SequenceId, TimelineTimeRange,
};
use mondrian_timeline::{sequence::MAX_NESTED_SEQUENCE_RENDER_DEPTH, Sequence};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const NESTED_SOURCE_CACHE_FRAMES: usize = 4_096;
const MEDIA_SOURCE_CACHE_FRAMES: usize = 4_096;
const RUNTIME_SOURCE_BINDING_BYTES: usize = 256;
const NESTED_RUNTIME_BINDING_BYTES: usize = 512;

/// Closure-wide hard admission grant for one Audio Program Runtime.
///
/// Per-Session processor/PDC/lookahead limits remain in
/// [`AudioRenderContract`]. This independent grant prevents a nested Signal
/// Closure from multiplying those local limits across many mutable
/// occurrences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioRuntimeResourceGrant {
    /// Maximum generated mutable Runtime occurrences in the nested closure.
    pub max_runtime_occurrences: usize,
    /// Maximum aggregate fixed live Session/source-window residency.
    pub max_fixed_resident_bytes: usize,
    /// Maximum aggregate immutable prepared-plan logical bytes.
    pub max_prepared_logical_bytes: usize,
}

impl AudioRuntimeResourceGrant {
    /// Build an explicit closure-wide hard grant.
    pub const fn new(
        max_runtime_occurrences: usize,
        max_fixed_resident_bytes: usize,
        max_prepared_logical_bytes: usize,
    ) -> Self {
        Self {
            max_runtime_occurrences,
            max_fixed_resident_bytes,
            max_prepared_logical_bytes,
        }
    }

    const fn unbounded() -> Self {
        Self::new(usize::MAX, usize::MAX, usize::MAX)
    }
}

/// Auditable resource footprint of one fully built nested Audio Runtime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioRuntimeResourceFootprint {
    /// Generated mutable Runtime occurrences, not unique Sequence identities.
    pub runtime_occurrences: usize,
    /// Aggregate fixed live residency.
    pub fixed_resident_bytes: usize,
    /// Aggregate immutable prepared-plan logical bytes.
    pub prepared_logical_bytes: usize,
    /// Session node/block render scratch.
    pub render_scratch_bytes: usize,
    /// Processor-declared private Session state.
    pub processor_session_bytes: usize,
    /// PDC delay-line payload.
    pub compensation_delay_bytes: usize,
    /// Sample-accurate processor-event storage.
    pub parameter_event_bytes: usize,
    /// Runtime-owned media source-window payload.
    pub media_window_bytes: usize,
    /// Runtime-owned nested-output window and ordering payload.
    pub nested_window_bytes: usize,
}

/// Hard closure resource whose grant was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRuntimeResourceCategory {
    /// Generated mutable Runtime occurrence count.
    RuntimeOccurrences,
    /// Fixed live Session/source-window residency.
    FixedResidentBytes,
    /// Immutable prepared-plan logical bytes.
    PreparedLogicalBytes,
}

#[derive(Debug, Clone, Copy)]
struct AudioRuntimeAdmissionLedger {
    grant: AudioRuntimeResourceGrant,
    footprint: AudioRuntimeResourceFootprint,
}

impl AudioRuntimeAdmissionLedger {
    fn new(grant: AudioRuntimeResourceGrant) -> Self {
        Self {
            grant,
            footprint: AudioRuntimeResourceFootprint::default(),
        }
    }

    fn reserve_session(
        &mut self,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
        session: AudioSessionResourceFootprint,
    ) -> Result<(), AudioRuntimeBuildError> {
        self.reserve(
            sequence_id,
            output_id,
            AudioRuntimeResourceFootprint {
                runtime_occurrences: 1,
                fixed_resident_bytes: session.fixed_resident_bytes,
                prepared_logical_bytes: session.prepared_logical_bytes,
                render_scratch_bytes: session.render_scratch_bytes,
                processor_session_bytes: session.processor_session_bytes,
                compensation_delay_bytes: session.compensation_delay_bytes,
                parameter_event_bytes: session.parameter_event_bytes,
                ..AudioRuntimeResourceFootprint::default()
            },
        )
    }

    fn reserve_media_window(
        &mut self,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
        bytes: usize,
    ) -> Result<(), AudioRuntimeBuildError> {
        let bytes = bytes.checked_add(RUNTIME_SOURCE_BINDING_BYTES).ok_or(
            AudioRuntimeBuildError::ResourceExtentOverflow {
                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                sequence_id,
                output_id,
            },
        )?;
        self.reserve(
            sequence_id,
            output_id,
            AudioRuntimeResourceFootprint {
                fixed_resident_bytes: bytes,
                media_window_bytes: bytes,
                ..AudioRuntimeResourceFootprint::default()
            },
        )
    }

    fn reserve_nested_window(
        &mut self,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
        bytes: usize,
    ) -> Result<(), AudioRuntimeBuildError> {
        let bytes = bytes
            .checked_add(RUNTIME_SOURCE_BINDING_BYTES)
            .and_then(|bytes| bytes.checked_add(NESTED_RUNTIME_BINDING_BYTES))
            .ok_or(AudioRuntimeBuildError::ResourceExtentOverflow {
                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                sequence_id,
                output_id,
            })?;
        self.reserve(
            sequence_id,
            output_id,
            AudioRuntimeResourceFootprint {
                fixed_resident_bytes: bytes,
                nested_window_bytes: bytes,
                ..AudioRuntimeResourceFootprint::default()
            },
        )
    }

    fn reserve(
        &mut self,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
        delta: AudioRuntimeResourceFootprint,
    ) -> Result<(), AudioRuntimeBuildError> {
        let next = self.footprint.checked_add(delta).ok_or(
            AudioRuntimeBuildError::ResourceExtentOverflow {
                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                sequence_id,
                output_id,
            },
        )?;
        for (category, required, granted) in [
            (
                AudioRuntimeResourceCategory::RuntimeOccurrences,
                next.runtime_occurrences,
                self.grant.max_runtime_occurrences,
            ),
            (
                AudioRuntimeResourceCategory::FixedResidentBytes,
                next.fixed_resident_bytes,
                self.grant.max_fixed_resident_bytes,
            ),
            (
                AudioRuntimeResourceCategory::PreparedLogicalBytes,
                next.prepared_logical_bytes,
                self.grant.max_prepared_logical_bytes,
            ),
        ] {
            if required > granted {
                return Err(AudioRuntimeBuildError::ResourceGrantExceeded {
                    category,
                    sequence_id,
                    output_id,
                    required,
                    granted,
                });
            }
        }
        self.footprint = next;
        Ok(())
    }
}

impl AudioRuntimeResourceFootprint {
    fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            runtime_occurrences: self.runtime_occurrences.checked_add(other.runtime_occurrences)?,
            fixed_resident_bytes: self
                .fixed_resident_bytes
                .checked_add(other.fixed_resident_bytes)?,
            prepared_logical_bytes: self
                .prepared_logical_bytes
                .checked_add(other.prepared_logical_bytes)?,
            render_scratch_bytes: self
                .render_scratch_bytes
                .checked_add(other.render_scratch_bytes)?,
            processor_session_bytes: self
                .processor_session_bytes
                .checked_add(other.processor_session_bytes)?,
            compensation_delay_bytes: self
                .compensation_delay_bytes
                .checked_add(other.compensation_delay_bytes)?,
            parameter_event_bytes: self
                .parameter_event_bytes
                .checked_add(other.parameter_event_bytes)?,
            media_window_bytes: self.media_window_bytes.checked_add(other.media_window_bytes)?,
            nested_window_bytes: self.nested_window_bytes.checked_add(other.nested_window_bytes)?,
        })
    }

    fn difference(self, before: Self) -> Self {
        Self {
            runtime_occurrences: self
                .runtime_occurrences
                .saturating_sub(before.runtime_occurrences),
            fixed_resident_bytes: self
                .fixed_resident_bytes
                .saturating_sub(before.fixed_resident_bytes),
            prepared_logical_bytes: self
                .prepared_logical_bytes
                .saturating_sub(before.prepared_logical_bytes),
            render_scratch_bytes: self
                .render_scratch_bytes
                .saturating_sub(before.render_scratch_bytes),
            processor_session_bytes: self
                .processor_session_bytes
                .saturating_sub(before.processor_session_bytes),
            compensation_delay_bytes: self
                .compensation_delay_bytes
                .saturating_sub(before.compensation_delay_bytes),
            parameter_event_bytes: self
                .parameter_event_bytes
                .saturating_sub(before.parameter_event_bytes),
            media_window_bytes: self.media_window_bytes.saturating_sub(before.media_window_bytes),
            nested_window_bytes: self
                .nested_window_bytes
                .saturating_sub(before.nested_window_bytes),
        }
    }
}

/// Consumer-owned decoded PCM exposed at the media/source Adapter Seam.
pub trait AudioDecodedSource: Send + Sync + 'static {
    /// Fill one exact native-layout interleaved block on the requested sample grid.
    ///
    /// `destination` is pre-zeroed and has exactly `frames` multiplied by the
    /// resolved source layout's channel count samples.
    /// Implementations must preserve silence outside the source range and return
    /// an error rather than publish a partial or shifted block.
    fn read_interleaved(
        &self,
        start_frame: i64,
        frames: usize,
        destination: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), String>;
}

/// One media dependency bound to native-layout PCM at the requested sample rate.
pub struct ResolvedAudioSource {
    channel_layout: AudioChannelLayout,
    source: Arc<dyn AudioDecodedSource>,
}

impl ResolvedAudioSource {
    /// Bind a decoded source to its exact native semantic signal layout.
    pub fn new(channel_layout: AudioChannelLayout, source: Arc<dyn AudioDecodedSource>) -> Self {
        Self { channel_layout, source }
    }

    /// Exact layout produced by every decoded block.
    pub const fn channel_layout(&self) -> AudioChannelLayout {
        self.channel_layout
    }
}

/// Resolve stable authoring source identities into decoded PCM.
pub trait AudioMediaResolver {
    /// Resolve one media component at the requested rate without changing its layout.
    fn resolve(
        &self,
        asset_id: AssetId,
        component_id: AudioSourceComponentId,
        sample_rate: u32,
    ) -> Result<ResolvedAudioSource, String>;
}

/// Shared compiled runtime used by Playback, Export, Audition, and Analysis.
///
/// The runtime owns one exclusive mutable Session and recursively compiled
/// child-output Sessions. Consumers only supply the media decode Adapter.
pub struct AudioProgramRuntime {
    session: AudioRenderSession,
    sources: RuntimeSources,
    execution_demand: AudioProgramExecutionDemand,
    resource_footprint: AudioRuntimeResourceFootprint,
}

impl AudioProgramRuntime {
    /// Compile, prepare, bind media, and recursively instantiate nested public outputs.
    pub fn build(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        contract: AudioRenderContract,
        output_id: Option<ProgramOutputId>,
    ) -> Result<Self, AudioRuntimeBuildError> {
        Self::build_with_processor_resolver_and_resource_grant(
            root,
            sequences,
            resolver,
            default_processor_resolver(),
            contract,
            output_id,
            AudioRuntimeResourceGrant::unbounded(),
        )
    }

    /// Build with one closure-wide hard resource grant.
    pub fn build_with_resource_grant(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        contract: AudioRenderContract,
        output_id: Option<ProgramOutputId>,
        resource_grant: AudioRuntimeResourceGrant,
    ) -> Result<Self, AudioRuntimeBuildError> {
        Self::build_with_processor_resolver_and_resource_grant(
            root,
            sequences,
            resolver,
            default_processor_resolver(),
            contract,
            output_id,
            resource_grant,
        )
    }

    /// Build a root Program from one explicit semantic compile request.
    ///
    /// The request may carry a transient root-Sequence audition overlay.
    /// Nested public outputs always compile their own canonical Program without
    /// inheriting parent Track identities or Session audition state.
    pub fn build_with_compile_request_and_resource_grant(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        contract: AudioRenderContract,
        request: AudioCompileRequest,
        resource_grant: AudioRuntimeResourceGrant,
    ) -> Result<Self, AudioRuntimeBuildError> {
        if contract.channel_layout != root.settings.audio_channel_layout {
            return Err(AudioRuntimeBuildError::ProgramLayoutMismatch {
                sequence_id: root.id,
                authored: root.settings.audio_channel_layout,
                prepared: contract.channel_layout,
            });
        }
        let mut stack = BTreeSet::new();
        let mut ledger = AudioRuntimeAdmissionLedger::new(resource_grant);
        Self::build_inner(
            root,
            sequences,
            resolver,
            default_processor_resolver(),
            contract,
            Some(request),
            None,
            None,
            &mut stack,
            0,
            &mut ledger,
        )
    }

    /// Build with an explicit processor resolver shared by the complete nested closure.
    pub fn build_with_processor_resolver(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        processor_resolver: &dyn AudioProcessorResolver,
        contract: AudioRenderContract,
        output_id: Option<ProgramOutputId>,
    ) -> Result<Self, AudioRuntimeBuildError> {
        Self::build_with_processor_resolver_and_resource_grant(
            root,
            sequences,
            resolver,
            processor_resolver,
            contract,
            output_id,
            AudioRuntimeResourceGrant::unbounded(),
        )
    }

    /// Build with an explicit processor resolver and closure-wide hard grant.
    pub fn build_with_processor_resolver_and_resource_grant(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        processor_resolver: &dyn AudioProcessorResolver,
        contract: AudioRenderContract,
        output_id: Option<ProgramOutputId>,
        resource_grant: AudioRuntimeResourceGrant,
    ) -> Result<Self, AudioRuntimeBuildError> {
        if contract.channel_layout != root.settings.audio_channel_layout {
            return Err(AudioRuntimeBuildError::ProgramLayoutMismatch {
                sequence_id: root.id,
                authored: root.settings.audio_channel_layout,
                prepared: contract.channel_layout,
            });
        }
        let mut stack = BTreeSet::new();
        let mut ledger = AudioRuntimeAdmissionLedger::new(resource_grant);
        Self::build_inner(
            root,
            sequences,
            resolver,
            processor_resolver,
            contract,
            output_id.map(AudioCompileRequest::program),
            None,
            None,
            &mut stack,
            0,
            &mut ledger,
        )
    }

    /// Build only the routed Signal Closure audible in one exact public window.
    ///
    /// This is the production seam for selected-range Export. Range selection
    /// happens after semantic output routing but before media binding and
    /// nested Runtime construction, so muted and off-range sources
    /// cannot consume resources or become accidental execution dependencies.
    pub fn build_for_range_with_resource_grant(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        contract: AudioRenderContract,
        output_id: Option<ProgramOutputId>,
        range: TimelineTimeRange,
        resource_grant: AudioRuntimeResourceGrant,
    ) -> Result<Self, AudioRuntimeBuildError> {
        if contract.channel_layout != root.settings.audio_channel_layout {
            return Err(AudioRuntimeBuildError::ProgramLayoutMismatch {
                sequence_id: root.id,
                authored: root.settings.audio_channel_layout,
                prepared: contract.channel_layout,
            });
        }
        let window = DependencyWindow::from_half_open(range).map_err(AudioExecutionError::from)?;
        let mut stack = BTreeSet::new();
        let mut ledger = AudioRuntimeAdmissionLedger::new(resource_grant);
        Self::build_inner(
            root,
            sequences,
            resolver,
            default_processor_resolver(),
            contract,
            output_id.map(AudioCompileRequest::program),
            Some(window),
            None,
            &mut stack,
            0,
            &mut ledger,
        )
    }

    /// Build one selected-range Runtime from the exact root and nested semantic
    /// Programs frozen at an earlier admission boundary.
    ///
    /// No Program is recompiled or reselected. The frozen occurrence key
    /// includes Sequence, public Output, and exact projected dependency window;
    /// it is deliberately not a coarse Sequence-ID cache.
    pub fn build_from_precompiled_closure_for_range_with_resource_grant(
        root: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        contract: AudioRenderContract,
        output_id: Option<ProgramOutputId>,
        range: TimelineTimeRange,
        prepared_closure: &AudioDependencyClosure,
        resource_grant: AudioRuntimeResourceGrant,
    ) -> Result<Self, AudioRuntimeBuildError> {
        if contract.channel_layout != root.settings.audio_channel_layout {
            return Err(AudioRuntimeBuildError::ProgramLayoutMismatch {
                sequence_id: root.id,
                authored: root.settings.audio_channel_layout,
                prepared: contract.channel_layout,
            });
        }
        let window = DependencyWindow::from_half_open(range).map_err(AudioExecutionError::from)?;
        let mut stack = BTreeSet::new();
        let mut ledger = AudioRuntimeAdmissionLedger::new(resource_grant);
        Self::build_inner(
            root,
            sequences,
            resolver,
            default_processor_resolver(),
            contract,
            output_id.map(AudioCompileRequest::program),
            Some(window),
            Some(prepared_closure),
            &mut stack,
            0,
            &mut ledger,
        )
    }

    fn build_inner(
        sequence: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        processor_resolver: &dyn AudioProcessorResolver,
        contract: AudioRenderContract,
        compile_request: Option<AudioCompileRequest>,
        selection: Option<DependencyWindow>,
        prepared_closure: Option<&AudioDependencyClosure>,
        stack: &mut BTreeSet<SequenceId>,
        depth: usize,
        ledger: &mut AudioRuntimeAdmissionLedger,
    ) -> Result<Self, AudioRuntimeBuildError> {
        if depth > MAX_NESTED_SEQUENCE_RENDER_DEPTH {
            return Err(AudioRuntimeBuildError::NestedDepthExceeded {
                sequence_id: sequence.id,
                maximum: MAX_NESTED_SEQUENCE_RENDER_DEPTH,
            });
        }
        if !stack.insert(sequence.id) {
            return Err(AudioRuntimeBuildError::NestedCycle(sequence.id));
        }
        let footprint_before = ledger.footprint;
        let result = (|| {
            let compile_request = compile_request
                .or_else(|| {
                    sequence
                        .audio_program
                        .outputs
                        .first()
                        .map(|output| AudioCompileRequest::program(output.id))
                })
                .ok_or(AudioRuntimeBuildError::MissingProgramOutput(sequence.id))?;
            let output_id = compile_request.output_id;
            let program = if let Some(prepared_closure) = prepared_closure {
                let window = selection.ok_or(AudioRuntimeBuildError::MissingPreparedProgram {
                    sequence_id: sequence.id,
                    output_id,
                })?;
                prepared_closure.program(sequence.id, output_id, window).cloned().ok_or(
                    AudioRuntimeBuildError::MissingPreparedProgram {
                        sequence_id: sequence.id,
                        output_id,
                    },
                )?
            } else {
                let mut program = compile_audio_program(sequence, compile_request)?;
                if let Some(window) = selection {
                    select_program_window(&mut program, window)
                        .map_err(AudioExecutionError::from)?;
                }
                Arc::new(program)
            };
            let execution_demand = program.execution_demand();
            let nested_cache_frames =
                contract.max_block_frames.clamp(1, NESTED_SOURCE_CACHE_FRAMES);
            let mut entries = BTreeMap::new();
            let mut dependencies = AudioPreparationDependencies::default();
            for contribution in program.contributions() {
                let source = match contribution.source {
                    CompiledAudioSource::Media { asset_id, component_id } => {
                        let resolved = resolver
                            .resolve(asset_id, component_id, contract.sample_rate)
                            .map_err(|reason| AudioRuntimeBuildError::Media {
                                asset_id,
                                component_id,
                                reason,
                            })?;
                        let channel_mix = resolve_channel_mapping(
                            &contribution.channel_mapping,
                            resolved.channel_layout,
                            contract.channel_layout,
                        )?;
                        dependencies.insert_source(
                            contribution.edit_id,
                            resolved.channel_layout,
                            channel_mix,
                            0,
                            false,
                        )?;
                        let cache_frames =
                            contract.max_block_frames.clamp(1, MEDIA_SOURCE_CACHE_FRAMES);
                        let cache_samples = cache_frames
                            .checked_mul(resolved.channel_layout.channel_count())
                            .ok_or(AudioRuntimeBuildError::ResourceExtentOverflow {
                                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                                sequence_id: sequence.id,
                                output_id,
                            })?;
                        let cache_bytes = cache_samples
                            .checked_mul(std::mem::size_of::<f32>())
                            .ok_or(AudioRuntimeBuildError::ResourceExtentOverflow {
                                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                                sequence_id: sequence.id,
                                output_id,
                            })?;
                        ledger.reserve_media_window(sequence.id, output_id, cache_bytes)?;
                        RuntimeSource::Media(MediaRuntimeSource {
                            source: resolved.source,
                            cache_start: i64::MIN,
                            cache_frames,
                            cache: vec![0.0; cache_samples],
                            channel_layout: resolved.channel_layout,
                        })
                    }
                    CompiledAudioSource::NestedOutput { sequence_id, output_id } => {
                        let child = sequences
                            .iter()
                            .find(|candidate| candidate.id == sequence_id)
                            .ok_or(AudioRuntimeBuildError::MissingNestedSequence(sequence_id))?;
                        let child_contract = AudioRenderContract {
                            channel_layout: child.settings.audio_channel_layout,
                            ..contract
                        };
                        let child_selection = match selection {
                            Some(window) => Some(
                                selected_contribution_source_window(contribution, window)
                                    .map_err(AudioExecutionError::from)?
                                    .ok_or(AudioExecutionError::InvalidPreparedSchedule)?,
                            ),
                            None => None,
                        };
                        let runtime = Self::build_inner(
                            child,
                            sequences,
                            resolver,
                            processor_resolver,
                            child_contract,
                            Some(AudioCompileRequest::program(output_id)),
                            child_selection,
                            prepared_closure,
                            stack,
                            depth + 1,
                            ledger,
                        )?;
                        if runtime.requires_state_entry()
                            && contribution.source_time_map.scale.numerator() < 0
                        {
                            return Err(
                                AudioRuntimeBuildError::UnsupportedStatefulNestedDirection(
                                    contribution.edit_id,
                                ),
                            );
                        }
                        let channel_mix = resolve_channel_mapping(
                            &contribution.channel_mapping,
                            child_contract.channel_layout,
                            contract.channel_layout,
                        )?;
                        dependencies.insert_source(
                            contribution.edit_id,
                            child_contract.channel_layout,
                            channel_mix,
                            // A public child Runtime already consumes its own
                            // lookahead and returns Timeline-aligned PCM.
                            0,
                            runtime.requires_state_entry(),
                        )?;
                        let cache_samples = nested_cache_frames
                            .checked_mul(child_contract.channel_count())
                            .ok_or(AudioRuntimeBuildError::ResourceExtentOverflow {
                                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                                sequence_id: sequence.id,
                                output_id,
                            })?;
                        let nested_bytes = cache_samples
                            .checked_mul(std::mem::size_of::<f32>())
                            .and_then(|bytes| {
                                contract
                                    .max_block_frames
                                    .checked_mul(std::mem::size_of::<usize>())
                                    .and_then(|order_bytes| bytes.checked_add(order_bytes))
                            })
                            .ok_or(AudioRuntimeBuildError::ResourceExtentOverflow {
                                category: AudioRuntimeResourceCategory::FixedResidentBytes,
                                sequence_id: sequence.id,
                                output_id,
                            })?;
                        ledger.reserve_nested_window(sequence.id, output_id, nested_bytes)?;
                        RuntimeSource::Nested(NestedRuntimeSource {
                            runtime: Box::new(runtime),
                            cache_start: i64::MIN,
                            cache_frames: nested_cache_frames,
                            cache: vec![0.0; cache_samples],
                            channel_layout: child_contract.channel_layout,
                            next_sample: None,
                            next_epoch: 1,
                            last_demanded_sample: None,
                            ordered_output_frames: Vec::with_capacity(contract.max_block_frames),
                        })
                    }
                };
                entries.insert(contribution.edit_id, source);
            }
            let plan = Arc::new(PreparedAudioPlan::prepare_with_dependencies(
                program,
                contract,
                AudioKernelBackend::default(),
                &dependencies,
                processor_resolver,
            )?);
            let session_footprint = plan.session_resource_footprint()?;
            ledger.reserve_session(sequence.id, output_id, session_footprint)?;
            let session = AudioRenderSession::new(plan)?;
            let capacity = session.capacity();
            let realized_compensation_bytes = capacity
                .compensation_delay_samples
                .checked_mul(std::mem::size_of::<f32>())
                .ok_or(AudioRuntimeBuildError::ResourceExtentOverflow {
                    category: AudioRuntimeResourceCategory::FixedResidentBytes,
                    sequence_id: sequence.id,
                    output_id,
                })?;
            if capacity.processor_session_scratch_bytes != session_footprint.processor_session_bytes
                || realized_compensation_bytes != session_footprint.compensation_delay_bytes
            {
                return Err(AudioExecutionError::InvalidPreparedSchedule.into());
            }
            Ok(Self {
                session,
                sources: RuntimeSources {
                    entries,
                    cancellation: ExecutionCancellationToken::new(),
                },
                execution_demand,
                resource_footprint: ledger.footprint.difference(footprint_before),
            })
        })();
        stack.remove(&sequence.id);
        result
    }

    /// Execute one exact block into caller-owned interleaved float PCM.
    pub fn render_into(
        &mut self,
        request: AudioRenderRequest,
        destination: &mut [f32],
    ) -> Result<(), AudioExecutionError> {
        self.render_into_cancellable(request, destination, &ExecutionCancellationToken::new())
    }

    /// Internal lookahead needed to return Timeline-aligned public PCM.
    pub fn public_output_lookahead_frames(&self) -> usize {
        self.session.public_output_lookahead_frames()
    }

    /// Frozen execution demand of the selected root Program Output.
    pub const fn execution_demand(&self) -> AudioProgramExecutionDemand {
        self.execution_demand
    }

    /// Frozen closure-wide resource evidence for this Runtime occurrence.
    pub const fn resource_footprint(&self) -> AudioRuntimeResourceFootprint {
        self.resource_footprint
    }

    /// Latest successfully completed root Program Output meter block.
    pub fn latest_meter_frame(&self) -> crate::AudioMeterFrame {
        self.session.latest_meter_frame()
    }

    /// Obtain a lock-free root Program Output observation handle.
    pub fn meter_observer(&self) -> crate::AudioMeterObserver {
        self.session.meter_observer()
    }

    /// Whether the selected root/child closure owns mutable continuity state.
    pub fn requires_state_entry(&self) -> bool {
        self.session.requires_state_entry()
    }

    #[cfg(test)]
    pub(crate) fn require_state_entry_recursively_for_test(&mut self) {
        self.session.require_state_entry_for_test();
        for source in self.sources.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.runtime.require_state_entry_recursively_for_test();
            }
        }
    }

    /// Enter a fresh root continuity epoch before executing a stateful Plan.
    ///
    /// Child instances enter lazily at the first exact child sample demanded by
    /// the parent time map. Each instance owns its own monotonic epoch sequence.
    pub fn enter_state(&mut self, entry: AudioStateEntry) -> Result<(), AudioExecutionError> {
        self.session.enter_state(entry)?;
        self.sources.begin_continuity();
        Ok(())
    }

    /// Execute one exact block with consumer-generation cancellation authority.
    pub fn render_into_cancellable(
        &mut self,
        request: AudioRenderRequest,
        destination: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        self.sources.cancellation = cancellation.clone();
        if cancellation.is_canceled() {
            return Err(AudioExecutionError::SourceUnavailable(
                "audio render generation was canceled before execution".to_owned(),
            ));
        }
        self.session.render_into(&mut self.sources, request, destination)
    }
}

struct RuntimeSources {
    entries: BTreeMap<AudioComponentEditId, RuntimeSource>,
    cancellation: ExecutionCancellationToken,
}

impl RuntimeSources {
    fn begin_continuity(&mut self) {
        for source in self.entries.values_mut() {
            if let RuntimeSource::Nested(nested) = source {
                nested.begin_continuity();
            }
        }
    }
}

enum RuntimeSource {
    Media(MediaRuntimeSource),
    Nested(NestedRuntimeSource),
}

struct MediaRuntimeSource {
    source: Arc<dyn AudioDecodedSource>,
    cache_start: i64,
    cache_frames: usize,
    cache: Vec<f32>,
    channel_layout: AudioChannelLayout,
}

struct NestedRuntimeSource {
    runtime: Box<AudioProgramRuntime>,
    cache_start: i64,
    cache_frames: usize,
    cache: Vec<f32>,
    channel_layout: AudioChannelLayout,
    next_sample: Option<i64>,
    next_epoch: u64,
    last_demanded_sample: Option<i64>,
    ordered_output_frames: Vec<usize>,
}

impl AudioPcmSource for RuntimeSources {
    fn read_indexed_interleaved(
        &mut self,
        edit: AudioComponentEditId,
        source_frames: &[i64],
        channel_layout: AudioChannelLayout,
        destination: &mut [f32],
    ) -> Result<(), AudioExecutionError> {
        let channels = channel_layout.channel_count();
        let expected_samples = source_frames
            .len()
            .checked_mul(channels)
            .ok_or(AudioExecutionError::BufferTooLarge)?;
        if destination.len() != expected_samples {
            return Err(AudioExecutionError::OutputSizeMismatch);
        }
        destination.fill(0.0);
        let cancellation = self.cancellation.clone();
        let source = self.entries.get_mut(&edit).ok_or_else(|| {
            AudioExecutionError::SourceUnavailable(format!("component edit {edit}"))
        })?;
        match source {
            RuntimeSource::Media(media) => {
                media.read_indexed(source_frames, channel_layout, destination, &cancellation)
            }
            RuntimeSource::Nested(nested) => nested.read_indexed(
                edit,
                source_frames,
                channel_layout,
                destination,
                &cancellation,
            ),
        }
    }
}

impl MediaRuntimeSource {
    fn read_indexed(
        &mut self,
        source_frames: &[i64],
        channel_layout: AudioChannelLayout,
        destination: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        if channel_layout != self.channel_layout {
            return Err(AudioExecutionError::SourceUnavailable(format!(
                "decoded media layout contract changed from {:?} to {channel_layout:?}",
                self.channel_layout
            )));
        }
        let channels = channel_layout.channel_count();
        let cache_frames_i64 =
            i64::try_from(self.cache_frames).map_err(|_| AudioExecutionError::BufferTooLarge)?;
        for (output_frame, source_frame) in source_frames.iter().copied().enumerate() {
            if source_frame < 0 {
                continue;
            }
            let cache_end = self.cache_start.saturating_add(cache_frames_i64);
            if source_frame < self.cache_start || source_frame >= cache_end {
                self.cache_start =
                    source_frame.div_euclid(cache_frames_i64).saturating_mul(cache_frames_i64);
                self.cache.fill(0.0);
                self.source
                    .read_interleaved(
                        self.cache_start,
                        self.cache_frames,
                        &mut self.cache,
                        cancellation,
                    )
                    .map_err(AudioExecutionError::SourceUnavailable)?;
            }
            copy_interleaved_frame(
                &self.cache,
                usize::try_from(source_frame - self.cache_start)
                    .map_err(|_| AudioExecutionError::BufferTooLarge)?,
                destination,
                output_frame,
                channels,
            )?;
        }
        Ok(())
    }
}

impl NestedRuntimeSource {
    fn begin_continuity(&mut self) {
        self.cache_start = i64::MIN;
        self.next_sample = None;
        self.last_demanded_sample = None;
    }

    fn read_indexed(
        &mut self,
        edit: AudioComponentEditId,
        source_frames: &[i64],
        channel_layout: AudioChannelLayout,
        destination: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        if channel_layout != self.channel_layout {
            return Err(AudioExecutionError::SourceUnavailable(format!(
                "nested audio layout contract changed from {:?} to {channel_layout:?}",
                self.channel_layout
            )));
        }
        let channels = channel_layout.channel_count();
        self.validate_demand_order(edit, source_frames)?;
        if source_frames.len() > self.ordered_output_frames.capacity() {
            return Err(AudioExecutionError::BlockTooLarge);
        }
        self.ordered_output_frames.clear();
        self.ordered_output_frames.extend(source_frames.iter().enumerate().filter_map(
            |(output_frame, source_frame)| (*source_frame >= 0).then_some(output_frame),
        ));
        self.ordered_output_frames
            .sort_unstable_by_key(|output_frame| source_frames[*output_frame]);

        for order_index in 0..self.ordered_output_frames.len() {
            let output_frame = self.ordered_output_frames[order_index];
            let source_frame = source_frames[output_frame];
            self.ensure_cached(source_frame, cancellation)?;
            copy_interleaved_frame(
                &self.cache,
                usize::try_from(source_frame - self.cache_start)
                    .map_err(|_| AudioExecutionError::BufferTooLarge)?,
                destination,
                output_frame,
                channels,
            )?;
        }
        if let Some(last_demanded_sample) =
            source_frames.iter().rev().copied().find(|sample| *sample >= 0)
        {
            self.last_demanded_sample = Some(last_demanded_sample);
        }
        Ok(())
    }

    fn validate_demand_order(
        &self,
        edit: AudioComponentEditId,
        source_frames: &[i64],
    ) -> Result<(), AudioExecutionError> {
        if !self.runtime.requires_state_entry() {
            return Ok(());
        }
        let mut previous = self.last_demanded_sample;
        for sample in source_frames.iter().copied().filter(|sample| *sample >= 0) {
            if previous.is_some_and(|previous| sample < previous) {
                return Err(AudioExecutionError::UnsupportedNestedStateDirection(edit));
            }
            previous = Some(sample);
        }
        Ok(())
    }

    fn ensure_cached(
        &mut self,
        source_frame: i64,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        let cache_frames_i64 =
            i64::try_from(self.cache_frames).map_err(|_| AudioExecutionError::BufferTooLarge)?;
        let cache_end = self.cache_start.checked_add(cache_frames_i64).unwrap_or(i64::MAX);
        if source_frame >= self.cache_start && source_frame < cache_end {
            return Ok(());
        }

        if !self.runtime.requires_state_entry() {
            return self.render_cache(source_frame, cancellation);
        }

        match self.next_sample {
            Some(next_sample) if source_frame >= next_sample => {}
            Some(_) | None => self.enter_child(source_frame)?,
        }
        while self.next_sample.is_some_and(|next_sample| source_frame >= next_sample) {
            let start_sample =
                self.next_sample.ok_or(AudioExecutionError::InvalidPreparedSchedule)?;
            self.render_cache(start_sample, cancellation)?;
        }
        Ok(())
    }

    fn enter_child(&mut self, start_sample: i64) -> Result<(), AudioExecutionError> {
        let epoch = AudioContinuityEpoch::new(self.next_epoch);
        self.next_epoch = self
            .next_epoch
            .checked_add(1)
            .ok_or(AudioExecutionError::ContinuityEpochExhausted)?;
        self.cache_start = i64::MIN;
        self.next_sample = None;
        self.runtime.enter_state(AudioStateEntry { epoch, start_sample })?;
        self.next_sample = Some(start_sample);
        Ok(())
    }

    fn render_cache(
        &mut self,
        start_sample: i64,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), AudioExecutionError> {
        self.cache_start = i64::MIN;
        self.cache.fill(0.0);
        let result = self.runtime.render_into_cancellable(
            AudioRenderRequest { start_sample, frames: self.cache_frames },
            &mut self.cache,
            cancellation,
        );
        if let Err(error) = result {
            self.next_sample = None;
            return Err(error);
        }
        self.cache_start = start_sample;
        if self.runtime.requires_state_entry() {
            self.next_sample = Some(
                start_sample
                    .checked_add(
                        i64::try_from(self.cache_frames)
                            .map_err(|_| AudioExecutionError::BufferTooLarge)?,
                    )
                    .ok_or(AudioExecutionError::BufferTooLarge)?,
            );
        }
        Ok(())
    }
}

fn copy_interleaved_frame(
    source: &[f32],
    source_frame: usize,
    destination: &mut [f32],
    destination_frame: usize,
    channels: usize,
) -> Result<(), AudioExecutionError> {
    let source_start =
        source_frame.checked_mul(channels).ok_or(AudioExecutionError::BufferTooLarge)?;
    let source_end =
        source_start.checked_add(channels).ok_or(AudioExecutionError::BufferTooLarge)?;
    let destination_start = destination_frame
        .checked_mul(channels)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let destination_end = destination_start
        .checked_add(channels)
        .ok_or(AudioExecutionError::BufferTooLarge)?;
    let source = source.get(source_start..source_end).ok_or_else(|| {
        AudioExecutionError::SourceUnavailable("audio source cache extent is invalid".to_owned())
    })?;
    let destination = destination
        .get_mut(destination_start..destination_end)
        .ok_or(AudioExecutionError::OutputSizeMismatch)?;
    destination.copy_from_slice(source);
    Ok(())
}
/// Failure before a render Session is ready. No partial/fallback graph is returned.
#[derive(Debug, thiserror::Error)]
pub enum AudioRuntimeBuildError {
    /// Authoring-to-execution compile failure.
    #[error(transparent)]
    Compile(#[from] crate::AudioCompileError),
    /// Scratch/session preparation failure.
    #[error(transparent)]
    Execution(#[from] AudioExecutionError),
    /// Conservative allocation-footprint calculation overflowed.
    #[error(transparent)]
    ResourceFootprint(#[from] crate::AudioResourceFootprintError),
    /// The complete nested closure exceeds its frozen hard resource grant.
    #[error(
        "audio runtime {category:?} for Sequence {sequence_id} output {output_id} requires {required}, grant is {granted}"
    )]
    ResourceGrantExceeded {
        category: AudioRuntimeResourceCategory,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
        required: usize,
        granted: usize,
    },
    /// A runtime source/session extent cannot be represented on this target.
    #[error(
        "audio runtime {category:?} extent overflowed for Sequence {sequence_id} output {output_id}"
    )]
    ResourceExtentOverflow {
        category: AudioRuntimeResourceCategory,
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
    },
    /// The selected Sequence has no public output.
    #[error("Sequence {0} has no audio Program Output")]
    MissingProgramOutput(SequenceId),
    /// A Sequence Program must execute in its authored semantic layout; device
    /// or export adaptation belongs after the selected public output.
    #[error(
        "Sequence {sequence_id} audio layout is {authored}, but preparation requested {prepared}"
    )]
    ProgramLayoutMismatch {
        sequence_id: SequenceId,
        authored: AudioChannelLayout,
        prepared: AudioChannelLayout,
    },
    /// The frozen selected-range closure has no exact semantic Program for one
    /// required root or nested occurrence.
    #[error(
        "prepared audio closure is missing Sequence {sequence_id} output {output_id} occurrence"
    )]
    MissingPreparedProgram {
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
    },
    /// A nested author reference cannot be resolved.
    #[error("nested Sequence {0} is unavailable")]
    MissingNestedSequence(SequenceId),
    /// Nested author references form a cycle.
    #[error("cyclic nested Sequence reference at {0}")]
    NestedCycle(SequenceId),
    /// Nested author references exceed the shared preview/export recursion contract.
    #[error("nested Sequence {sequence_id} exceeds maximum render depth {maximum}")]
    NestedDepthExceeded {
        sequence_id: SequenceId,
        maximum: usize,
    },
    /// Generic child processor state has no proven reverse-evaluation contract.
    #[error("nested contribution {0} cannot evaluate stateful audio in reverse")]
    UnsupportedStatefulNestedDirection(AudioComponentEditId),
    /// The consumer media Adapter failed to bind a stable component.
    #[error("audio media component {component_id} of Asset {asset_id} is unavailable: {reason}")]
    Media {
        asset_id: AssetId,
        component_id: AudioSourceComponentId,
        reason: String,
    },
}
