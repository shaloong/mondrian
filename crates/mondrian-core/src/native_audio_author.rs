//! Format-neutral, bounded native author observations on the existing serial
//! completion path. These values are facts, not author or installed permission.
//!
//! The actual Runtime retains the same shared proposal until the sole Session,
//! canonical parameter Plan handoff and native acknowledgement have accepted it.
//! A copied target, revision or proposal cannot construct that private receipt.

use crate::{
    AuthoringList, NativeAudioDiagnosticBatch, NativeAudioEditorTarget, ParameterId, SequenceId,
};
use std::sync::Arc;

/// Maximum callback records in one product author completion.
pub const NATIVE_AUDIO_AUTHOR_CALLBACKS: usize = 4096;
/// Maximum combined retained value capacities across rescan callbacks in one
/// native author completion. Initialized value counts do not replace capacities.
pub const NATIVE_AUDIO_AUTHOR_RESCAN_VALUES: usize = 1024;
/// Maximum coherent component/controller bytes in one product completion.
pub const NATIVE_AUDIO_AUTHOR_STATE_BYTES: usize = 128 * 1024;

/// Actual phase of a native parameter callback, preserving cross-kind order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioParameterCallbackPhase {
    /// Begin one edit; repeated Begin callbacks are separately accounted.
    Begin,
    /// Perform a definition-domain value. The producer preserves the native
    /// format's actual gesture policy: an unwrapped CLAP Value is legal; VST
    /// Begin/End ownership is checked by its own normative producer.
    Value,
    /// Finish one actual edit, without closing another parameter's gesture.
    End,
}

/// Adapter-classified rescan behavior, independent of any native ABI bitmask.
/// Original callback flags remain with the adapter's actual retained source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioParameterRescanKind {
    /// No current-value refresh. This includes a native no-change request and
    /// presentation-only requests; it does not assert that metadata changed.
    NoValueRefresh,
    /// Refresh current values without changing the admitted parameter set.
    Values,
    /// The parameter-set boundary also requires coherent configuration handling.
    ParameterSetAndValues,
}

impl NativeAudioParameterRescanKind {
    /// Whether this completion can carry main-thread current scalar facts.
    pub const fn refreshes_values(self) -> bool {
        !matches!(self, Self::NoValueRefresh)
    }
}

/// One observed native author callback. Values retain exact finite Float64 bits;
/// their numeric type, range and unit still come from the canonical author schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeAudioAuthorCallback {
    /// One stable definition-local parameter and its actual callback phase.
    Parameter {
        /// Stable machine identity, never native caption or ordinal.
        parameter: ParameterId,
        /// Actual Begin, Value or End callback.
        phase: NativeAudioParameterCallbackPhase,
        /// Finite Float64 bits only for Value; Begin/End carry no invented value.
        value_bits: Option<u64>,
    },
    /// A recordable native output at its actual execution sample coordinate.
    ParameterAtExecution {
        /// Stable definition-local identity.
        parameter: ParameterId,
        /// Actual phase; no synthesized gesture boundary.
        phase: NativeAudioParameterCallbackPhase,
        /// Finite plain scalar only for Value.
        value_bits: Option<u64>,
        /// Exact observed Sequence-domain execution time, authenticated by Runtime.
        position: crate::DomainTime,
    },
    /// Refresh current values without creating recorded automation points.
    /// Existing keyed curves and absent references remain unchanged.
    ParameterValuesRescanned {
        /// Behavior classified by the originating adapter, not external ABI bits.
        kind: NativeAudioParameterRescanKind,
        /// Actual finite plain values read on the plugin's main thread.
        values: Vec<(ParameterId, u64)>,
    },
    /// Remove host references in actual native callback order. The stable ID
    /// may already be absent; this carries no default or current-value claim.
    ParameterReferencesCleared {
        /// Stable definition-local identity from the original native ID.
        parameter: ParameterId,
        /// Original official CLAP bits, admitted as ALL=1/AUTOMATIONS=2/MODULATIONS=4.
        flags: u32,
    },
    /// Actual authored native configuration dirty flag.
    /// A false flag does not mean the canonical project was saved.
    DirtyChanged(bool),
    /// Start one shared-timestamp scope.
    GroupStarted,
    /// Close that scope while individual parameter edits may remain open.
    GroupFinished,
}

