//! Ordered native author source data on the sole native observation stream.
//!
//! Source facts, field-encoded callbacks and child acknowledgements grant no
//! author or installed-runtime permission. The actual Hosted record retains an
//! outstanding delivery until the sole author Session and parameter-plan handoff
//! have accepted it. Source emptiness cannot erase that barrier.

use crate::NativeAudioObjectBindRequest;
use mondrian_core::ParameterId;

/// Maximum callbacks retained in one complete native author delivery.
pub const NATIVE_AUDIO_AUTHOR_EVENTS: usize = 4096;
/// Maximum component/controller configuration bytes in an author delivery.
pub const NATIVE_AUDIO_AUTHOR_CONFIGURATION_BYTES: usize = 128 * 1024;

/// Lifetime-sticky incompleteness reported by the actual native callback source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioAuthorSourceFailure {
    /// A normative author callback arrived outside the recorded main thread.
    WrongThread,
    /// The sole ordered source or existing DSP feedback sink exhausted capacity.
    Overflow,
    /// A callback supplied a non-finite or illegal normalized value.
    InvalidValue,
    /// An End callback has no corresponding actual Begin boundary.
    InvalidBoundary,
    /// The non-wrapping actual callback revision was exhausted.
    SequenceExhausted,
    /// A bounded void callback could not publish its actual source order.
    Contention,
    /// Unknown control flags or an invalid native lifecycle made the source incomplete.
    InvalidSource,
}

/// Non-consuming facts observed from one actual component-handler source.
///
/// Parameter edit counts include repeated Begin calls for the same parameter.
/// A group is a timestamp scope and may close while parameter edits remain open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioAuthorSourceStatus {
    pub(crate) revision: u64,
    pub(crate) pending_events: u32,
    pub(crate) pending_parameter_observations: u32,
    pub(crate) pending_dsp_feedback_values: u32,
    pub(crate) open_parameter_edits: u32,
    pub(crate) group_depth: u32,
    pub(crate) failure: Option<NativeAudioAuthorSourceFailure>,
    /// Actual plugin flush request not yet claimed by its exclusive DSP role.
    /// This is a scheduling bit, not an invented feedback-value count.
    pub(crate) parameter_flush_pending: bool,
    /// Producer publication prevented an exact non-consuming status snapshot.
    pub(crate) publishing: bool,
}

impl NativeAudioAuthorSourceStatus {
    /// Non-wrapping revision of successfully retained callbacks in this object.
    pub const fn revision(self) -> u64 {
        self.revision
    }
    /// Callback events not consumed by the canonical native observer.
    pub const fn pending_events(self) -> u32 {
        self.pending_events
    }
    /// Accepted nonauthor parameter observations still retained by this object.
    /// Readonly/DONT_RECORD callbacks do not become author edits. They must be
    /// delivered before a lifecycle operation can erase the owning source.
    pub const fn pending_parameter_observations(self) -> u32 {
        self.pending_parameter_observations
    }
    /// Actual values remaining in the existing native editor-to-DSP sink.
    pub const fn pending_dsp_feedback_values(self) -> u32 {
        self.pending_dsp_feedback_values
    }
    /// Begin calls not yet matched by End, including repeated parameter IDs.
    pub const fn open_parameter_edits(self) -> u32 {
        self.open_parameter_edits
    }
    /// Shared-timestamp groups not yet matched by their group finish callback.
    pub const fn group_depth(self) -> u32 {
        self.group_depth
    }
    /// Source failure remains present after drains or empty callbacks.
    pub const fn failure(self) -> Option<NativeAudioAuthorSourceFailure> {
        self.failure
    }
    /// Whether the actual CLAP plugin still needs its legal audio/main flush.
    pub const fn parameter_flush_pending(self) -> bool {
        self.parameter_flush_pending
    }
    /// Whether a producer is publishing a source batch. It cannot be treated
    /// as an empty source or service permission while this bit is set.
    pub const fn publishing(self) -> bool {
        self.publishing
    }
    /// Every actual parameter and timestamp boundary is closed.
    /// Events or DSP values may still be pending; this is no acceptance receipt.
    pub const fn boundaries_closed(self) -> bool {
        self.open_parameter_edits == 0
            && self.group_depth == 0
            && self.failure.is_none()
            && !self.publishing
    }
    /// Exact source and feedback emptiness required for configuration service.
    /// Runtime outstanding deliveries and current author fences are additional
    /// requirements; copying these facts cannot satisfy them.
    pub const fn closed_empty(self) -> bool {
        self.boundaries_closed()
            && self.pending_events == 0
            && self.pending_parameter_observations == 0
            && self.pending_dsp_feedback_values == 0
            && !self.parameter_flush_pending
    }
    pub(crate) fn validate(self) -> Result<(), NativeAudioAuthorRefusal> {
        let pending_source = self
            .pending_events
            .checked_add(self.pending_parameter_observations)
            .ok_or(NativeAudioAuthorRefusal::InvalidSource)?;
        if usize::try_from(pending_source).map_or(true, |count| count > NATIVE_AUDIO_AUTHOR_EVENTS)
            || usize::try_from(self.open_parameter_edits)
                .map_or(true, |count| count > NATIVE_AUDIO_AUTHOR_EVENTS)
            || usize::try_from(self.pending_dsp_feedback_values)
                .map_or(true, |count| count > NATIVE_AUDIO_AUTHOR_EVENTS)
            || u64::from(pending_source) > self.revision
        {
            return Err(NativeAudioAuthorRefusal::InvalidSource);
        }
        Ok(())
    }
}

