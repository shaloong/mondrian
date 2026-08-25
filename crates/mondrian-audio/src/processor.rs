//! Render-contract-bound processor hosting and allocation-free parameter delivery.

use crate::{AudioKernelBackend, AudioProcessingMode, AudioRenderContract, AudioRenderRequest};
use mondrian_core::{
    AudioChannelLayout, AudioComponentEditId, AudioProcessingScopeId, AudioProcessorInstanceId,
    AudioSampleRate, MixBusId, ParameterId, ProgramOutputId, TrackId,
};
use mondrian_timeline::audio::{AudioProcessorDefinitionRef, AudioProcessorParameter};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

/// Realized processor behavior that affects scheduling and admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioProcessorExecutionContract {
    algorithmic_latency_frames: usize,
    tail: AudioProcessorTail,
    requires_state_entry: bool,
    realtime_capable: bool,
    offline_capable: bool,
    session_scratch_bytes: usize,
}

impl AudioProcessorExecutionContract {
    /// Construct and validate one realized execution contract.
    pub fn new(
        algorithmic_latency_frames: usize,
        tail: AudioProcessorTail,
        requires_state_entry: bool,
        realtime_capable: bool,
        offline_capable: bool,
        session_scratch_bytes: usize,
    ) -> Result<Self, AudioProcessorHostError> {
        if (!realtime_capable && !offline_capable)
            || ((algorithmic_latency_frames > 0 || tail != AudioProcessorTail::None)
                && !requires_state_entry)
            || matches!(tail, AudioProcessorTail::Finite(0))
        {
            return Err(AudioProcessorHostError::InvalidContract(
                "processor must support at least one mode; algorithmic latency and tail require state entry; finite tail must be non-zero"
                    .to_owned(),
            ));
        }
        Ok(Self {
            algorithmic_latency_frames,
            tail,
            requires_state_entry,
            realtime_capable,
            offline_capable,
            session_scratch_bytes,
        })
    }

    /// Zero-latency stateless processor admitted in realtime and offline modes.
    pub const fn stateless() -> Self {
        Self {
            algorithmic_latency_frames: 0,
            tail: AudioProcessorTail::None,
            requires_state_entry: false,
            realtime_capable: true,
            offline_capable: true,
            session_scratch_bytes: 0,
        }
    }

    /// Hidden group delay/lookahead that the Host must compensate.
    pub const fn algorithmic_latency_frames(self) -> usize {
        self.algorithmic_latency_frames
    }

    /// Meaningful output after input silence, excluding compensated latency.
    pub const fn tail(self) -> AudioProcessorTail {
        self.tail
    }

    /// Whether a fresh continuity epoch must explicitly reset processor state.
    pub const fn requires_state_entry(self) -> bool {
        self.requires_state_entry
    }

    /// Whether the realized Adapter admits deadline-constrained execution.
    pub const fn realtime_capable(self) -> bool {
        self.realtime_capable
    }

    /// Whether the realized Adapter admits deterministic offline execution.
    pub const fn offline_capable(self) -> bool {
        self.offline_capable
    }

    /// Exact processor-private scratch bytes declared for one Session instance.
    pub const fn session_scratch_bytes(self) -> usize {
        self.session_scratch_bytes
    }

    pub(crate) fn admits(self, mode: AudioProcessingMode) -> bool {
        match mode {
            AudioProcessingMode::Realtime => self.realtime_capable,
            AudioProcessingMode::Offline => self.offline_capable,
        }
    }
}

/// Processor output extent after its input becomes silent.
///
/// Tail is distinct from algorithmic latency: latency is hidden group delay
/// aligned by PDC, while tail is audible signal semantics such as delay or
/// reverb decay. External Adapters map their native finite/infinite sentinel to
/// this value during preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioProcessorTail {
    /// No meaningful output after the input interval.
    None,
    /// A finite non-zero number of sample frames.
    Finite(usize),
    /// No finite bound is declared by the Processor.
    Infinite,
}

/// Failure while resolving, instantiating, resetting, or executing a processor.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioProcessorHostError {
    /// No installed Adapter can realize the persistent definition.
    #[error("audio processor is unavailable: {0}")]
    Unavailable(String),
    /// Adapter supplied scheduling facts that cannot be executed safely.
    #[error("invalid audio processor execution contract: {0}")]
    InvalidContract(String),
    /// A prepared factory could not create its exclusive Session instance.
    #[error("audio processor instance creation failed: {0}")]
    InstanceCreation(String),
    /// A processor rejected a required continuity entry.
    #[error("audio processor state entry failed: {0}")]
    StateEntry(String),
    /// A processor failed while executing one admitted block.
    #[error("audio processor block execution failed: {0}")]
    Process(String),
    /// An Adapter unwound across the Host boundary.
    #[error("audio processor adapter panicked during {0}")]
    AdapterPanicked(&'static str),
    /// A prior failure left an exclusive instance in unknown mutable state.
    #[error("audio processor instance is poisoned and must be recreated")]
    PoisonedInstance,
}