/// Structural refusal before a product author completion is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NativeAudioAuthorProjectionError {
    /// Count/revision or byte storage exceeds its bounded completion contract.
    #[error("native author completion extent is invalid")]
    Extent,
    /// A callback phase has a missing, unexpected or non-finite scalar.
    #[error("native author callback scalar is invalid")]
    Callback,
    /// Opaque state was supplied without a closed actual native boundary.
    #[error("native author opaque state lacks a closed native boundary")]
    IncompleteState,
}

/// Actual producer completion fact, independent of an optional state extension.
/// This admits DTO shape only; it grants no author, installation or DSP permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioAuthorCompletion {
    /// Ordered values may be accepted while the native gesture is still open.
    Nonterminal,
    /// Actual closed parameter boundary; exposed values persist in canonical
    /// parameter references and the native object has no state extension.
    ParametersOnly,
    /// Actual closed boundary with successfully captured native configuration.
    CoherentConfiguration,
}

#[derive(Debug, PartialEq, Eq)]
struct NativeAudioAuthorContent {
    callbacks: Vec<NativeAudioAuthorCallback>,
    state: Option<AuthoringList<u8>>,
    completion: NativeAudioAuthorCompletion,
}

/// Shared immutable delivery retained by the actual Hosted receipt and the
/// existing completion. Clone shares bounded storage; it does not clone native
/// state bytes or create an author history, queue, instance or permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAudioAuthorProposal {
    target: NativeAudioEditorTarget,
    sequence: SequenceId,
    revision: u64,
    open_parameter_edits: u32,
    group_depth: u32,
    content: Arc<NativeAudioAuthorContent>,
}