/// Exact phase of one parameter callback in the ordered source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioAuthorParameterPhase {
    /// Begin one actual native parameter gesture.
    Begin,
    /// Actual finite plain-value callback. The format's producer enforces its
    /// own boundary rules; legal CLAP Values need not have a Begin/End pair.
    Value,
    /// End one actual native parameter gesture.
    End,
}

/// Finite scalar already projected through the actual native parameter schema.
///
/// Float64 bits preserve the producer's admitted value without an f32 ratio or
/// controller-display conversion. The canonical author schema still validates
/// the value and supplies its numeric type/unit; this wrapper owns no schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioAuthorScalar(u64);

impl NativeAudioAuthorScalar {
    pub(crate) fn admit(value: f64) -> Result<Self, NativeAudioAuthorRefusal> {
        if !value.is_finite() {
            return Err(NativeAudioAuthorRefusal::InvalidValue);
        }
        Ok(Self(value.to_bits()))
    }
    /// Exact admitted authored scalar, never a slider coordinate.
    pub fn value(self) -> f64 {
        f64::from_bits(self.0)
    }
    pub(crate) const fn bits(self) -> u64 {
        self.0
    }
    pub(crate) fn from_bits(bits: u64) -> Result<Self, NativeAudioAuthorRefusal> {
        Self::admit(f64::from_bits(bits))
    }
}

/// One definition-local parameter event with its actual phase and exact value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAudioAuthorParameterEvent {
    pub(crate) parameter: ParameterId,
    pub(crate) phase: NativeAudioAuthorParameterPhase,
    pub(crate) value: Option<NativeAudioAuthorScalar>,
}

impl NativeAudioAuthorParameterEvent {
    pub(crate) fn admit(
        parameter: ParameterId,
        phase: NativeAudioAuthorParameterPhase,
        value: Option<NativeAudioAuthorScalar>,
    ) -> Result<Self, NativeAudioAuthorRefusal> {
        if (phase == NativeAudioAuthorParameterPhase::Value) != value.is_some() {
            return Err(NativeAudioAuthorRefusal::InvalidValue);
        }
        Ok(Self { parameter, phase, value })
    }
    /// Stable machine identity from the same native definition's schema.
    pub fn parameter(&self) -> &ParameterId {
        &self.parameter
    }
    /// Actual callback phase in the source order.
    pub const fn phase(&self) -> NativeAudioAuthorParameterPhase {
        self.phase
    }
    /// Exact scalar only for Value callbacks; Begin/End do not invent a value.
    pub const fn value(&self) -> Option<NativeAudioAuthorScalar> {
        self.value
    }
}

/// A callback in the single cross-kind native author order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeAudioAuthorEvent {
    /// Parameter Begin, Value or End.
    Parameter(NativeAudioAuthorParameterEvent),
    /// Recordable native Process output in the actual signal sample domain.
    ParameterAtExecution {
        /// Original parameter event, with no manufactured Begin or End.
        parameter: NativeAudioAuthorParameterEvent,
        /// Signed signal sample plus the actual callback offset, after sole PDC.
        sample_position: i64,
        /// Actual prepared sample rate, never a guessed transport rate.
        sample_rate: u32,
    },
    /// One actual CLAP rescan and its main-thread current plain-value scan.
    /// A scan is not a historic value at the callback's original timestamp.
    ParameterValuesRescanned {
        /// Original official VALUES/TEXT/INFO/ALL bits.
        flags: u32,
        /// Writable actual values, populated only for VALUES or ALL.
        values: Vec<NativeAudioAuthorParameterEvent>,
    },
    /// Exact CLAP reference-clear request in cross-kind source order. An ID may
    /// already be absent from the plugin; flags never manufacture a native value.
    ParameterReferencesCleared {
        /// Stable identity translated from the original native ID.
        parameter: ParameterId,
        /// Original official CLAP clear bits: ALL=1, AUTOMATIONS=2, MODULATIONS=4.
        flags: u32,
    },
    /// Actual native authored-state dirty flag callback.
    DirtyChanged(bool),
    /// Begin a shared-timestamp callback scope.
    GroupStarted,
    /// Finish that timestamp scope without closing parameter gestures.
    GroupFinished,
}

