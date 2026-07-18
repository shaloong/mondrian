use crate::{
    compile_audio_program, AudioCompileRequest, AudioExecutionError, AudioPcmSource,
    AudioRenderContract, AudioRenderRequest, AudioRenderSession, CompiledAudioSource,
    PreparedAudioPlan,
};
use mondrian_core::{
    AssetId, AudioComponentEditId, AudioSourceComponentId, ExecutionCancellationToken,
    ProgramOutputId, SequenceId,
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
    /// `destination` is pre-zeroed and has exactly `frames * channels` samples.
    /// Implementations must preserve silence outside the source range and return
    /// an error rather than publish a partial or shifted block.
    fn read_interleaved(
        &self,
        start_frame: i64,
        frames: usize,
        channels: usize,
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
                            cache: vec![0.0; cache_frames * contract.channels],
                            channels: contract.channels,
                        })
                    }
                    CompiledAudioSource::NestedOutput { sequence_id, output_id } => {
                        let child = sequences
                            .iter()
                            .find(|candidate| candidate.id == sequence_id)
                            .ok_or(AudioRuntimeBuildError::MissingNestedSequence(sequence_id))?;
                        RuntimeSource::Nested(NestedRuntimeSource {
                            runtime: Box::new(Self::build_inner(
                                child,
                                sequences,
                                resolver,
                                contract,
                                Some(output_id),
                                stack,
                                depth + 1,
                            )?),
                            cache_start: i64::MIN,
                            cache_frames: nested_cache_frames,
                            cache: vec![0.0; nested_cache_frames * contract.channels],
                            channels: contract.channels,
                        })
                    }
                };
                entries.insert(contribution.edit_id, source);
            }
            let plan = Arc::new(PreparedAudioPlan::prepare(program, contract)?);
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

enum RuntimeSource {
    Media(MediaRuntimeSource),
    Nested(NestedRuntimeSource),
}

struct MediaRuntimeSource {
    source: Arc<dyn AudioDecodedSource>,
    cache_start: i64,
    cache_frames: usize,
    cache: Vec<f32>,
    channels: usize,
}

struct NestedRuntimeSource {
    runtime: Box<AudioProgramRuntime>,
    cache_start: i64,
    cache_frames: usize,
    cache: Vec<f32>,
    channels: usize,
}

impl AudioPcmSource for RuntimeSources {
    fn sample(
        &mut self,
        edit: AudioComponentEditId,
        source_frame: i64,
        channel: usize,
    ) -> Result<f32, AudioExecutionError> {
        if source_frame < 0 {
            return Ok(0.0);
        }
        let source = self.entries.get_mut(&edit).ok_or_else(|| {
            AudioExecutionError::SourceUnavailable(format!("component edit {edit}"))
        })?;
        match source {
            RuntimeSource::Media(media) => {
                let cache_frames_i64 = i64::try_from(media.cache_frames).unwrap_or(i64::MAX);
                let cache_end = media.cache_start.saturating_add(cache_frames_i64);
                if source_frame < media.cache_start || source_frame >= cache_end {
                    media.cache_start =
                        source_frame.div_euclid(cache_frames_i64).saturating_mul(cache_frames_i64);
                    media.cache.fill(0.0);
                    media
                        .source
                        .read_interleaved(
                            media.cache_start,
                            media.cache_frames,
                            media.channels,
                            &mut media.cache,
                            &self.cancellation,
                        )
                        .map_err(AudioExecutionError::SourceUnavailable)?;
                }
                let local_frame =
                    usize::try_from(source_frame - media.cache_start).map_err(|_| {
                        AudioExecutionError::SourceUnavailable(
                            "decoded media cache coordinate overflow".to_owned(),
                        )
                    })?;
                Ok(media
                    .cache
                    .get(local_frame.saturating_mul(media.channels).saturating_add(channel))
                    .copied()
                    .unwrap_or(0.0))
            }
            RuntimeSource::Nested(nested) => {
                let cache_end = nested
                    .cache_start
                    .saturating_add(i64::try_from(nested.cache_frames).unwrap_or(i64::MAX));
                if source_frame < nested.cache_start || source_frame >= cache_end {
                    nested.cache_start = source_frame;
                    nested.runtime.render_into_cancellable(
                        AudioRenderRequest {
                            start_sample: nested.cache_start,
                            frames: nested.cache_frames,
                        },
                        &mut nested.cache,
                        &self.cancellation,
                    )?;
                }
                let frame = usize::try_from(source_frame - nested.cache_start)
                    .map_err(|_| AudioExecutionError::BufferTooLarge)?;
                let index = frame
                    .checked_mul(nested.channels)
                    .and_then(|base| base.checked_add(channel.min(nested.channels - 1)))
                    .ok_or(AudioExecutionError::BufferTooLarge)?;
                Ok(nested.cache.get(index).copied().unwrap_or(0.0))
            }
        }
    }
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
    /// The consumer media Adapter failed to bind a stable component.
    #[error("audio media component {component_id} of Asset {asset_id} is unavailable: {reason}")]
    Media {
        asset_id: AssetId,
        component_id: AudioSourceComponentId,
        reason: String,
    },
}