/// Exact generated owner of one processor occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioProcessorOccurrenceOwner {
    /// One independent materialization of a shared Component processing Scope.
    Contribution {
        /// Placement-local audio Component edit.
        edit_id: AudioComponentEditId,
        /// Shared immutable processing definition.
        scope_id: AudioProcessingScopeId,
    },
    /// Audio Track channel strip.
    Track(TrackId),
    /// Mix Bus channel strip.
    Bus(MixBusId),
    /// Public Program Output channel strip.
    Output(ProgramOutputId),
}

/// Stable insertion point within one occurrence owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioProcessorInsertionPoint {
    /// Component processing Scope rack.
    Scope,
    /// Channel-strip rack before the fader.
    PreFader,
    /// Channel-strip rack after the fader and before mute.
    PostFader,
}

/// Generated processor address retained through preparation and Session hosting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioProcessorOccurrence {
    /// Stable author instance identity.
    pub instance_id: AudioProcessorInstanceId,
    /// Exact generated owner.
    pub owner: AudioProcessorOccurrenceOwner,
    /// Exact rack insertion point.
    pub insertion: AudioProcessorInsertionPoint,
}

/// Immutable data offered to a processor resolver during plan preparation.
pub struct AudioProcessorPrepareRequest<'a> {
    occurrence: AudioProcessorOccurrence,
    definition: &'a AudioProcessorDefinitionRef,
    parameters: &'a BTreeMap<ParameterId, AudioProcessorParameter>,
    opaque_state: Option<&'a [u8]>,
    render_contract: AudioRenderContract,
}

impl<'a> AudioProcessorPrepareRequest<'a> {
    pub(crate) fn new(
        occurrence: AudioProcessorOccurrence,
        definition: &'a AudioProcessorDefinitionRef,
        parameters: &'a BTreeMap<ParameterId, AudioProcessorParameter>,
        opaque_state: Option<&'a [u8]>,
        render_contract: AudioRenderContract,
    ) -> Self {
        Self {
            occurrence,
            definition,
            parameters,
            opaque_state,
            render_contract,
        }
    }

    /// Stable author instance being realized.
    pub const fn instance_id(&self) -> AudioProcessorInstanceId {
        self.occurrence.instance_id
    }

    /// Exact generated owner and insertion address being realized.
    pub const fn occurrence(&self) -> AudioProcessorOccurrence {
        self.occurrence
    }

    /// Versioned built-in or plugin definition identity.
    pub const fn definition(&self) -> &AudioProcessorDefinitionRef {
        self.definition
    }

    /// Validated parameter schema snapshots and exact curves.
    pub const fn parameters(&self) -> &BTreeMap<ParameterId, AudioProcessorParameter> {
        self.parameters
    }

    /// Opaque author state to restore before any Session block executes.
    pub const fn opaque_state(&self) -> Option<&[u8]> {
        self.opaque_state
    }

    /// Exact sample-rate/layout/block/mode contract being prepared.
    pub const fn render_contract(&self) -> AudioRenderContract {
        self.render_contract
    }
}

/// Non-realtime resolver for built-in or separately hosted processor definitions.
///
/// Discovery, ABI loading, state restoration, bus/layout negotiation, and
/// isolation selection happen here. The resolver must return a factory already
/// bound to the exact Render Contract; it is never called from `render_into`.
pub trait AudioProcessorResolver: Send + Sync {
    /// Resolve one immutable author instance for this plan.
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError>;
}

/// Render-contract-bound factory retained by an immutable prepared plan.
pub trait AudioProcessorFactory: Send + Sync {
    /// Fixed latency, continuity, and processing-mode admission facts.
    fn execution_contract(&self) -> AudioProcessorExecutionContract;

    /// Create one exclusive mutable instance before realtime execution starts.
    fn create(&self) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError>;
}

/// Exclusive mutable processor state owned by exactly one render Session.
///
/// `process` is on the realtime path. Implementations admitted as realtime
/// capable must not allocate, grow containers, lock, perform I/O, wait for
/// another thread/device, panic, or retain borrowed block data. An isolated
/// plugin Adapter owns enforcing its deadline and failure policy.
pub trait AudioProcessor: Send {
    /// Reset all history for a fresh continuity epoch at an exact Sequence sample.
    fn enter_state(&mut self, start_sample: i64) -> Result<(), AudioProcessorHostError>;

    /// Process one exact in-place main bus and its sample-accurate parameters.
    fn process(
        &mut self,
        context: AudioProcessorProcessContext,
        audio: &mut dyn AudioProcessorAudioIo,
        parameters: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError>;
}

/// Exact immutable facts for one processor callback.
#[derive(Debug, Clone, Copy)]
pub struct AudioProcessorProcessContext {
    request: AudioRenderRequest,
    sample_rate: AudioSampleRate,
    channel_layout: AudioChannelLayout,
    processing_mode: AudioProcessingMode,
    kernel_backend: AudioKernelBackend,
}

impl AudioProcessorProcessContext {
    pub(crate) const fn new(
        request: AudioRenderRequest,
        sample_rate: AudioSampleRate,
        channel_layout: AudioChannelLayout,
        processing_mode: AudioProcessingMode,
        kernel_backend: AudioKernelBackend,
    ) -> Self {
        Self {
            request,
            sample_rate,
            channel_layout,
            processing_mode,
            kernel_backend,
        }
    }