impl NativeAudioAuthorProposal {
    /// Project already authenticated source facts into bounded shared storage.
    ///
    /// Construction admits only DTO shape. The Runtime must compare the actual
    /// installed claim/object and retain the private outstanding receipt before
    /// publication. The Session must then validate its own generation, owner,
    /// definition, parameter schema and exact author time independently.
    pub fn project(
        target: NativeAudioEditorTarget,
        sequence: SequenceId,
        revision: u64,
        boundaries: NativeAudioAuthorBoundaryFacts,
        callbacks: Vec<NativeAudioAuthorCallback>,
        state: Option<Vec<u8>>,
    ) -> Result<Self, NativeAudioAuthorProjectionError> {
        if callbacks.is_empty()
            || callbacks.len() > NATIVE_AUDIO_AUTHOR_CALLBACKS
            || callbacks.capacity() > NATIVE_AUDIO_AUTHOR_CALLBACKS
            || revision == 0
            || u64::try_from(callbacks.len()).map_or(true, |length| length > revision)
            || usize::try_from(boundaries.open_parameter_edits)
                .map_or(true, |count| count > NATIVE_AUDIO_AUTHOR_CALLBACKS)
            || state
                .as_ref()
                .is_some_and(|bytes| bytes.capacity() > NATIVE_AUDIO_AUTHOR_STATE_BYTES)
        {
            return Err(NativeAudioAuthorProjectionError::Extent);
        }
        let mut scanned_values = 0_usize;
        for callback in &callbacks {
            match callback {
                NativeAudioAuthorCallback::ParameterAtExecution {
                    parameter,
                    phase,
                    value_bits,
                    position,
                } => {
                    if parameter.storage_owner().capacity() > 255
                        || position.domain != crate::AuthoringTimeDomain::Sequence(sequence)
                        || (*phase == NativeAudioParameterCallbackPhase::Value)
                            != value_bits.is_some()
                        || value_bits.is_some_and(|bits| !f64::from_bits(bits).is_finite())
                    {
                        return Err(NativeAudioAuthorProjectionError::Callback);
                    }
                }
                NativeAudioAuthorCallback::ParameterValuesRescanned { kind, values } => {
                    scanned_values = scanned_values
                        .checked_add(values.capacity())
                        .ok_or(NativeAudioAuthorProjectionError::Extent)?;
                    if scanned_values > NATIVE_AUDIO_AUTHOR_RESCAN_VALUES {
                        return Err(NativeAudioAuthorProjectionError::Extent);
                    }
                    if values.capacity() > NATIVE_AUDIO_AUTHOR_RESCAN_VALUES
                        || (!kind.refreshes_values() && !values.is_empty())
                        || values.iter().any(|(id, bits)| {
                            id.storage_owner().capacity() > 255
                                || !f64::from_bits(*bits).is_finite()
                        })
                    {
                        return Err(NativeAudioAuthorProjectionError::Callback);
                    }
                }
                _ => {}
            }
            if let NativeAudioAuthorCallback::ParameterReferencesCleared { parameter, flags } =
                callback
                && (parameter.storage_owner().capacity() > 255 || flags & !7 != 0)
            {
                return Err(NativeAudioAuthorProjectionError::Callback);
            }
            if let NativeAudioAuthorCallback::Parameter { parameter, phase, value_bits } = callback
            {
                if parameter.storage_owner().capacity() > 255 {
                    return Err(NativeAudioAuthorProjectionError::Extent);
                }
                if (*phase == NativeAudioParameterCallbackPhase::Value) != value_bits.is_some()
                    || value_bits.is_some_and(|bits| !f64::from_bits(bits).is_finite())
                {
                    return Err(NativeAudioAuthorProjectionError::Callback);
                }
            }
        }
        let terminal = boundaries.completion != NativeAudioAuthorCompletion::Nonterminal;
        if (boundaries.completion == NativeAudioAuthorCompletion::CoherentConfiguration)
            != state.is_some()
            || terminal && (boundaries.open_parameter_edits != 0 || boundaries.group_depth != 0)
            || boundaries.completion == NativeAudioAuthorCompletion::ParametersOnly
                && callbacks.iter().any(|callback| {
                    !matches!(
                        callback,
                        NativeAudioAuthorCallback::Parameter { .. }
                            | NativeAudioAuthorCallback::ParameterAtExecution { .. }
                            | NativeAudioAuthorCallback::GroupStarted
                            | NativeAudioAuthorCallback::GroupFinished
                            | NativeAudioAuthorCallback::ParameterReferencesCleared { .. }
                    ) && !matches!(callback, NativeAudioAuthorCallback::ParameterValuesRescanned { kind, .. } if *kind != NativeAudioParameterRescanKind::ParameterSetAndValues)
                })
        {
            return Err(NativeAudioAuthorProjectionError::IncompleteState);
        }
        Ok(Self {
            target,
            sequence,
            revision,
            open_parameter_edits: boundaries.open_parameter_edits,
            group_depth: boundaries.group_depth,
            content: Arc::new(NativeAudioAuthorContent {
                callbacks,
                state: state.map(AuthoringList::from),
                completion: boundaries.completion,
            }),
        })
    }

    /// Exact installed address facts at source publication; not a current lease.
    pub const fn target(&self) -> NativeAudioEditorTarget {
        self.target
    }
    /// Actual local authored Sequence retained by the compiled Runtime.
    pub const fn sequence(&self) -> SequenceId {
        self.sequence
    }
    /// Exact last callback revision in this complete delivery.
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    /// Complete actual cross-kind callback order, without synthetic End.
    pub fn callbacks(&self) -> &[NativeAudioAuthorCallback] {
        &self.content.callbacks
    }
    /// Actual open parameter-edit count after this delivery's callbacks.
    pub const fn open_parameter_edits(&self) -> u32 {
        self.open_parameter_edits
    }
    /// Actual timestamp-scope depth, distinct from parameter-edit count.
    pub const fn group_depth(&self) -> u32 {
        self.group_depth
    }
    /// Structurally shared coherent state; never a private DSP checkpoint.
    pub fn state(&self) -> Option<&AuthoringList<u8>> {
        self.content.state.as_ref()
    }
    /// Actual native completion kind; optional State absence is not save failure.
    pub fn completion(&self) -> NativeAudioAuthorCompletion {
        self.content.completion
    }
    /// A proven closed parameter or coherent-configuration boundary was observed.
    /// This never implies opaque state exists; use `state()` for that fact.
    pub fn terminal(&self) -> bool {
        self.content.completion != NativeAudioAuthorCompletion::Nonterminal
    }
    /// Whether two completions still share the same actual retained proposal.
    /// Equal public target/revision facts cannot substitute for this identity.
    pub fn same_retained_proposal(&self, other: &Self) -> bool {
        self.target == other.target
            && self.sequence == other.sequence
            && self.revision == other.revision
            && Arc::ptr_eq(&self.content, &other.content)
    }
}

