use mondrian_core::{
    AudioChannelLayout, AudioComponentEditId, AudioProcessingScopeId, AudioProcessorInstanceId,
    ExactAutomationCurve, MixBusId, ProgramOutputId, SequenceId, SourceSampleTarget, TimeScale,
    TimelineTime, TimelineTimeRange, TrackId,
};
use mondrian_timeline::audio::{
    AudioComponentChannelMapping, AudioFadeCurve, AudioProcessorDefinitionRef,
    AudioProcessorParameter, AudioRouteDestination, AudioRouteSource, AudioTransitionCurve,
};
use std::collections::{BTreeMap, BTreeSet};

/// DSP execution contract selected by one consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioProcessingMode {
    /// Deadline-constrained realtime processing.
    Realtime,
    /// Deterministic offline processing.
    Offline,
}

/// Temporary audition selection applied while resolving a Signal Closure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioAuditionOverlay {
    /// When non-empty, only these Track program sources are audible while all
    /// downstream and sidechain dependencies remain resolvable.
    pub soloed_tracks: BTreeSet<TrackId>,
}

/// Semantic compile request. Purpose is intentionally absent from fingerprints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioCompileRequest {
    /// Public Sequence output to resolve.
    pub output_id: ProgramOutputId,
    /// Optional non-program audition overlay.
    pub audition: AudioAuditionOverlay,
}

impl AudioCompileRequest {
    /// Resolve the canonical, non-auditioned Program Output.
    pub fn program(output_id: ProgramOutputId) -> Self {
        Self {
            output_id,
            audition: AudioAuditionOverlay::default(),
        }
    }
}

/// Exact source mapping derived from one immutable Clip placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompiledSourceTimeMap {
    /// Sequence position of component-local zero.
    pub sequence_start: TimelineTime,
    /// Source-media position corresponding to component-local zero.
    pub source_origin: TimelineTime,
    /// Exact source delta per component-local delta.
    pub scale: TimeScale,
    /// Half-open sample ownership retained from the canonical Clip map.
    pub sampling_boundary: mondrian_core::SourceSamplingBoundary,
}

impl CompiledSourceTimeMap {
    /// Resolve one Sequence time into source-media time.
    pub fn map(
        self,
        sequence_time: TimelineTime,
    ) -> Result<TimelineTime, mondrian_core::TimelineTimeError> {
        let local = sequence_time.checked_sub(self.sequence_start)?;
        let source_local = local.checked_scale(self.scale)?;
        self.source_origin.checked_add(source_local)
    }

    /// Resolve one Sequence time into the complete source-sampling contract.
    ///
    /// The exact coordinate and its half-open ownership stay coupled until the
    /// prepared renderer lowers them onto the physical audio sample grid.
    pub fn sample(
        self,
        sequence_time: TimelineTime,
    ) -> Result<SourceSampleTarget, mondrian_core::TimelineTimeError> {
        let time = self.map(sequence_time)?;
        Ok(match self.sampling_boundary {
            mondrian_core::SourceSamplingBoundary::Covering => SourceSampleTarget::covering(time),
            mondrian_core::SourceSamplingBoundary::StrictPredecessor => {
                SourceSampleTarget::strict_predecessor(time)
            }
        })
    }
}

/// Compiled source identity. No Track or time placement is persisted here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompiledAudioSource {
    /// One media Asset audio component.
    Media {
        asset_id: mondrian_core::AssetId,
        component_id: mondrian_core::AudioSourceComponentId,
    },
    /// One child public Sequence output selected by the owning nested Clip.
    NestedOutput {
        sequence_id: SequenceId,
        output_id: ProgramOutputId,
    },
}

/// One immutable semantic processor instance retained from authoring.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompiledProcessor {
    pub(crate) instance_id: AudioProcessorInstanceId,
    pub(crate) definition: AudioProcessorDefinitionRef,
    pub(crate) parameters: BTreeMap<mondrian_core::ParameterId, AudioProcessorParameter>,
    pub(crate) opaque_state: Option<Vec<u8>>,
}

/// Ordered, immutable processor operations.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CompiledRack {
    pub(crate) processors: Vec<CompiledProcessor>,
}

/// Non-placement processing scope compiled from one author definition.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompiledProcessingScope {
    pub(crate) id: AudioProcessingScopeId,
    pub(crate) input_gain_db: f64,
    pub(crate) input_gain_automation: Option<ExactAutomationCurve>,
    pub(crate) rack: CompiledRack,
}

/// One generated PCM-bearing operation derived from a real Clip placement.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledAudioContribution {
    /// Stable author origin.
    pub edit_id: AudioComponentEditId,
    /// Owning Clip placement identity.
    pub clip_id: mondrian_core::ClipId,
    /// Derived owning Track identity.
    pub track_id: TrackId,
    /// Derived Sequence-time audible interval.
    pub sequence_range: TimelineTimeRange,
    /// Derived exact source mapping.
    pub source_time_map: CompiledSourceTimeMap,
    /// Media component or nested public output.
    pub source: CompiledAudioSource,
    /// Author-selected standard or explicit source-to-Sequence channel mapping.
    pub(crate) channel_mapping: AudioComponentChannelMapping,
    /// Referenced non-placement processing definition.
    pub(crate) processing_scope: AudioProcessingScopeId,
    /// Scope-local coordinate at component-local zero.
    pub(crate) scope_in: TimelineTime,
    /// Edit-local automation coordinate at component-local zero.
    pub(crate) local_time_in: TimelineTime,
    pub(crate) volume_db: f64,
    pub(crate) volume_automation: Option<ExactAutomationCurve>,
    pub(crate) pan: f64,
    pub(crate) pan_automation: Option<ExactAutomationCurve>,
    pub(crate) fade_in: Option<(TimelineTime, AudioFadeCurve)>,
    pub(crate) fade_out: Option<(TimelineTime, AudioFadeCurve)>,
}

