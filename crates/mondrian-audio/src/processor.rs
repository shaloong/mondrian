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

    /// Add Adapter-owned Session storage to the admitted scratch obligation.
    ///
    /// Process-isolated Adapters use this to account for fixed shared audio and
    /// parameter exchange storage without exposing their transport layout to
    /// the semantic Processor contract.
    pub fn with_additional_session_scratch_bytes(
        self,
        additional_bytes: usize,
    ) -> Result<Self, AudioProcessorHostError> {
        let session_scratch_bytes =
            self.session_scratch_bytes.checked_add(additional_bytes).ok_or_else(|| {
                AudioProcessorHostError::InvalidContract(
                    "processor Session scratch obligation overflowed".to_owned(),
                )
            })?;
        Ok(Self { session_scratch_bytes, ..self })
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
    /// The exact native parameter source has no exclusive publication role.
    /// This callback-path refusal owns no diagnostic String or allocation.
    #[error("native parameter source publication is unavailable")]
    NativeParameterSourceUnavailable,
    /// Dedicated native DSP scheduling admission or same-thread restoration failed.
    #[error(transparent)]
    NativeScheduling(#[from] mondrian_audio_scheduling::AudioThreadSchedulingError),
    /// The known native worker lane request or actual owner failed admission.
    #[error(transparent)]
    NativeWorkerLaneResource(#[from] crate::NativeWorkerLaneResourceError),
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
    /// A supervised external Worker exceeded its admitted operation deadline.
    #[error("isolated audio processor Worker exceeded its {operation} deadline")]
    WorkerDeadlineExceeded {
        /// Stable operation class whose deadline expired.
        operation: &'static str,
    },
    /// A supervised external Worker crashed, disconnected, or violated protocol.
    #[error("isolated audio processor Worker failed during {operation}: {detail}")]
    WorkerFailed {
        /// Stable operation class being executed.
        operation: &'static str,
        /// Bounded diagnostic or protocol detail.
        detail: String,
    },
    /// A legal native request is retained; the next independent window is deferred.
    #[error("native audio processor requires owner reprepare: {0:?}")]
    ReprepareRequired(mondrian_core::NativeAudioReprepareRequest),
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

/// Exact execution ticket retained by one Session before nonRT owner service.
///
/// Shared author IDs are insufficient when nested Programs or processing Scopes
/// materialize more than once. Both immutable preparation and exclusive Session
/// identities must match before a future candidate can replace this occurrence.
/// This is transient execution evidence, never an author edit or DSP checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioProcessorReprepareTicket {
    /// Immutable plan whose exact PDC/port contract is pending replacement.
    pub prepared_plan_id: mondrian_core::AudioPreparedPlanId,
    /// Unique live Session, even when another Session shares the same Plan Arc.
    pub session_id: mondrian_core::AudioRenderSessionId,
    /// Exact local generated owner/rack/author instance within this Session.
    pub occurrence: AudioProcessorOccurrence,
    /// Continuity identity observed at the deferred boundary, when entered.
    pub continuity_epoch: Option<crate::AudioContinuityEpoch>,
    /// Actual standard request retained by the live native child.
    pub request: mondrian_core::NativeAudioReprepareRequest,
}

impl AudioProcessorReprepareTicket {
    /// Minimal product notice; not authority to select or mutate a materialization.
    pub const fn product_notice(self) -> mondrian_core::NativeAudioReprepareNotice {
        mondrian_core::NativeAudioReprepareNotice {
            instance_id: self.occurrence.instance_id,
            request: self.request,
        }
    }
}

impl std::fmt::Display for AudioProcessorReprepareTicket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} in Session {} / Plan {}",
            self.product_notice(),
            self.session_id,
            self.prepared_plan_id
        )
    }
}