/// Actual source boundary facts, observed independently of project generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioAuthorBoundaryFacts {
    /// Producer's explicit completion fact, not inferred from zero open counts.
    pub completion: NativeAudioAuthorCompletion,
    /// Repeated Begin callbacks count separately until their matching End.
    pub open_parameter_edits: u32,
    /// GroupFinish can make this zero while parameter edits remain open.
    pub group_depth: u32,
}

/// Actual view-lifetime transition from one admitted native observation.
/// This is read-only metadata, never an installed lease or author permission.
/// The serial Runtime compares its private attachment receipt with the observed
/// current object. `retained` never implies a view was opened on a new object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioEditorAttachmentObservation {
    /// Exact target originally accepted when the real view attached.
    pub previous: crate::NativeAudioEditorTarget,
    /// Current installed target, independently re-admitted before SDK work.
    pub current: crate::NativeAudioEditorTarget,
    /// Actual current authenticated child PID.
    pub process_id: std::num::NonZeroU32,
    /// Actual current native object load incarnation.
    pub object_serial: std::num::NonZeroU64,
    /// True only when the private attached object is this same actual object.
    pub retained: bool,
}

/// One result from the sole native observation work. Logs and author callbacks
/// use the same actual child/main snapshot and existing completion channel.
/// Diagnostic-only records still grant no author permission.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NativeAudioObservationBatch {
    /// Existing bounded diagnostic projection, unchanged in meaning.
    pub diagnostics: Option<NativeAudioDiagnosticBatch>,
    /// Exact retained author proposal, absent for unsupported/unbound sources.
    pub author: Option<NativeAudioAuthorProposal>,
    /// Actual same-owner plain values/INFO/TEXT facts. This field never writes history.
    pub parameters: Option<crate::NativeAudioParameterObservation>,
    /// Actual attachment target transition, delivered before values/author data.
    pub attachment: Option<NativeAudioEditorAttachmentObservation>,
}

impl NativeAudioObservationBatch {
    /// True only for no author proposal and a complete empty diagnostic batch.
    /// This is presentation emptiness, never native author source closure.
    pub fn is_empty_complete(&self) -> bool {
        self.author.is_none()
            && self.parameters.is_none()
            && self.attachment.is_none()
            && self
                .diagnostics
                .as_ref()
                .is_none_or(NativeAudioDiagnosticBatch::is_empty_complete)
    }

    /// Diagnostic count for existing stale completion accounting. Retained
    /// author proposals stay with their private Hosted owner and may reappear.
    pub fn diagnostic_record_count(&self) -> usize {
        self.diagnostics.as_ref().map_or(0, |batch| batch.records().len())
    }
}

#[cfg(test)]
mod typed_rescan_contracts {
    use super::*;
    use crate::{
        AudioPreparedPlanId, AudioProcessorBindingId, AudioProcessorInstanceId,
        AudioRenderSessionId, NativeAudioEditorInsertion, NativeAudioEditorOccurrenceOwner,
        ProgramOutputId,
    };
    use std::num::NonZeroU64;