    /// Exact Sequence-domain block coordinates.
    pub const fn request(self) -> AudioRenderRequest {
        self.request
    }

    /// Prepared sample grid.
    pub const fn sample_rate(self) -> AudioSampleRate {
        self.sample_rate
    }

    /// Negotiated main-bus semantic layout.
    pub const fn channel_layout(self) -> AudioChannelLayout {
        self.channel_layout
    }

    /// Realtime or deterministic offline admission mode.
    pub const fn processing_mode(self) -> AudioProcessingMode {
        self.processing_mode
    }

    /// CPU reference/SIMD backend chosen by the prepared plan.
    pub const fn kernel_backend(self) -> AudioKernelBackend {
        self.kernel_backend
    }
}

/// Borrowed auxiliary processor input bus.
#[derive(Debug, Clone, Copy)]
pub struct AudioProcessorInputBus<'a> {
    /// Stable definition-owned bus key used by typed sidechain routing.
    pub bus_key: &'a str,
    /// Semantic layout negotiated during preparation.
    pub channel_layout: AudioChannelLayout,
    /// Exact frame count in this block.
    pub frames: usize,
    /// Read-only interleaved PCM.
    pub interleaved: &'a [f32],
}

/// Audio-bus view supplied to one processor callback.
///
/// Current channel-strip inserts expose one in-place main bus. The keyed
/// auxiliary lookup is the stable extension point for sidechains; absent buses
/// return `None` instead of being silently mixed into the main input.
pub trait AudioProcessorAudioIo {
    /// Negotiated main-bus layout.
    fn main_layout(&self) -> AudioChannelLayout;
    /// Exact frame count.
    fn frames(&self) -> usize;
    /// Mutable in-place main-bus PCM.
    fn main_interleaved(&mut self) -> &mut [f32];
    /// Find one negotiated read-only auxiliary input by stable definition key.
    fn auxiliary_input(&self, bus_key: &str) -> Option<AudioProcessorInputBus<'_>>;
}

/// Default resolver for Mondrian built-ins. External definitions fail closed.
#[derive(Debug, Clone, Copy, Default)]
pub struct BuiltInAudioProcessorResolver;

/// One exact definition-domain value change inside a processor block.
///
/// Values are not normalized to a plugin ABI. A concrete VST3, CLAP, GPU, or
/// native Adapter owns that final conversion after it has negotiated the real
/// parameter definition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioParameterEvent {
    /// Zero-based sample-frame offset from the start of the current block.
    pub sample_offset: u32,
    /// Exact value in the parameter's captured definition domain.
    pub value: f64,
}

/// Borrowed sample-accurate parameter events for one generated processor occurrence.
///
/// Lanes follow the immutable prepared parameter order and each lane is sorted
/// by `sample_offset`. Constant parameters contain one event at offset zero;
/// varying curves contain an exact value for every sample frame. The Session
/// owns and reuses the backing storage, so callback execution does not allocate.
#[derive(Debug, Clone, Copy)]
pub struct AudioParameterEventBatch<'a> {
    block_start_sample: i64,
    block_frames: usize,
    parameter_ids: &'a [ParameterId],
    lane_ranges: &'a [Range<usize>],
    events: &'a [AudioParameterEvent],
}

impl<'a> AudioParameterEventBatch<'a> {
    pub(crate) fn new(
        block_start_sample: i64,
        block_frames: usize,
        parameter_ids: &'a [ParameterId],
        lane_ranges: &'a [Range<usize>],
        events: &'a [AudioParameterEvent],
    ) -> Self {
        debug_assert_eq!(parameter_ids.len(), lane_ranges.len());
        Self {
            block_start_sample,
            block_frames,
            parameter_ids,
            lane_ranges,
            events,
        }
    }

    /// First absolute Sequence-domain sample frame represented by this batch.
    pub const fn block_start_sample(self) -> i64 {
        self.block_start_sample
    }

    /// Number of sample frames represented by this batch.
    pub const fn block_frames(self) -> usize {
        self.block_frames
    }

    /// Number of stable parameter lanes.
    pub const fn lane_count(self) -> usize {
        self.parameter_ids.len()
    }

    /// Stable parameter identity at one prepared lane.
    pub fn parameter_id(self, lane: usize) -> Option<&'a ParameterId> {
        self.parameter_ids.get(lane)
    }

    /// Ordered events for one prepared parameter lane.
    pub fn events(self, lane: usize) -> Option<&'a [AudioParameterEvent]> {
        let range = self.lane_ranges.get(lane)?.clone();
        self.events.get(range)
    }

    /// Total event count across all parameter lanes.
    pub const fn event_count(self) -> usize {
        self.events.len()
    }
}