/// Safe refusal preserves the current native object and unaccepted author work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioAuthorRefusal {
    /// This adapter has no normative native author producer.
    UnsupportedAdapter,
    /// The requested installed binding no longer owns this actual native object.
    OwnerChanged,
    /// Another delivery is still awaiting canonical author/plan acceptance.
    AwaitingAcceptance,
    /// The source is incomplete, malformed or changed during coherent capture.
    InvalidSource,
    /// A callback's parameter is absent from the exact native definition.
    UnknownParameter,
    /// The parameter is declared read-only by that native definition.
    ReadOnlyParameter,
    /// A callback's scalar or value/boundary payload is inadmissible.
    InvalidValue,
    /// A source/value changed during a bounded native state capture.
    SourceChanged,
    /// Real native parameter feedback still needs a DSP flush before capture.
    DspFeedbackPending,
    /// The actual component/controller provides no bounded coherent state.
    ConfigurationUnavailable,
    /// Native state/capture or event storage exceeds its admitted byte extent.
    ResourceLimit,
    /// An acknowledgement does not match the retained exact delivery revision.
    StaleDelivery,
}

/// Source result from the same processor's zero-sample parameter operation.
/// This does not publish PCM, advance author time or acknowledge a Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioAuthorParameterFlushOutcome {
    /// Existing ordered Value callbacks reached the actual DSP processor.
    Flushed,
    /// No truncated parameter prefix was processed; source work stays pending.
    Refused(NativeAudioAuthorRefusal),
}

/// Bounded immutable configuration captured from the actual native object.
///
/// One terminal delivery makes one bounded copy for the wire response while
/// the child retains the original until acknowledgement. Preserve/awaiting
/// polls never clone it. Both actual capacities require resource admission.
/// It grants no author/runtime permission and is not a DSP checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAudioAuthorConfiguration(Vec<u8>);

impl NativeAudioAuthorConfiguration {
    pub(crate) fn admit(bytes: Vec<u8>) -> Result<Self, NativeAudioAuthorRefusal> {
        if bytes.capacity() > NATIVE_AUDIO_AUTHOR_CONFIGURATION_BYTES {
            return Err(NativeAudioAuthorRefusal::ResourceLimit);
        }
        Ok(Self(bytes))
    }
    /// The exact captured component/controller bytes, without state recapture.
    pub fn bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
    pub(crate) fn retained_capacity(&self) -> usize {
        self.0.capacity()
    }
    /// Move the admitted byte allocation to its canonical consumer.
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

/// Owned complete callbacks and optional coherent terminal configuration.
///
/// Only a closed native boundary after same-object DSP parameter flush can
/// carry terminal configuration. An open gesture may deliver values while
/// retaining the canonical Session's existing NativeStatePending barrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAudioAuthorDelivery {
    pub(crate) binding: NativeAudioObjectBindRequest,
    pub(crate) revision: u64,
    pub(crate) events: Vec<NativeAudioAuthorEvent>,
    pub(crate) configuration: Option<NativeAudioAuthorConfiguration>,
    pub(crate) completion: mondrian_core::NativeAudioAuthorCompletion,
}