    fn project(
        callbacks: Vec<NativeAudioAuthorCallback>,
        completion: NativeAudioAuthorCompletion,
        state: Option<Vec<u8>>,
    ) -> Result<NativeAudioAuthorProposal, NativeAudioAuthorProjectionError> {
        NativeAudioAuthorProposal::project(
            NativeAudioEditorTarget {
                root_plan: AudioPreparedPlanId::new(),
                root_session: AudioRenderSessionId::new(),
                local_plan: AudioPreparedPlanId::new(),
                local_session: AudioRenderSessionId::new(),
                instance: AudioProcessorInstanceId::new(),
                owner: NativeAudioEditorOccurrenceOwner::Output(ProgramOutputId::new()),
                insertion: NativeAudioEditorInsertion::PreFader,
                continuity_epoch: None,
                binding: AudioProcessorBindingId::new(),
                installed_generation: NonZeroU64::new(1).unwrap(),
            },
            SequenceId::new(),
            7,
            NativeAudioAuthorBoundaryFacts {
                completion,
                open_parameter_edits: 0,
                group_depth: 0,
            },
            callbacks,
            state,
        )
    }
    fn rescan(kind: NativeAudioParameterRescanKind, count: usize) -> NativeAudioAuthorCallback {
        let mut values = Vec::with_capacity(count);
        for index in 0..count {
            values.push((
                ParameterId::new(format!("test.value{index}")).unwrap(),
                0.625_f64.to_bits(),
            ));
        }
        NativeAudioAuthorCallback::ParameterValuesRescanned { kind, values }
    }

    #[test]
    fn native_rescan_kind_preserves_value_and_structural_completion_boundaries() {
        use NativeAudioAuthorCompletion as Completion;
        use NativeAudioParameterRescanKind as Kind;
        assert!(!Kind::NoValueRefresh.refreshes_values());
        assert!(Kind::Values.refreshes_values());
        assert!(Kind::ParameterSetAndValues.refreshes_values());
        for (kind, count) in [(Kind::NoValueRefresh, 0), (Kind::Values, 1)] {
            let proposal =
                project(vec![rescan(kind, count)], Completion::ParametersOnly, None).unwrap();
            assert_eq!(proposal.callbacks().len(), 1);
            assert!(proposal.state().is_none());
        }
        assert_eq!(
            project(
                vec![rescan(Kind::NoValueRefresh, 1)],
                Completion::ParametersOnly,
                None
            ),
            Err(NativeAudioAuthorProjectionError::Callback)
        );
        assert_eq!(
            project(
                vec![rescan(Kind::ParameterSetAndValues, 1)],
                Completion::ParametersOnly,
                None
            ),
            Err(NativeAudioAuthorProjectionError::IncompleteState)
        );
        let proposal = project(
            vec![rescan(Kind::ParameterSetAndValues, 1)],
            Completion::CoherentConfiguration,
            Some(vec![3, 5]),
        )
        .unwrap();
        assert_eq!(proposal.state().unwrap().as_slice(), &[3, 5]);
        assert_eq!(proposal.completion(), Completion::CoherentConfiguration);
    }

    #[test]
    fn native_rescan_kind_keeps_finite_and_aggregate_capacity_guards() {
        use NativeAudioAuthorCompletion::ParametersOnly;
        use NativeAudioParameterRescanKind::Values;
        assert!(project(
            vec![rescan(Values, 512), rescan(Values, 512)],
            ParametersOnly,
            None
        )
        .is_ok());
        assert_eq!(
            project(
                vec![rescan(Values, 513), rescan(Values, 513)],
                ParametersOnly,
                None
            ),
            Err(NativeAudioAuthorProjectionError::Extent)
        );
        let mut oversized = Vec::with_capacity(1025);
        oversized.push((ParameterId::new("test.value").unwrap(), 0.5_f64.to_bits()));
        assert_eq!(
            project(
                vec![NativeAudioAuthorCallback::ParameterValuesRescanned {
                    kind: Values,
                    values: oversized,
                }],
                ParametersOnly,
                None
            ),
            Err(NativeAudioAuthorProjectionError::Extent)
        );
        assert_eq!(
            project(
                vec![NativeAudioAuthorCallback::ParameterValuesRescanned {
                    kind: Values,
                    values: vec![(ParameterId::new("test.value").unwrap(), f64::NAN.to_bits())],
                }],
                ParametersOnly,
                None
            ),
            Err(NativeAudioAuthorProjectionError::Callback)
        );
    }
}