/// Immutable compiled channel-strip mathematics.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompiledChannelStrip {
    pub(crate) input_trim_db: f64,
    pub(crate) pre_fader: CompiledRack,
    pub(crate) fader_db: f64,
    pub(crate) fader_automation: Option<ExactAutomationCurve>,
    pub(crate) post_fader: CompiledRack,
}

/// Track mixer semantics resolved from the same Sequence snapshot.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompiledTrackChannel {
    pub(crate) muted: bool,
    pub(crate) strip: CompiledChannelStrip,
}

/// One enabled Route edge retained by the selected Signal Closure.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompiledRoute {
    pub(crate) id: mondrian_core::AudioRouteId,
    pub(crate) source: AudioRouteSource,
    pub(crate) destination: AudioRouteDestination,
    pub(crate) gain_db: f64,
    pub(crate) gain_automation: Option<ExactAutomationCurve>,
}

/// One compiled explicit Transition relationship.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompiledTransition {
    pub(crate) left: AudioComponentEditId,
    pub(crate) right: AudioComponentEditId,
    pub(crate) sequence_range: TimelineTimeRange,
    pub(crate) curve: AudioTransitionCurve,
}

/// Context-independent exact-time semantic Signal Closure.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledAudioProgram {
    pub(crate) output_id: ProgramOutputId,
    pub(crate) contributions: Vec<CompiledAudioContribution>,
    pub(crate) processing_scopes: BTreeMap<AudioProcessingScopeId, CompiledProcessingScope>,
    pub(crate) transitions: Vec<CompiledTransition>,
    pub(crate) track_channels: BTreeMap<TrackId, CompiledTrackChannel>,
    pub(crate) buses: BTreeMap<MixBusId, CompiledChannelStrip>,
    pub(crate) bus_order: Vec<MixBusId>,
    pub(crate) output: CompiledChannelStrip,
    pub(crate) routes: Vec<CompiledRoute>,
}

/// Whether one compiled Program Output can be omitted without changing PCM.
///
/// This evidence is deliberately conservative across the processor Adapter
/// Seam. An active processor may retain a tail or generate signal from silent
/// input, and the current processor Interface publishes no silence-preservation
/// proof, so its presence requires canonical execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioProgramExecutionDemand {
    /// The selected Signal Closure is proven to produce only exact silence.
    ProvenSilent,
    /// The selected Signal Closure must be prepared and executed.
    RequiresExecution,
}

impl AudioProgramExecutionDemand {
    /// Whether the canonical Program Output Runtime must execute.
    pub const fn requires_execution(self) -> bool {
        matches!(self, Self::RequiresExecution)
    }
}

impl CompiledAudioProgram {
    /// Public output produced by this Signal Closure.
    pub fn output_id(&self) -> ProgramOutputId {
        self.output_id
    }

    /// Generated contributions and their deterministic author origins.
    pub fn contributions(&self) -> &[CompiledAudioContribution] {
        &self.contributions
    }

    /// Derive conservative execution demand from the selected Signal Closure.
    ///
    /// Disabled sources and mute-gated `PostMute` paths have already been
    /// removed by compilation. Any remaining source Contribution or active
    /// processor can affect the public output. This is the only semantic
    /// evidence consumers may use to replace execution with exact silence.
    pub fn execution_demand(&self) -> AudioProgramExecutionDemand {
        let has_processors =
            self.processing_scopes.values().any(|scope| !scope.rack.processors.is_empty())
                || self.track_channels.values().any(|channel| {
                    !channel.strip.pre_fader.processors.is_empty()
                        || !channel.strip.post_fader.processors.is_empty()
                })
                || self.buses.values().any(|strip| {
                    !strip.pre_fader.processors.is_empty()
                        || !strip.post_fader.processors.is_empty()
                })
                || !self.output.pre_fader.processors.is_empty()
                || !self.output.post_fader.processors.is_empty();
        if self.contributions.is_empty() && !has_processors {
            AudioProgramExecutionDemand::ProvenSilent
        } else {
            AudioProgramExecutionDemand::RequiresExecution
        }
    }
}

/// Render-contract-specific immutable preparation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioRenderContract {
    /// Concrete Evaluation Grid.
    pub sample_rate: u32,
    /// Semantic output layout and canonical interleaving order.
    pub channel_layout: AudioChannelLayout,
    /// Largest block admitted by the Session.
    pub max_block_frames: usize,
    /// Realtime or offline processor contract.
    pub processing_mode: AudioProcessingMode,
    /// Maximum processor-private bytes retained by one prepared Session.
    pub processor_session_scratch_budget_bytes: usize,
    /// Maximum internal frames evaluated before Timeline-aligned public PCM.
    pub public_output_lookahead_budget_frames: usize,
    /// Maximum interleaved PDC storage retained by one prepared Session.
    pub compensation_delay_scratch_budget_bytes: usize,
}

impl AudioRenderContract {
    /// Conservative default admission budget for processor-private Session state.
    pub const DEFAULT_PROCESSOR_SESSION_SCRATCH_BUDGET_BYTES: usize = 256 * 1024 * 1024;
    /// Conservative default admission budget for public-output lookahead.
    pub const DEFAULT_PUBLIC_OUTPUT_LOOKAHEAD_BUDGET_FRAMES: usize = 480_000;
    /// Conservative default admission budget for PDC delay-line storage.
    pub const DEFAULT_COMPENSATION_DELAY_SCRATCH_BUDGET_BYTES: usize = 256 * 1024 * 1024;

    /// Channel count derived from the sole layout authority.
    pub const fn channel_count(self) -> usize {
        self.channel_layout.channel_count()
    }
}
