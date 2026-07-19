use crate::schedule::{AudioKernelBackend, AudioPreparationDependencies};
use crate::{
    compile_audio_program, AudioCompileRequest, AudioContinuityEpoch, AudioExecutionError,
    AudioPcmSource, AudioRenderContract, AudioRenderRequest, AudioRenderSession, AudioStateEntry,
    CompiledAudioSource, PreparedAudioPlan,
};
use mondrian_core::{
    AssetId, AudioChannelLayout, AudioComponentEditId, AudioSourceComponentId,
    ExecutionCancellationToken, ProgramOutputId, SequenceId,
};
use mondrian_timeline::{sequence::MAX_NESTED_SEQUENCE_RENDER_DEPTH, Sequence};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const NESTED_SOURCE_CACHE_FRAMES: usize = 4_096;
const MEDIA_SOURCE_CACHE_FRAMES: usize = 4_096;

/// Consumer-owned decoded PCM exposed at the media/source Adapter Seam.
pub trait AudioDecodedSource: Send + Sync + 'static {
    /// Fill one exact interleaved block on the prepared contract's Evaluation Grid.
    ///
    /// `destination` is pre-zeroed and has exactly `frames` multiplied by the
    /// resolved Render Contract layout's channel count samples.
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

/// Resolve stable authoring source identities into decoded PCM.
pub trait AudioMediaResolver {
    /// Resolve and, when useful, cache one media component at the render contract.
    fn resolve(
        &self,
        asset_id: AssetId,
        component_id: AudioSourceComponentId,
        contract: AudioRenderContract,
    ) -> Result<Arc<dyn AudioDecodedSource>, String>;
}

/// Shared compiled runtime used by Playback, Export, Audition, and Analysis.
///
/// The runtime owns one exclusive mutable Session and recursively compiled
/// child-output Sessions. Consumers only supply the media decode Adapter.
pub struct AudioProgramRuntime {
    session: AudioRenderSession,
    sources: RuntimeSources,
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
        let mut stack = BTreeSet::new();
        Self::build_inner(
            root, sequences, resolver, contract, output_id, &mut stack, 0,
        )
    }

    fn build_inner(
        sequence: &Sequence,
        sequences: &[Sequence],
        resolver: &dyn AudioMediaResolver,
        contract: AudioRenderContract,
        output_id: Option<ProgramOutputId>,
        stack: &mut BTreeSet<SequenceId>,
        depth: usize,
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
        let result = (|| {
            let output_id = output_id
                .or_else(|| sequence.audio_program.outputs.first().map(|output| output.id))
                .ok_or(AudioRuntimeBuildError::MissingProgramOutput(sequence.id))?;
            let program = Arc::new(compile_audio_program(
                sequence,
                AudioCompileRequest::program(output_id),
            )?);
            let nested_cache_frames =
                contract.max_block_frames.clamp(1, NESTED_SOURCE_CACHE_FRAMES);
            let mut entries = BTreeMap::new();
            let mut dependencies = AudioPreparationDependencies::default();
            for contribution in program.contributions() {
                let source = match contribution.source {
                    CompiledAudioSource::Media { asset_id, component_id } => {
                        let source = resolver.resolve(asset_id, component_id, contract).map_err(
                            |reason| AudioRuntimeBuildError::Media {
                                asset_id,
                                component_id,
                                reason,
                            },
                        )?;
                        let cache_frames =
                            contract.max_block_frames.clamp(1, MEDIA_SOURCE_CACHE_FRAMES);
                        RuntimeSource::Media(MediaRuntimeSource {
                            source,
                            cache_start: i64::MIN,
                            cache_frames,
                            cache: vec![0.0; cache_frames * contract.channel_count()],
                            channel_layout: contract.channel_layout,
                        })
                    }
                    CompiledAudioSource::NestedOutput { sequence_id, output_id } => {
                        let child = sequences
                            .iter()
                            .find(|candidate| candidate.id == sequence_id)
                            .ok_or(AudioRuntimeBuildError::MissingNestedSequence(sequence_id))?;
                        let runtime = Self::build_inner(
                            child,
                            sequences,
                            resolver,
                            contract,
                            Some(output_id),
                            stack,
                            depth + 1,
                        )?;
                        if runtime.requires_state_entry()
                            && contribution.source_time_map.speed.scale().numerator() < 0
                        {
                            return Err(
                                AudioRuntimeBuildError::UnsupportedStatefulNestedDirection(
                                    contribution.edit_id,
                                ),
                            );
                        }
                        dependencies.insert_nested_source(
                            contribution.edit_id,
                            runtime.output_latency_frames(),
                            runtime.requires_state_entry(),
                        )?;
                        RuntimeSource::Nested(NestedRuntimeSource {
                            runtime: Box::new(runtime),
                            cache_start: i64::MIN,
                            cache_frames: nested_cache_frames,
                            cache: vec![0.0; nested_cache_frames * contract.channel_count()],
                            channel_layout: contract.channel_layout,
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
            )?);
            Ok(Self {
                session: AudioRenderSession::new(plan)?,
                sources: RuntimeSources {
                    entries,
                    cancellation: ExecutionCancellationToken::new(),
                },
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

    /// Total prepared latency of this selected public output.
    pub fn output_latency_frames(&self) -> usize {
        self.session.output_latency_frames()
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
    /// The selected Sequence has no public output.
    #[error("Sequence {0} has no audio Program Output")]
    MissingProgramOutput(SequenceId),
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
