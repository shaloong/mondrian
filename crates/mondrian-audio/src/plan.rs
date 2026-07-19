use mondrian_core::{
    AudioChannelLayout, AudioComponentEditId, AudioProcessingScopeId, ExactAutomationCurve,
    MixBusId, ProgramOutputId, SequenceId, TimelineTime, TimelineTimeRange, TrackId,
};
use mondrian_timeline::audio::{AudioFadeCurve, AudioRoute, AudioTransitionCurve};
use mondrian_timeline::clip::SpeedMap;
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
    pub source_in: TimelineTime,
    /// Exact source delta per component-local delta.
    pub speed: SpeedMap,
}

impl CompiledSourceTimeMap {
    /// Resolve one Sequence time into source-media time.
    pub fn map(
        self,
        sequence_time: TimelineTime,
    ) -> Result<TimelineTime, mondrian_core::TimelineTimeError> {
        let local = sequence_time.checked_sub(self.sequence_start)?;
        let source_local = local.checked_scale(self.speed.scale())?;
        self.source_in.checked_add(source_local)
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

/// Built-in processor operations currently admitted by the common executor.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CompiledProcessor {
    Gain { automation: ExactAutomationCurve },
}

impl CompiledProcessor {
    /// Render-Contract-specific intrinsic latency after processor realization.
    /// Built-in Gain is strictly zero-latency.
    pub(crate) const fn latency_frames(&self) -> usize {
        match self {
            Self::Gain { .. } => 0,
        }
    }

    /// Whether execution owns history that requires an explicit continuity entry.
    pub(crate) const fn requires_state_entry(&self) -> bool {
        self.latency_frames() > 0
            || match self {
                Self::Gain { .. } => false,
            }
    }
}

/// Ordered, immutable processor operations.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CompiledRack {
    pub(crate) processors: Vec<CompiledProcessor>,
}

impl CompiledRack {
    pub(crate) fn latency_frames(&self) -> Option<usize> {
        self.processors.iter().try_fold(0_usize, |latency, processor| {
            latency.checked_add(processor.latency_frames())
        })
    }

    pub(crate) fn requires_state_entry(&self) -> bool {
        self.processors.iter().any(CompiledProcessor::requires_state_entry)
    }
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
    pub(crate) routes: Vec<AudioRoute>,
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
}

impl AudioRenderContract {
    /// Channel count derived from the sole layout authority.
    pub const fn channel_count(self) -> usize {
        self.channel_layout.channel_count()
    }
}