/// Immutable data offered to a processor resolver during plan preparation.
pub struct AudioProcessorPrepareRequest<'a> {
    occurrence: AudioProcessorOccurrence,
    definition: &'a AudioProcessorDefinitionRef,
    parameters: &'a BTreeMap<ParameterId, AudioProcessorParameter>,
    opaque_state: Option<&'a [u8]>,
    render_contract: AudioRenderContract,
    note_input_requested: bool,
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
            note_input_requested: false,
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

    pub(crate) const fn with_note_input_requested(mut self, requested: bool) -> Self {
        self.note_input_requested = requested;
        self
    }

    /// Whether this occurrence has selected authored Notes to bind. Metadata
    /// discovery alone remains separate from actual Note execution admission.
    pub const fn note_input_requested(&self) -> bool {
        self.note_input_requested
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
    /// Visit each actual nested immutable factory allocation without loading,
    /// creating, entering or mutating a processor. The prepared binding separately
    /// reports this factory's Arc payload and its own auxiliary metadata.
    ///
    /// Returning Unavailable is an explicit missing observer, never zero storage.
    /// Visitors must preserve each coverage kind, propagate every rejection and
    /// retain owner borrows through admission. This is non-realtime observation,
    /// not a grant or proof of complete std/SDK/foreign process residency.
    fn visit_host_owned_storage<'a>(
        &'a self,
        _visitor: &mut dyn FnMut(
            crate::AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), crate::AudioProcessorFactoryStorageError>,
    ) -> Result<(), crate::AudioProcessorFactoryStorageError> {
        Err(crate::AudioProcessorFactoryStorageError::Unavailable)
    }

    /// Fixed latency, continuity, and processing-mode admission facts.
    fn execution_contract(&self) -> AudioProcessorExecutionContract;

    /// Stable auxiliary-input buses negotiated for this Render Contract.
    ///
    /// The default is an exact empty contract. Adapters must return the same
    /// value through Session creation; a prepared sidechain may target only a
    /// declared key and exact semantic layout.
    fn auxiliary_input_contract(&self) -> AudioProcessorAuxiliaryInputContract {
        AudioProcessorAuxiliaryInputContract::default()
    }

    /// Concrete Note inputs and fixed limits for this exact restored instance.
    /// The empty default admits no Notes. Native adapters must preserve this
    /// contract through state seeding and validate it on actual creation.
    fn note_input_contract(&self) -> crate::AudioProcessorNoteInputContract {
        crate::AudioProcessorNoteInputContract::default()
    }

    /// Optional control-side native names keyed by admitted auxiliary buses.
    /// Names never enter execution-contract equality or per-block processing.
    fn auxiliary_input_names(&self) -> Vec<AudioProcessorInputName> {
        Vec::new()
    }

    /// Bind reusable native configuration to a new immutable factory with the
    /// same already prepared execution, buses, parameter lanes and render mode.
    ///
    /// This is a pure non-realtime payload operation: it must not create, probe,
    /// enter, stop or mutate a native instance. The caller retains responsibility
    /// for exact author/source admission, configuration provenance, old/new peak
    /// resources and actual Plan publication. Configuration is not a DSP checkpoint.
    /// Future instance creation still validates the real activated contract.
    /// `None` is an unsupported capability and cannot authorize a handoff.
    fn with_native_configuration_seed(
        &self,
        _format: crate::NativeConfigurationFormat,
        _configuration: &[u8],
    ) -> Result<Option<Arc<dyn AudioProcessorFactory>>, AudioProcessorHostError> {
        Ok(None)
    }
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
    /// Whether this exact instance has an authenticated native observation lane.
    /// This read-only capability performs no foreign/native calls.
    fn native_observations_supported(&self) -> bool {
        false
    }

    /// Observe the actual retained native child/object without loading or DSP.
    /// The isolation adapter checks the retained supervisor process handle;
    /// an authored ID, class name or seed ticket cannot synthesize this fact.
    /// None means no supported native-owner adapter, not an installed lease.
    fn native_audio_object_observation(
        &mut self,
    ) -> Result<Option<crate::NativeAudioObjectObservation>, AudioProcessorHostError> {
        Ok(None)
    }

    /// Non-realtime observation on the existing authenticated native-owner stream.
    /// Capability preparation preserves pending logs; the serial diagnostic
    /// consumer drains a fixed batch. Diagnostics do not authorize author work.
    fn poll_native_observations(
        &mut self,
        _consumption: crate::NativeAudioObservationConsumption,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        Ok(crate::NativeAudioObservations {
            object: self.native_audio_object_observation()?,
            ..Default::default()
        })
    }

    /// Observe native object, cleanup and pending configuration before a root
    /// DSP window without consuming callbacks or diagnostics. Generic adapters
    /// retain both existing observation/query semantics; the isolated adapter
    /// overrides this seam with one actual main-thread Poll9 round trip under
    /// its configured process deadline. None cannot clear a Session ticket.
    fn preflight_native_observations(
        &mut self,
        expected_object: crate::NativeAudioObjectObservation,
    ) -> Result<crate::NativeAudioObservations, AudioProcessorHostError> {
        let mut observations =
            self.poll_native_observations(crate::NativeAudioObservationConsumption::Preserve)?;
        if observations.object == Some(expected_object) && observations.editor_cleanup.is_none() {
            observations.pending_reprepare =
                self.preflight_reprepare()?.or(observations.pending_reprepare);
        }
        Ok(observations)
    }

    /// Confirm the exact actual object on the existing authenticated stream.
    /// This non-RT acknowledgement alone grants neither installed ownership
    /// nor author permission; the sole current Runtime must publish its record.
    fn bind_native_audio_object(
        &mut self,
        _request: crate::NativeAudioObjectBindRequest,
    ) -> Result<crate::NativeAudioObjectBindOutcome, AudioProcessorHostError> {
        Ok(crate::NativeAudioObjectBindOutcome::Refused(
            crate::NativeAudioObjectBindRefusal::UnsupportedAdapter,
        ))
    }

    /// Attach this actual bound native object's view after DSP join.
    /// The sole Runtime has already admitted its installed owner lease.
    fn open_native_audio_editor(
        &mut self,
        _request: crate::NativeAudioEditorOpenRequest,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        Ok(crate::NativeAudioEditorOutcome::Refused(
            crate::NativeAudioEditorRefusal::UnsupportedAdapter,
        ))
    }

    /// Close this same bound view; native teardown failure retains its owner.
    fn close_native_audio_editor(
        &mut self,
        _binding: crate::NativeAudioObjectBindRequest,
    ) -> Result<crate::NativeAudioEditorOutcome, AudioProcessorHostError> {
        Ok(crate::NativeAudioEditorOutcome::Refused(
            crate::NativeAudioEditorRefusal::UnsupportedAdapter,
        ))
    }

    /// Prepare a fresh native incarnation on this same child after DSP/output
    /// quiescence. The caller still needs whole-root resource/author/Source
    /// admission. Success does not enter a sample or authorize an editor.
    fn prepare_native_author_state_entry(
        &mut self,
        _request: crate::NativeAudioAuthorStateEntryRequest<'_>,
    ) -> Result<crate::NativeAudioAuthorStateEntryOutcome, AudioProcessorHostError> {
        Ok(crate::NativeAudioAuthorStateEntryOutcome::Refused(
            crate::NativeAudioAuthorStateEntryRefusal::UnsupportedAdapter,
        ))
    }

    /// Snapshot an exact native callback cohort on this retained owner, without servicing it.
    ///
    /// This non-RT control query is separate from root realtime preflight. Its
    /// result must remain associated with the same exclusive processor object.
    fn native_configuration_cohort(
        &mut self,
    ) -> Result<Option<crate::NativeRequestCohort>, AudioProcessorHostError> {
        Ok(None)
    }

    /// Run bounded native control after quiescence, returning an uninstalled candidate.
    ///
    /// The caller must validate exact occurrence, plan, session and epoch before
    /// a later installation, recompile canonical PDC and admit the entire known
    /// owner peak. The candidate is configuration, not a DSP checkpoint; this
    /// command neither resumes playback nor commits an author edit or Undo item.
    fn service_native_configuration(
        &mut self,
        _request: crate::NativeConfigurationControlRequest<'_>,
    ) -> Result<crate::NativeConfigurationControlOutcome, AudioProcessorHostError> {
        Ok(crate::NativeConfigurationControlOutcome::CapabilityRefused(
            crate::NativeConfigurationRefusal::UnsupportedAdapter,
        ))
    }

    /// Stage an exact same-child configuration seed while DSP/output are quiesced.
    ///
    /// Accepted storage is one additional accounted child owner. It does not
    /// install a root plan, resume audio, authorize Undo or preserve private DSP.
    fn stage_native_configuration_seed(
        &mut self,
        _request: crate::NativeConfigurationSeedRequest<'_>,
    ) -> Result<crate::NativeConfigurationSeedOutcome, AudioProcessorHostError> {
        Ok(crate::NativeConfigurationSeedOutcome::Refused(
            crate::NativeConfigurationSeedRefusal::UnsupportedAdapter,
        ))
    }

    /// Commit only the exact preowned seed on this exclusive child processor.
    ///
    /// The caller still needs its current occurrence/plan/session/epoch lease,
    /// complete resource/PDC admission and paused graph installation. This child
    /// acknowledgement is not playback completion or a continuous DSP resume.
    fn commit_native_configuration_seed(
        &mut self,
        _ticket: crate::NativeConfigurationSeedTicket,
    ) -> Result<crate::NativeConfigurationSeedOutcome, AudioProcessorHostError> {
        Ok(crate::NativeConfigurationSeedOutcome::Refused(
            crate::NativeConfigurationSeedRefusal::UnsupportedAdapter,
        ))
    }

    /// Check pending native requests before admitting a complete root window.
    ///
    /// Native isolation uses its bounded process deadline for a fixed pending
    /// query. This calls no DSP/metadata/lifecycle operation and consumes no flag.
    /// A successful notice retains the instance and is distinct from a fault.
    fn preflight_reprepare(
        &mut self,
    ) -> Result<Option<mondrian_core::NativeAudioReprepareRequest>, AudioProcessorHostError> {
        Ok(None)
    }

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
    admitted_root_window: bool,
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
            admitted_root_window: false,
        }
    }

    pub(crate) const fn admitted_root_window(mut self) -> Self {
        self.admitted_root_window = true;
        self
    }

    pub(crate) const fn has_root_window_admission(self) -> bool {
        self.admitted_root_window
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

/// Simultaneous disjoint main/auxiliary bus borrow for one processor callback.
///
/// This is the mutation-safe Interface for sidechain DSP: processors can read
/// one auxiliary bus while modifying the in-place main bus without copying or
/// allocating on the realtime thread.
pub struct AudioProcessorMainAndInputBuses<'a> {
    /// Negotiated main-bus layout.
    pub main_layout: AudioChannelLayout,
    /// Exact frame count shared by both buses.
    pub frames: usize,
    /// Mutable in-place main-bus PCM.
    pub main_interleaved: &'a mut [f32],
    /// Read-only keyed auxiliary input.
    pub auxiliary: AudioProcessorInputBus<'a>,
}