impl NativeAudioAuthorDelivery {
    /// Exact same-object binding, checked by the actual installed Runtime.
    pub const fn binding(&self) -> NativeAudioObjectBindRequest {
        self.binding
    }
    /// Terminal revision of every callback in this complete ordered delivery.
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    /// Complete callback sequence, without synthetic End or truncated prefix.
    pub fn events(&self) -> &[NativeAudioAuthorEvent] {
        &self.events
    }
    /// Actual component/controller state after terminal parameter delivery.
    /// This is authored configuration, not a private DSP history checkpoint.
    pub fn configuration(&self) -> Option<&[u8]> {
        self.configuration.as_ref().map(NativeAudioAuthorConfiguration::bytes)
    }
    /// Actual selected-prefix finality, independent of current source suffixes.
    pub const fn completion(&self) -> mondrian_core::NativeAudioAuthorCompletion {
        self.completion
    }
    pub(crate) fn validate(
        &self,
        source: NativeAudioAuthorSourceStatus,
    ) -> Result<(), NativeAudioAuthorRefusal> {
        source.validate()?;
        if self.events.is_empty()
            || self.events.len() > NATIVE_AUDIO_AUTHOR_EVENTS
            || self.events.capacity() > NATIVE_AUDIO_AUTHOR_EVENTS
            || self.revision == 0
            || self.revision > source.revision
            || u64::try_from(self.events.len()).map_or(true, |count| count > self.revision)
            || self.configuration.as_ref().is_some_and(|state| {
                state.retained_capacity() > NATIVE_AUDIO_AUTHOR_CONFIGURATION_BYTES
            })
            || (self.completion
                == mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration)
                != self.configuration.is_some()
            || self.completion == mondrian_core::NativeAudioAuthorCompletion::ParametersOnly
                && self.events.iter().any(|event| {
                    !matches!(
                        event,
                        NativeAudioAuthorEvent::Parameter(_)
                            | NativeAudioAuthorEvent::ParameterAtExecution { .. }
                            | NativeAudioAuthorEvent::GroupStarted
                            | NativeAudioAuthorEvent::GroupFinished
                            | NativeAudioAuthorEvent::ParameterReferencesCleared { .. }
                    ) && !matches!(event, NativeAudioAuthorEvent::ParameterValuesRescanned { flags, .. } if flags & 8 == 0)
                })
            || self.completion != mondrian_core::NativeAudioAuthorCompletion::Nonterminal
                && (!source.boundaries_closed()
                    || source.pending_dsp_feedback_values != 0
                    || source.parameter_flush_pending)
        {
            return Err(NativeAudioAuthorRefusal::InvalidSource);
        }
        Ok(())
    }
    /// Move the one admitted payload to the canonical consumer; no history clone.
    pub fn into_parts(
        self,
    ) -> (
        NativeAudioObjectBindRequest,
        u64,
        Vec<NativeAudioAuthorEvent>,
        Option<Vec<u8>>,
        mondrian_core::NativeAudioAuthorCompletion,
    ) {
        (
            self.binding,
            self.revision,
            self.events,
            self.configuration.map(NativeAudioAuthorConfiguration::into_bytes),
            self.completion,
        )
    }
}

/// Child delivery disposition, separate from current source emptiness.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NativeAudioAuthorDeliveryState {
    /// Non-consuming status or a supported source with no new callbacks.
    #[default]
    Preserved,
    /// Complete owned callbacks; Runtime must retain an outstanding receipt.
    Delivered(NativeAudioAuthorDelivery),
    /// The previous exact delivery still awaits Session plus live plan handoff.
    AwaitingAcknowledgement {
        /// Same object binding that originally produced the delivery.
        binding: NativeAudioObjectBindRequest,
        /// Exact retained source revision, never a later empty snapshot.
        revision: u64,
    },
    /// The exact retained delivery was confirmed; future source events remain
    /// pending and are never erased by this acknowledgement.
    Acknowledged(NativeAudioAuthorAcknowledgement),
    /// Actual child confirmed the private prior/fresh preparation and original prefix.
    ReenteredAcknowledged(crate::NativeAudioReenteredAuthorAcknowledgement),
    /// Typed refusal supplies no successful/truncated author prefix.
    Refused(NativeAudioAuthorRefusal),
}

/// One actual source snapshot on Poll9, without author or installed permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAudioAuthorObservation {
    /// Exact non-consuming source facts from the current native main object.
    pub source: NativeAudioAuthorSourceStatus,
    /// Ordered author delivery or explicit acknowledgement/preservation boundary.
    pub delivery: NativeAudioAuthorDeliveryState,
}

/// Read-only facts for an already validated Runtime acknowledgement.
///
/// Public transport facts do not prove a Session commit. The sole Runtime must
/// match its private outstanding delivery and successful parameter-plan handoff
/// before lowering these facts to the authenticated same-child stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioAuthorAcknowledgement {
    /// Recorded source binding that produced the retained exact delivery.
    pub source_binding: NativeAudioObjectBindRequest,
    /// Newly installed parameter Plan's binding of that same actual object.
    /// Source/installed bindings may differ; their object incarnation must not.
    pub installed_binding: NativeAudioObjectBindRequest,
    /// Exact terminal callback revision of the accepted complete delivery.
    pub revision: u64,
}