/// One stable processor auxiliary-input bus negotiated during preparation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioProcessorAuxiliaryInputBusContract {
    /// Definition-owned stable key used by author Routes and callback lookup.
    pub bus_key: String,
    /// Exact semantic layout accepted by this bus.
    pub channel_layout: AudioChannelLayout,
}

/// Control-side native name independent of a bus's routing identity and layout.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioProcessorInputName {
    /// Exact stable key in the auxiliary input contract.
    pub bus_key: String,
    /// Native text suitable for one control row.
    pub name: String,
}

impl AudioProcessorInputName {
    /// Capture valid native text; absent or invalid names use the key in the UI.
    pub fn capture(bus_key: impl Into<String>, name: &str) -> Option<Self> {
        let name = name.trim();
        Self::valid_name(name).then(|| Self { bus_key: bus_key.into(), name: name.to_owned() })
    }

    fn valid_name(name: &str) -> bool {
        !name.trim().is_empty()
            && name.len() <= 1024
            && !name.chars().any(|character| {
                character.is_control() || matches!(character, '\u{2028}' | '\u{2029}')
            })
    }

    pub(crate) fn validate_all(
        names: &[Self],
        contract: &AudioProcessorAuxiliaryInputContract,
    ) -> Result<(), AudioProcessorHostError> {
        if names.len() > contract.buses.len() {
            return Err(AudioProcessorHostError::InvalidContract(
                "too many auxiliary input names".to_owned(),
            ));
        }
        let mut keys = std::collections::BTreeSet::new();
        for name in names {
            if !contract.buses.iter().any(|bus| bus.bus_key == name.bus_key)
                || !keys.insert(name.bus_key.as_str())
                || !Self::valid_name(&name.name)
            {
                return Err(AudioProcessorHostError::InvalidContract(
                    "invalid auxiliary input presentation metadata".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

/// Complete immutable auxiliary-input contract of one prepared processor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioProcessorAuxiliaryInputContract {
    /// Ordered auxiliary buses exposed by the realized definition.
    pub buses: Vec<AudioProcessorAuxiliaryInputBusContract>,
}

impl AudioProcessorAuxiliaryInputContract {
    pub(crate) fn validate(
        &self,
        main_layout: AudioChannelLayout,
    ) -> Result<(), AudioProcessorHostError> {
        let mut keys = std::collections::BTreeSet::new();
        for bus in &self.buses {
            if bus.bus_key.trim().is_empty()
                || bus.bus_key.len() > 128
                || bus.bus_key.chars().any(char::is_control)
                || !keys.insert(bus.bus_key.as_str())
            {
                return Err(AudioProcessorHostError::InvalidContract(
                    "processor auxiliary bus keys must be unique bounded non-empty strings"
                        .to_owned(),
                ));
            }
            if bus.channel_layout != main_layout {
                return Err(AudioProcessorHostError::InvalidContract(format!(
                    "processor auxiliary bus {} requires {:?}, but this host currently admits exact main-layout auxiliary buses ({main_layout:?})",
                    bus.bus_key, bus.channel_layout
                )));
            }
        }
        Ok(())
    }
}

/// Audio-bus view supplied to one processor callback.
///
/// Current channel-strip inserts expose one in-place main bus. The keyed
/// auxiliary lookup is the stable extension point for sidechains; absent buses
/// return `None` instead of being silently mixed into the main input.
pub trait AudioProcessorAudioIo {
    /// Borrow the admitted Note stream for this same process call. The empty
    /// default preserves parameter/PCM-only consumers. Unsupported adapters
    /// must reject nonempty input before touching PCM or native DSP.
    fn note_events(&self) -> &[crate::AudioNoteEvent] {
        &[]
    }
    /// Retain one copied completion within the prepared output capacity. True
    /// means retained, never discarded. Adapters must report any refusal even
    /// if a plugin ignores its failed output callback. No author mutation or
    /// voice-capacity credit occurs here; the parent applies it after return.
    fn try_push_note_end(&mut self, _event: crate::AudioNoteEndEvent) -> bool {
        false
    }
    /// Negotiated main-bus layout.
    fn main_layout(&self) -> AudioChannelLayout;
    /// Exact frame count.
    fn frames(&self) -> usize;
    /// Mutable in-place main-bus PCM.
    fn main_interleaved(&mut self) -> &mut [f32];
    /// Find one negotiated read-only auxiliary input by stable definition key.
    fn auxiliary_input(&self, bus_key: &str) -> Option<AudioProcessorInputBus<'_>>;
    /// Borrow the mutable main bus and one read-only auxiliary bus together.
    fn main_and_auxiliary_input(
        &mut self,
        bus_key: &str,
    ) -> Option<AudioProcessorMainAndInputBuses<'_>>;
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
