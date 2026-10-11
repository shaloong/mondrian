//! Atomic authoring operations for every Sequence-owned Audio Processor Rack.

use crate::audio::{
    AudioAuthoringError, AudioChannelStrip, AudioChannelStripOwner, AudioProcessorDefinitionRef,
    AudioProcessorInstance, AudioProcessorRack, AudioProgram,
};
use crate::audio_channel_strip_edit::{
    audio_channel_strip, inspect_audio_channel_strip, AudioChannelStripAddressError,
    AudioChannelStripEditBlocker,
};
use crate::sequence::Sequence;
use mondrian_core::{
    AudioProcessingScopeId, AudioProcessorInstanceId, AuthoringTimeDomain, DomainTime,
    ExactAutomationCurve, MixBusId, ParameterId, ProgramOutputId, TrackId,
};
use serde::{Deserialize, Serialize};

/// Ordered insertion stage within an Audio Channel Strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioChannelStripRack {
    /// Processor chain after input trim and before the fader.
    PreFader,
    /// Processor chain after the fader and before the Track mute gate.
    PostFader,
}

/// Stable address of one Sequence-owned Audio Processor Rack.
///
/// The closed shape cannot express a post-fader Processing Scope or a Rack
/// whose owner kind must be guessed from an untyped identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AudioProcessorRackAddress {
    /// Clip-level processing definition shared only through explicit bindings.
    ProcessingScope {
        /// Stable Processing Scope identity.
        scope_id: AudioProcessingScopeId,
    },
    /// Pre- or post-fader Rack on a Track, Bus, or Program Output strip.
    ChannelStrip {
        /// Stable typed strip owner.
        owner: AudioChannelStripOwner,
        /// Exact insertion stage within that strip.
        rack: AudioChannelStripRack,
    },
}

/// Stable relative placement within one Rack.
///
/// Array indexes are deliberately absent: UI projections and Undo may reorder
/// a Rack, while Processor Instance identity remains stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AudioProcessorRackPlacement {
    /// Place after the current final Processor.
    End,
    /// Place immediately before one Processor in the same Rack.
    Before {
        /// Stable anchor Processor identity.
        processor_id: AudioProcessorInstanceId,
    },
}

/// Explicit author policy for one native parameter value.
/// No later active frame, sample position or UI selection is inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeAudioParameterWriteMode {
    /// Write only an unkeyed parameter, retaining its lack of automation.
    Unkeyed,
    /// Preserve the exact curve and write a key at this qualified author time.
    AtOwnerTime {
        /// Exact Sequence or Processing Scope domain resolved by authoring.
        position: DomainTime,
    },
}

/// One exact schema-bound scalar observed from a native editor author gesture.
/// Native normalized coordinates must already be lowered by the format adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAudioParameterWrite {
    /// Complete current parameter contract, independent of labels and ordering.
    pub schema: Box<mondrian_core::automation::ParameterSchema>,
    /// Exact definition-domain value; Timeline performs its usual admission.
    pub value: f64,
    /// Chosen canonical author-time policy; never inferred from native state.
    pub mode: NativeAudioParameterWriteMode,
}

/// One ordered operation on the host references to a native parameter.
///
/// Clear removes the entire host curve, including its unkeyed value. It never
/// resets the native value or substitutes the definition default. Later Value
/// operations may explicitly establish a new host reference in the same batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeAudioParameterOperation {
    /// Definition-admitted author value and explicit owner-time write policy.
    Value {
        /// Complete schema, scalar and chosen write policy.
        write: NativeAudioParameterWrite,
    },
    /// Remove all host automation references; native state retains its value.
    ClearAutomation {
        /// Stable native parameter identity, including a removed parameter ID.
        parameter_id: ParameterId,
    },
}

impl From<NativeAudioParameterWrite> for NativeAudioParameterOperation {
    fn from(write: NativeAudioParameterWrite) -> Self {
        Self::Value { write }
    }
}

/// A targeted native author update, not a Processor or Rack replacement.
///
/// `None` preserves opaque state; `Some`, including empty bytes, replaces it.
/// Opaque bytes must be captured at a quiescent native author boundary by the
/// exact owner adapter. This value does not prove native origin or install a
/// runtime configuration. Native callback and storage admission budgets remain
/// adapter and Session responsibilities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAudioAuthorUpdate {
    /// Exact source-order operations. Repeated IDs are interpreted in this order.
    /// A malformed operation refuses the entire batch before author publication.
    pub parameters: Vec<NativeAudioParameterOperation>,
    /// Optional authored state replacement, structurally shared in history.
    pub opaque_state: Option<mondrian_core::AuthoringList<u8>>,
}
/// One atomic edit against an addressed Audio Processor Rack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum AudioProcessorRackEdit {
    /// Insert one complete built-in or hosted Processor author instance.
    Insert {
        /// New instance with a globally unique Sequence-local identity.
        processor: AudioProcessorInstance,
        /// Stable relative placement in the target Rack.
        placement: AudioProcessorRackPlacement,
    },
    /// Remove one exact Processor from the addressed Rack.
    Remove {
        /// Processor identity to remove.
        processor_id: AudioProcessorInstanceId,
    },
    /// Reorder one Processor within the addressed Rack.
    Move {
        /// Processor identity to move.
        processor_id: AudioProcessorInstanceId,
        /// Stable relative destination in the same Rack.
        placement: AudioProcessorRackPlacement,
    },
    /// Change explicit user bypass without changing Processor state.
    SetBypassed {
        /// Processor identity to edit.
        processor_id: AudioProcessorInstanceId,
        /// New authored bypass state.
        bypassed: bool,
    },
    /// Bind an existing native instance to a separately verified binary revision.
    RebindNative {
        /// Exact processor to update without replacing its automation or state.
        processor_id: AudioProcessorInstanceId,
        /// Definition observed before the external plugin probe.
        expected_definition: AudioProcessorDefinitionRef,
        /// Definition produced by the verified installed plugin probe.
        new_definition: AudioProcessorDefinitionRef,
    },
    /// Restore one parameter's definition default and remove every authored key.
    ResetParameterToDefault {
        /// Processor instance to edit.
        processor_id: AudioProcessorInstanceId,
        /// Current definition parameter identity.
        parameter_id: ParameterId,
    },

    /// Atomically publish parameters and opaque state from a native author edit.
    /// Native/runtime origin must be admitted by the product adapter separately.
    SetNativeAuthorState {
        /// Exact authored Processor; neighboring Rack state is preserved.
        processor_id: AudioProcessorInstanceId,
        /// Definition revision observed by the exact native instance adapter.
        expected_definition: AudioProcessorDefinitionRef,
        /// Complete targeted update, with no implicit native coordinate mapping.
        update: NativeAudioAuthorUpdate,
    },
    /// Replace one parameter's unkeyed value by stable definition identity.
    SetParameterStaticValue {
        /// Processor identity to edit.
        processor_id: AudioProcessorInstanceId,
        /// Stable parameter identity captured in the instance schema.
        parameter_id: ParameterId,
        /// Definition-domain numeric value.
        value: f64,
    },
}

impl AudioProcessorRackEdit {
    fn processor_id(&self) -> AudioProcessorInstanceId {
        match self {
            Self::Insert { processor, .. } => processor.id,
            Self::Remove { processor_id }
            | Self::Move { processor_id, .. }
            | Self::SetBypassed { processor_id, .. }
            | Self::RebindNative { processor_id, .. }
            | Self::SetNativeAuthorState { processor_id, .. }
            | Self::SetParameterStaticValue { processor_id, .. }
            | Self::ResetParameterToDefault { processor_id, .. } => *processor_id,
        }
    }
}

/// Complete Rack address and edit intent consumed by one author transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioProcessorRackEditRequest {
    /// Stable target Rack.
    pub address: AudioProcessorRackAddress,
    /// Exact mutation to apply.
    pub edit: AudioProcessorRackEdit,
}

/// Receipt from one successful Rack edit attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioProcessorRackEditOutcome {
    /// Processor addressed or inserted by the edit.
    pub processor_id: AudioProcessorInstanceId,
    /// Whether canonical author state changed.
    pub changed: bool,
}

/// Fail-closed Rack address or mutation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioProcessorRackEditError {
    /// Track owner does not exist in the Sequence audio Timeline.
    #[error("Audio Processor Rack references an unknown audio Track: {0}")]
    UnknownTrack(TrackId),
    /// A persistent Track lock forbids editing its channel strip.
    #[error("Audio Processor Rack cannot modify locked Track: {0}")]
    LockedTrack(TrackId),
    /// Processing Scope is absent from the Sequence Audio Program.
    #[error("Audio Processor Rack references an unknown Processing Scope: {0}")]
    UnknownProcessingScope(AudioProcessingScopeId),
    /// Editing a shared Scope would indirectly modify a locked Track binding.
    #[error("Audio Processing Scope {scope_id} is bound from locked Track {track_id}")]
    LockedProcessingScopeBinding {
        /// Shared Scope that was targeted.
        scope_id: AudioProcessingScopeId,
        /// Locked Track containing at least one binding.
        track_id: TrackId,
    },
    /// Mix Bus owner is absent from the Sequence Audio Program.
    #[error("Audio Processor Rack references an unknown Mix Bus: {0}")]
    UnknownBus(MixBusId),
    /// Program Output owner is absent from the Sequence Audio Program.
    #[error("Audio Processor Rack references an unknown Program Output: {0}")]
    UnknownProgramOutput(ProgramOutputId),
    /// Processor identity is absent from the addressed Rack.
    #[error("Audio Processor Rack does not contain Processor {0}")]
    UnknownProcessor(AudioProcessorInstanceId),
    /// Inserted identity already exists somewhere in the Sequence Audio Program.
    #[error("Audio Processor Instance identity already exists in this Sequence: {0}")]
    DuplicateProcessor(AudioProcessorInstanceId),
    /// Relative placement anchor is absent from the addressed Rack.
    #[error("Audio Processor Rack placement anchor does not exist: {0}")]
    UnknownPlacementAnchor(AudioProcessorInstanceId),
    /// Parameter identity is absent from the addressed Processor definition snapshot.
    #[error("Audio Processor {processor_id} has no parameter {parameter_id}")]
    UnknownParameter {
        /// Processor whose schema was queried.
        processor_id: AudioProcessorInstanceId,
        /// Missing stable parameter identity.
        parameter_id: ParameterId,
    },
    /// A rebind raced with another edit or changed plugin identity/schema version.
    #[error("CLAP Processor definition changed or rebind identity is incompatible")]
    IncompatibleRebind,
    /// Native author state names a different or unpinned native definition.
    #[error("native audio author definition is unavailable or changed")]
    NativeDefinitionMismatch,
    /// The current parameter schema differs from the observed native binding.
    #[error("native audio author parameter schema changed: {0}")]
    NativeParameterSchemaChanged(ParameterId),
    /// Native scalar callbacks cannot silently replace an existing author curve.
    #[error("native audio author parameter needs a chosen automation write policy: {0}")]
    NativeAutomationConflict(ParameterId),
    /// A qualified native write belongs to a different exact author-time owner.
    #[error(
        "native audio parameter {parameter_id} has owner domain {actual:?}, expected {expected:?}"
    )]
    NativeTimeDomainMismatch {
        /// Stable parameter whose time was refused.
        parameter_id: ParameterId,
        /// Domain resolved from the addressed canonical curve.
        expected: AuthoringTimeDomain,
        /// Explicit domain supplied with the write.
        actual: AuthoringTimeDomain,
    },
    /// Existing canonical automation preparation refused this complete write.
    #[error(transparent)]
    NativeAutomation(#[from] crate::AudioAutomationEditError),
    /// Internal addressed edit did not carry its prepared native curve token.
    #[error("native audio author update lacks its prepared curves")]
    NativePreparationMissing,

    /// Complete resulting audio author state is invalid.
    #[error("Audio Processor Rack edit produced invalid author state: {0}")]
    AuthorState(AudioAuthoringError),
}

/// Read-only author projection for one resolved Audio Processor Rack.
///
/// This is the sole query Interface for both Rack contents and edit admission.
/// Product surfaces must not reproduce Track-lock or shared-Scope binding rules.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioProcessorRackInspection<'a> {
    rack: &'a AudioProcessorRack,
    processing_scope_binding_count: Option<usize>,
    edit_blocker: Option<AudioProcessorRackEditError>,
}

impl<'a> AudioProcessorRackInspection<'a> {
    /// Resolved Rack in canonical author order.
    pub fn rack(&self) -> &'a AudioProcessorRack {
        self.rack
    }

    /// Number of Component Edits bound to a Processing Scope.
    ///
    /// Channel-strip Racks return `None` because they are owned directly rather
    /// than referenced through Component bindings.
    pub fn processing_scope_binding_count(&self) -> Option<usize> {
        self.processing_scope_binding_count
    }

    /// Authoritative reason this valid Rack cannot currently be edited.
    pub fn edit_blocker(&self) -> Option<&AudioProcessorRackEditError> {
        self.edit_blocker.as_ref()
    }

    /// Whether an edit request against this Rack may enter a transaction.
    pub fn is_editable(&self) -> bool {
        self.edit_blocker.is_none()
    }
}

/// Resolve one Rack and inspect its authoritative edit admission state.
///
/// Address validity, shared Processing Scope binding counts, and Track locks
/// are evaluated from the same immutable Sequence snapshot. The mutation path
/// consumes this Interface before cloning or changing author state.
pub fn inspect_audio_processor_rack<'a>(
    sequence: &'a Sequence,
    address: &AudioProcessorRackAddress,
) -> Result<AudioProcessorRackInspection<'a>, AudioProcessorRackEditError> {
    let (rack, processing_scope_binding_count, edit_blocker) = match *address {
        AudioProcessorRackAddress::ProcessingScope { scope_id } => {
            let rack = resolve_rack(sequence, address)?;
            let mut binding_count = 0usize;
            let mut locked_track = None;
            for track in &sequence.audio_tracks {
                for clip in &track.clips {
                    for edit in &clip.audio_components {
                        if edit.processing.scope_id == scope_id {
                            binding_count = binding_count.saturating_add(1);
                            if track.is_locked {
                                locked_track.get_or_insert(track.id);
                            }
                        }
                    }
                }
            }
            (
                rack,
                Some(binding_count),
                locked_track.map(|track_id| {
                    AudioProcessorRackEditError::LockedProcessingScopeBinding { scope_id, track_id }
                }),
            )
        }
        AudioProcessorRackAddress::ChannelStrip { owner, rack } => {
            let inspection = inspect_audio_channel_strip(sequence, owner)
                .map_err(map_channel_strip_address_error)?;
            (
                rack_in_strip(inspection.strip(), rack),
                None,
                inspection.edit_blocker().cloned().map(map_channel_strip_blocker),
            )
        }
    };
    Ok(AudioProcessorRackInspection { rack, processing_scope_binding_count, edit_blocker })
}

/// Resolve the editable author owner and its exact pinned native definition.
///
/// This is author admission only. It cannot prove an executing processor object,
/// native callback origin, continuity epoch, or runtime installation lease.
pub fn inspect_native_audio_authoring<'a>(
    sequence: &'a Sequence,
    address: &AudioProcessorRackAddress,
    processor_id: AudioProcessorInstanceId,
    expected_definition: &AudioProcessorDefinitionRef,
) -> Result<&'a AudioProcessorInstance, AudioProcessorRackEditError> {
    let inspection = inspect_audio_processor_rack(sequence, address)?;
    if let Some(blocker) = inspection.edit_blocker() {
        return Err(blocker.clone());
    }
    let processor = inspection
        .rack()
        .processors
        .iter()
        .find(|processor| processor.id == processor_id)
        .ok_or(AudioProcessorRackEditError::UnknownProcessor(processor_id))?;
    admit_native_author_definition(processor, expected_definition)?;
    Ok(processor)
}

fn admit_native_author_definition(
    processor: &AudioProcessorInstance,
    expected_definition: &AudioProcessorDefinitionRef,
) -> Result<(), AudioProcessorRackEditError> {
    let pinned = match &processor.definition {
        AudioProcessorDefinitionRef::Vst3 { binary_sha256: Some(hash), .. }
        | AudioProcessorDefinitionRef::Clap { binary_sha256: Some(hash), .. } => *hash != [0; 32],
        _ => false,
    };
    if !pinned || &processor.definition != expected_definition {
        return Err(AudioProcessorRackEditError::NativeDefinitionMismatch);
    }
    Ok(())
}

struct PreparedNativeAudioAuthorUpdate {
    curves: std::collections::BTreeMap<ParameterId, Option<ExactAutomationCurve>>,
    opaque_state: Option<mondrian_core::AuthoringList<u8>>,
}

fn prepare_native_author_update(
    sequence: &Sequence,
    address: &AudioProcessorRackAddress,
    processor_id: AudioProcessorInstanceId,
    expected_definition: &AudioProcessorDefinitionRef,
    update: &NativeAudioAuthorUpdate,
) -> Result<PreparedNativeAudioAuthorUpdate, AudioProcessorRackEditError> {
    let processor =
        inspect_native_audio_authoring(sequence, address, processor_id, expected_definition)?;
    let mut curves = std::collections::BTreeMap::new();
    for operation in &update.parameters {
        let write = match operation {
            NativeAudioParameterOperation::ClearAutomation { parameter_id } => {
                // A plugin may remove an ID before clearing its references. An
                // absent schema has no references; do not create a phantom one.
                if processor.parameters.contains_key(parameter_id) {
                    curves.insert(parameter_id.clone(), None);
                }
                continue;
            }
            NativeAudioParameterOperation::Value { write } => write,
        };
        let parameter_id = &write.schema.parameter_id;
        let parameter = processor.parameters.get(parameter_id).ok_or_else(|| {
            AudioProcessorRackEditError::UnknownParameter {
                processor_id,
                parameter_id: parameter_id.clone(),
            }
        })?;
        if parameter.schema != *write.schema {
            return Err(AudioProcessorRackEditError::NativeParameterSchemaChanged(
                parameter_id.clone(),
            ));
        }
        let previous = curves.get(parameter_id).unwrap_or(&parameter.automation);
        let mut base = match previous {
            Some(curve) => curve.clone(),
            None => {
                ExactAutomationCurve::new(parameter_id.clone(), write.value).map_err(|error| {
                    AudioProcessorRackEditError::AuthorState(
                        AudioAuthoringError::InvalidAutomation { reason: error.to_string() },
                    )
                })?
            }
        };
        let curve = match write.mode {
            NativeAudioParameterWriteMode::Unkeyed => {
                if !base.keyframes.is_empty() {
                    return Err(AudioProcessorRackEditError::NativeAutomationConflict(
                        parameter_id.clone(),
                    ));
                }
                base.default_value = write.value;
                crate::audio::validate_numeric_automation(&parameter.schema, &base)
                    .map_err(AudioProcessorRackEditError::AuthorState)?;
                base
            }
            NativeAudioParameterWriteMode::AtOwnerTime { position } => {
                let target = crate::AudioAutomationTarget::ProcessorParameter {
                    rack: *address,
                    processor_id,
                    parameter_id: parameter_id.clone(),
                };
                let inspection = crate::inspect_audio_automation(sequence, &target)
                    .map_err(crate::AudioAutomationEditError::from)?;
                if position.domain != inspection.time_domain() {
                    return Err(AudioProcessorRackEditError::NativeTimeDomainMismatch {
                        parameter_id: parameter_id.clone(),
                        expected: inspection.time_domain(),
                        actual: position.domain,
                    });
                }
                inspection.curve_contract().preview_value_at_time(
                    &base,
                    position.time,
                    write.value,
                )?
            }
        };
        curves.insert(parameter_id.clone(), Some(curve));
    }
    Ok(PreparedNativeAudioAuthorUpdate { curves, opaque_state: update.opaque_state.clone() })
}

fn apply_native_author_update(
    processor: &mut AudioProcessorInstance,
    expected_definition: &AudioProcessorDefinitionRef,
    prepared: PreparedNativeAudioAuthorUpdate,
) -> Result<bool, AudioProcessorRackEditError> {
    admit_native_author_definition(processor, expected_definition)?;
    let mut changed = false;
    for (parameter_id, curve) in prepared.curves {
        let parameter = processor.parameters.get_mut(&parameter_id).ok_or_else(|| {
            AudioProcessorRackEditError::UnknownParameter {
                processor_id: processor.id,
                parameter_id: parameter_id.clone(),
            }
        })?;
        if parameter.automation != curve {
            parameter.automation = curve;
            changed = true;
        }
    }
    if let Some(state) = prepared.opaque_state
        && processor.opaque_state.as_ref() != Some(&state)
    {
        processor.opaque_state = Some(state);
        changed = true;
    }
    Ok(changed)
}

/// Resolve one Rack for read-only projection by stable typed address.
pub fn audio_processor_rack<'a>(
    sequence: &'a Sequence,
    address: &AudioProcessorRackAddress,
) -> Result<&'a AudioProcessorRack, AudioProcessorRackEditError> {
    resolve_rack(sequence, address)
}

fn resolve_rack<'a>(
    sequence: &'a Sequence,
    address: &AudioProcessorRackAddress,
) -> Result<&'a AudioProcessorRack, AudioProcessorRackEditError> {
    match *address {
        AudioProcessorRackAddress::ProcessingScope { scope_id } => sequence
            .audio_program
            .processing_scopes
            .iter()
            .find(|scope| scope.id == scope_id)
            .map(|scope| &scope.processors)
            .ok_or(AudioProcessorRackEditError::UnknownProcessingScope(
                scope_id,
            )),
        AudioProcessorRackAddress::ChannelStrip { owner, rack } => {
            let strip =
                audio_channel_strip(sequence, owner).map_err(map_channel_strip_address_error)?;
            Ok(rack_in_strip(strip, rack))
        }
    }
}

fn rack_in_strip(strip: &AudioChannelStrip, rack: AudioChannelStripRack) -> &AudioProcessorRack {
    match rack {
        AudioChannelStripRack::PreFader => &strip.pre_fader,
        AudioChannelStripRack::PostFader => &strip.post_fader,
    }
}

fn map_channel_strip_address_error(
    error: AudioChannelStripAddressError,
) -> AudioProcessorRackEditError {
    match error {
        AudioChannelStripAddressError::UnknownTrack(track_id) => {
            AudioProcessorRackEditError::UnknownTrack(track_id)
        }
        AudioChannelStripAddressError::UnknownBus(bus_id) => {
            AudioProcessorRackEditError::UnknownBus(bus_id)
        }
        AudioChannelStripAddressError::UnknownProgramOutput(output_id) => {
            AudioProcessorRackEditError::UnknownProgramOutput(output_id)
        }
    }
}

fn map_channel_strip_blocker(blocker: AudioChannelStripEditBlocker) -> AudioProcessorRackEditError {
    match blocker {
        AudioChannelStripEditBlocker::LockedTrack(track_id) => {
            AudioProcessorRackEditError::LockedTrack(track_id)
        }
    }
}

/// Apply one Rack edit atomically to a Sequence candidate.
///
/// The original Sequence is replaced only after lock admission, stable address
/// resolution, local mutation, and complete Audio Program validation succeed.
pub fn apply_audio_processor_rack_edit(
    sequence: &mut Sequence,
    request: &AudioProcessorRackEditRequest,
) -> Result<AudioProcessorRackEditOutcome, AudioProcessorRackEditError> {
    let processor_id = request.edit.processor_id();
    ensure_editable(sequence, &request.address)?;

    if let AudioProcessorRackEdit::Insert { processor, .. } = &request.edit
        && processor_id_exists(&sequence.audio_program, processor.id)
    {
        return Err(AudioProcessorRackEditError::DuplicateProcessor(
            processor.id,
        ));
    }

    let prepared_native = match &request.edit {
        AudioProcessorRackEdit::SetNativeAuthorState {
            processor_id,
            expected_definition,
            update,
        } => Some(prepare_native_author_update(
            sequence,
            &request.address,
            *processor_id,
            expected_definition,
            update,
        )?),
        _ => None,
    };
    let mut candidate = sequence.clone();
    let changed = apply_to_rack(
        resolve_rack_mut(&mut candidate, &request.address)?,
        &request.edit,
        prepared_native,
    )?;
    if !changed {
        return Ok(AudioProcessorRackEditOutcome { processor_id, changed: false });
    }
    candidate
        .audio_program
        .validate(
            &candidate.audio_tracks,
            &candidate.audio_roles,
            candidate.settings.audio_channel_layout,
        )
        .map_err(AudioProcessorRackEditError::AuthorState)?;
    *sequence = candidate;
    Ok(AudioProcessorRackEditOutcome { processor_id, changed: true })
}

fn ensure_editable(
    sequence: &Sequence,
    address: &AudioProcessorRackAddress,
) -> Result<(), AudioProcessorRackEditError> {
    let inspection = inspect_audio_processor_rack(sequence, address)?;
    if let Some(blocker) = inspection.edit_blocker {
        Err(blocker)
    } else {
        Ok(())
    }
}

fn resolve_rack_mut<'a>(
    sequence: &'a mut Sequence,
    address: &AudioProcessorRackAddress,
) -> Result<&'a mut AudioProcessorRack, AudioProcessorRackEditError> {
    match *address {
        AudioProcessorRackAddress::ProcessingScope { scope_id } => sequence
            .audio_program
            .processing_scopes
            .iter_mut()
            .find(|scope| scope.id == scope_id)
            .map(|scope| &mut scope.processors)
            .ok_or(AudioProcessorRackEditError::UnknownProcessingScope(
                scope_id,
            )),
        AudioProcessorRackAddress::ChannelStrip { owner, rack } => {
            let strip = match owner {
                AudioChannelStripOwner::Track { track_id } => {
                    &mut sequence
                        .audio_program
                        .track_channels
                        .get_mut(&track_id)
                        .ok_or(AudioProcessorRackEditError::UnknownTrack(track_id))?
                        .strip
                }
                AudioChannelStripOwner::Bus { bus_id } => {
                    &mut sequence
                        .audio_program
                        .buses
                        .iter_mut()
                        .find(|bus| bus.id == bus_id)
                        .ok_or(AudioProcessorRackEditError::UnknownBus(bus_id))?
                        .strip
                }
                AudioChannelStripOwner::ProgramOutput { output_id } => {
                    &mut sequence
                        .audio_program
                        .outputs
                        .iter_mut()
                        .find(|output| output.id == output_id)
                        .ok_or(AudioProcessorRackEditError::UnknownProgramOutput(output_id))?
                        .strip
                }
            };
            Ok(match rack {
                AudioChannelStripRack::PreFader => &mut strip.pre_fader,
                AudioChannelStripRack::PostFader => &mut strip.post_fader,
            })
        }
    }
}

fn apply_to_rack(
    rack: &mut AudioProcessorRack,
    edit: &AudioProcessorRackEdit,
    prepared_native: Option<PreparedNativeAudioAuthorUpdate>,
) -> Result<bool, AudioProcessorRackEditError> {
    match edit {
        AudioProcessorRackEdit::Insert { processor, placement } => {
            let index = placement_index(rack, *placement)?;
            rack.processors.insert(index, processor.clone());
            Ok(true)
        }
        AudioProcessorRackEdit::Remove { processor_id } => {
            let index = processor_index(rack, *processor_id)?;
            rack.processors.remove(index);
            Ok(true)
        }
        AudioProcessorRackEdit::Move { processor_id, placement } => {
            let source = processor_index(rack, *processor_id)?;
            if matches!(placement, AudioProcessorRackPlacement::Before { processor_id: anchor } if anchor == processor_id)
            {
                return Ok(false);
            }
            placement_index(rack, *placement)?;
            let processor = rack.processors.remove(source);
            let destination = placement_index(rack, *placement)?;
            if source == destination {
                rack.processors.insert(source, processor);
                return Ok(false);
            }
            rack.processors.insert(destination, processor);
            Ok(true)
        }
        AudioProcessorRackEdit::SetBypassed { processor_id, bypassed } => {
            let index = processor_index(rack, *processor_id)?;
            if rack.processors[index].bypassed == *bypassed {
                return Ok(false);
            }
            rack.processors[index].bypassed = *bypassed;
            Ok(true)
        }
        AudioProcessorRackEdit::RebindNative {
            processor_id,
            expected_definition,
            new_definition,
        } => {
            let index = processor_index(rack, *processor_id)?;
            let processor = &mut rack.processors[index];
            let compatible = match (expected_definition, new_definition) {
                (
                    AudioProcessorDefinitionRef::Clap {
                        plugin_id: old_id,
                        schema_version: old_version,
                        ..
                    },
                    AudioProcessorDefinitionRef::Clap {
                        plugin_id: new_id,
                        schema_version: new_version,
                        binary_sha256: Some(new_hash),
                    },
                ) => old_id == new_id && old_version == new_version && *new_hash != [0; 32],
                (
                    AudioProcessorDefinitionRef::Vst3 {
                        class_id: old_id,
                        vendor: old_vendor,
                        schema_version: old_version,
                        ..
                    },
                    AudioProcessorDefinitionRef::Vst3 {
                        class_id: new_id,
                        vendor: new_vendor,
                        schema_version: new_version,
                        binary_sha256: Some(new_hash),
                    },
                ) => {
                    old_id == new_id
                        && old_vendor == new_vendor
                        && old_version == new_version
                        && *new_hash != [0; 32]
                }
                _ => false,
            };
            if &processor.definition != expected_definition || !compatible {
                return Err(AudioProcessorRackEditError::IncompatibleRebind);
            }
            if &processor.definition == new_definition {
                return Ok(false);
            }
            processor.definition = new_definition.clone();
            Ok(true)
        }
        AudioProcessorRackEdit::ResetParameterToDefault { processor_id, parameter_id } => {
            let index = processor_index(rack, *processor_id)?;
            let processor = &mut rack.processors[index];
            let parameter = processor.parameters.get(parameter_id).ok_or_else(|| {
                AudioProcessorRackEditError::UnknownParameter {
                    processor_id: *processor_id,
                    parameter_id: parameter_id.clone(),
                }
            })?;
            let curve = crate::audio::default_numeric_automation(&parameter.schema)
                .map_err(AudioProcessorRackEditError::AuthorState)?;
            if parameter.automation.as_ref() == Some(&curve) {
                return Ok(false);
            }
            processor
                .set_parameter_automation(curve)
                .map_err(AudioProcessorRackEditError::AuthorState)?;
            Ok(true)
        }
        AudioProcessorRackEdit::SetNativeAuthorState {
            processor_id, expected_definition, ..
        } => {
            let index = processor_index(rack, *processor_id)?;
            let prepared =
                prepared_native.ok_or(AudioProcessorRackEditError::NativePreparationMissing)?;
            apply_native_author_update(&mut rack.processors[index], expected_definition, prepared)
        }
        AudioProcessorRackEdit::SetParameterStaticValue { processor_id, parameter_id, value } => {
            let index = processor_index(rack, *processor_id)?;
            set_parameter_static_value(&mut rack.processors[index], parameter_id, *value)
        }
    }
}

fn set_parameter_static_value(
    processor: &mut AudioProcessorInstance,
    parameter_id: &ParameterId,
    value: f64,
) -> Result<bool, AudioProcessorRackEditError> {
    let parameter = processor.parameters.get(parameter_id).ok_or_else(|| {
        AudioProcessorRackEditError::UnknownParameter {
            processor_id: processor.id,
            parameter_id: parameter_id.clone(),
        }
    })?;
    let mut curve = match &parameter.automation {
        Some(curve) => curve.clone(),
        None => crate::audio::default_numeric_automation(&parameter.schema)
            .map_err(AudioProcessorRackEditError::AuthorState)?,
    };
    curve.default_value = value;
    if parameter.automation.as_ref() == Some(&curve) {
        return Ok(false);
    }
    processor
        .set_parameter_automation(curve)
        .map_err(AudioProcessorRackEditError::AuthorState)?;
    Ok(true)
}

fn processor_index(
    rack: &AudioProcessorRack,
    processor_id: AudioProcessorInstanceId,
) -> Result<usize, AudioProcessorRackEditError> {
    rack.processors
        .iter()
        .position(|processor| processor.id == processor_id)
        .ok_or(AudioProcessorRackEditError::UnknownProcessor(processor_id))
}

fn placement_index(
    rack: &AudioProcessorRack,
    placement: AudioProcessorRackPlacement,
) -> Result<usize, AudioProcessorRackEditError> {
    match placement {
        AudioProcessorRackPlacement::End => Ok(rack.processors.len()),
        AudioProcessorRackPlacement::Before { processor_id } => {
            rack.processors.iter().position(|processor| processor.id == processor_id).ok_or(
                AudioProcessorRackEditError::UnknownPlacementAnchor(processor_id),
            )
        }
    }
}

fn processor_id_exists(program: &AudioProgram, processor_id: AudioProcessorInstanceId) -> bool {
    all_racks(program)
        .any(|rack| rack.processors.iter().any(|processor| processor.id == processor_id))
}

fn all_racks(program: &AudioProgram) -> impl Iterator<Item = &AudioProcessorRack> {
    program
        .processing_scopes
        .iter()
        .map(|scope| &scope.processors)
        .chain(
            program
                .track_channels
                .values()
                .flat_map(|channel| [&channel.strip.pre_fader, &channel.strip.post_fader]),
        )
        .chain(
            program
                .buses
                .iter()
                .flat_map(|bus| [&bus.strip.pre_fader, &bus.strip.post_fader]),
        )
        .chain(
            program
                .outputs
                .iter()
                .flat_map(|output| [&output.strip.pre_fader, &output.strip.post_fader]),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{
        AudioChannelStrip, AudioMixBus, AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID,
        BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID, GAIN_DB_PARAMETER_ID,
    };
    use crate::Clip;
    use mondrian_core::{AssetId, AudioSourceComponentId, TimelineTime};

    fn sequence_with_audio_clip() -> (Sequence, TrackId, AudioProcessingScopeId) {
        let mut sequence = Sequence::new("Rack authoring");
        let track_id = sequence.audio_tracks[0].id;
        let clip = Clip::new(
            AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::new(4, 1).expect("duration"),
        )
        .expect("Clip");
        sequence
            .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
            .expect("audio Clip");
        let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
        (sequence, track_id, scope_id)
    }

    fn request(
        address: AudioProcessorRackAddress,
        edit: AudioProcessorRackEdit,
    ) -> AudioProcessorRackEditRequest {
        AudioProcessorRackEditRequest { address, edit }
    }

    fn insert(
        sequence: &mut Sequence,
        address: AudioProcessorRackAddress,
        processor: AudioProcessorInstance,
    ) -> AudioProcessorInstanceId {
        let id = processor.id;
        let outcome = apply_audio_processor_rack_edit(
            sequence,
            &request(
                address,
                AudioProcessorRackEdit::Insert {
                    processor,
                    placement: AudioProcessorRackPlacement::End,
                },
            ),
        )
        .expect("insert Processor");
        assert!(outcome.changed);
        assert_eq!(outcome.processor_id, id);
        id
    }

    #[test]
    fn one_edit_interface_addresses_scope_track_bus_and_output_racks() {
        let (mut sequence, track_id, scope_id) = sequence_with_audio_clip();
        let bus_id = MixBusId::new();
        sequence.audio_program.buses.push(AudioMixBus {
            id: bus_id,
            name: "Dialog".to_owned(),
            strip: AudioChannelStrip::default(),
        });
        let output_id = sequence.audio_program.outputs[0].id;
        let addresses = [
            AudioProcessorRackAddress::ProcessingScope { scope_id },
            AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::Track { track_id },
                rack: AudioChannelStripRack::PreFader,
            },
            AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::Bus { bus_id },
                rack: AudioChannelStripRack::PostFader,
            },
            AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::ProgramOutput { output_id },
                rack: AudioChannelStripRack::PreFader,
            },
        ];
        for address in addresses {
            let id = insert(
                &mut sequence,
                address,
                AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1),
            );
            assert_eq!(
                audio_processor_rack(&sequence, &address).expect("Rack").processors[0].id,
                id
            );
        }
        sequence
            .audio_program
            .validate(
                &sequence.audio_tracks,
                &sequence.audio_roles,
                sequence.settings.audio_channel_layout,
            )
            .expect("complete valid Audio Program");
    }

    #[test]
    fn inspection_is_the_single_address_lock_and_scope_binding_authority() {
        let (mut sequence, track_id, scope_id) = sequence_with_audio_clip();
        sequence.audio_tracks[0].clips[0].audio_components.push(
            crate::audio::AudioComponentEdit::media(AudioSourceComponentId::new(), scope_id),
        );
        let scope_address = AudioProcessorRackAddress::ProcessingScope { scope_id };
        let scope = inspect_audio_processor_rack(&sequence, &scope_address).expect("Scope");
        assert_eq!(scope.processing_scope_binding_count(), Some(2));
        assert!(scope.is_editable());
        assert!(scope.edit_blocker().is_none());

        let track_address = AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id },
            rack: AudioChannelStripRack::PreFader,
        };
        let track = inspect_audio_processor_rack(&sequence, &track_address).expect("Track Rack");
        assert_eq!(track.processing_scope_binding_count(), None);
        assert!(track.is_editable());

        sequence.audio_tracks[0].is_locked = true;
        let scope = inspect_audio_processor_rack(&sequence, &scope_address).expect("locked Scope");
        assert_eq!(
            scope.edit_blocker(),
            Some(&AudioProcessorRackEditError::LockedProcessingScopeBinding { scope_id, track_id })
        );
        let track =
            inspect_audio_processor_rack(&sequence, &track_address).expect("locked Track Rack");
        assert_eq!(
            track.edit_blocker(),
            Some(&AudioProcessorRackEditError::LockedTrack(track_id))
        );
    }

    #[test]
    fn parameter_reset_processor_uses_schema_for_animated_and_static_integer_values() {
        for (definition, parameter_id, authored) in [
            (BUILTIN_GAIN_DEFINITION_ID, GAIN_DB_PARAMETER_ID, -6.0),
            (
                crate::audio::BUILTIN_SAMPLE_DELAY_DEFINITION_ID,
                crate::audio::SAMPLE_DELAY_FRAMES_PARAMETER_ID,
                32.0,
            ),
        ] {
            let (mut sequence, track_id, _) = sequence_with_audio_clip();
            let address = AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::Track { track_id },
                rack: AudioChannelStripRack::PreFader,
            };
            let processor_id = insert(
                &mut sequence,
                address,
                AudioProcessorInstance::built_in(definition, 1),
            );
            let parameter_id = ParameterId::new_static(parameter_id);
            apply_audio_processor_rack_edit(
                &mut sequence,
                &request(
                    address,
                    AudioProcessorRackEdit::SetParameterStaticValue {
                        processor_id,
                        parameter_id: parameter_id.clone(),
                        value: authored,
                    },
                ),
            )
            .expect("authored static");
            if definition == BUILTIN_GAIN_DEFINITION_ID {
                crate::apply_audio_automation_edit(
                    &mut sequence,
                    &crate::AudioAutomationEditRequest {
                        target: crate::AudioAutomationTarget::ProcessorParameter {
                            rack: address,
                            processor_id,
                            parameter_id: parameter_id.clone(),
                        },
                        edit: crate::AudioAutomationEdit::UpsertKeyframe {
                            keyframe: mondrian_core::ExactAutomationKeyframe::linear(
                                TimelineTime::ONE,
                                -3.0,
                            ),
                        },
                    },
                )
                .expect("animated gain");
            }
            let reset = request(
                address,
                AudioProcessorRackEdit::ResetParameterToDefault {
                    processor_id,
                    parameter_id: parameter_id.clone(),
                },
            );
            sequence.audio_tracks[0].is_locked = true;
            let locked = sequence.clone();
            assert!(apply_audio_processor_rack_edit(&mut sequence, &reset).is_err());
            assert_eq!(sequence, locked);
            sequence.audio_tracks[0].is_locked = false;
            assert!(apply_audio_processor_rack_edit(&mut sequence, &reset).expect("reset").changed);
            let processor = &audio_processor_rack(&sequence, &address).expect("rack").processors[0];
            assert_eq!(processor.id, processor_id);
            let parameter = &processor.parameters[&parameter_id];
            assert_eq!(
                parameter.automation.as_ref().expect("bound host curve").default_value,
                0.0
            );
            assert!(parameter.automation.as_ref().expect("bound host curve").keyframes.is_empty());
            let before = sequence.clone();
            assert!(
                !apply_audio_processor_rack_edit(&mut sequence, &reset).expect("no-op").changed
            );
            let stale = request(
                address,
                AudioProcessorRackEdit::ResetParameterToDefault {
                    processor_id,
                    parameter_id: ParameterId::new_static("missing.parameter"),
                },
            );
            assert!(apply_audio_processor_rack_edit(&mut sequence, &stale).is_err());
            assert_eq!(sequence, before);
        }
    }

    #[test]
    fn stable_processor_identities_drive_reorder_bypass_and_static_parameter_edits() {
        let (mut sequence, track_id, _) = sequence_with_audio_clip();
        let address = AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id },
            rack: AudioChannelStripRack::PreFader,
        };
        let first = insert(
            &mut sequence,
            address,
            AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1),
        );
        let second = insert(
            &mut sequence,
            address,
            AudioProcessorInstance::built_in(BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID, 1),
        );
        let moved = apply_audio_processor_rack_edit(
            &mut sequence,
            &request(
                address,
                AudioProcessorRackEdit::Move {
                    processor_id: second,
                    placement: AudioProcessorRackPlacement::Before { processor_id: first },
                },
            ),
        )
        .expect("move by stable anchor");
        assert!(moved.changed);
        assert_eq!(
            audio_processor_rack(&sequence, &address)
                .expect("Rack")
                .processors
                .iter()
                .map(|processor| processor.id)
                .collect::<Vec<_>>(),
            vec![second, first]
        );

        assert!(
            apply_audio_processor_rack_edit(
                &mut sequence,
                &request(
                    address,
                    AudioProcessorRackEdit::SetBypassed { processor_id: first, bypassed: true },
                ),
            )
            .expect("bypass")
            .changed
        );
        assert!(
            !apply_audio_processor_rack_edit(
                &mut sequence,
                &request(
                    address,
                    AudioProcessorRackEdit::SetBypassed { processor_id: first, bypassed: true },
                ),
            )
            .expect("bypass no-op")
            .changed
        );

        let parameter_id = ParameterId::new_static(GAIN_DB_PARAMETER_ID);
        apply_audio_processor_rack_edit(
            &mut sequence,
            &request(
                address,
                AudioProcessorRackEdit::SetParameterStaticValue {
                    processor_id: first,
                    parameter_id: parameter_id.clone(),
                    value: -6.0,
                },
            ),
        )
        .expect("static parameter");
        let gain = audio_processor_rack(&sequence, &address)
            .expect("Rack")
            .processors
            .iter()
            .find(|processor| processor.id == first)
            .expect("Gain");
        assert_eq!(
            gain.parameters[&parameter_id]
                .automation
                .as_ref()
                .expect("bound host curve")
                .default_value,
            -6.0
        );
        assert!(gain.parameters[&parameter_id]
            .automation
            .as_ref()
            .expect("bound host curve")
            .keyframes
            .is_empty());
    }

    #[test]
    fn clap_rebind_preserves_instance_state_and_rejects_stale_or_cross_plugin_requests() {
        let (mut sequence, track_id, _) = sequence_with_audio_clip();
        let address = AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id },
            rack: AudioChannelStripRack::PreFader,
        };
        let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        processor.definition = AudioProcessorDefinitionRef::Clap {
            plugin_id: "org.example.gain".to_owned(),
            schema_version: 1,
            binary_sha256: None,
        };
        processor.opaque_state = Some(vec![1, 2, 3].into());
        let id = insert(&mut sequence, address, processor.clone());
        let expected = processor.definition.clone();
        let next = AudioProcessorDefinitionRef::Clap {
            plugin_id: "org.example.gain".to_owned(),
            schema_version: 1,
            binary_sha256: Some([7; 32]),
        };
        let rebind = |expected_definition, new_definition| {
            request(
                address,
                AudioProcessorRackEdit::RebindNative {
                    processor_id: id,
                    expected_definition,
                    new_definition,
                },
            )
        };
        let before = sequence.clone();
        assert_eq!(
            apply_audio_processor_rack_edit(
                &mut sequence,
                &rebind(
                    expected.clone(),
                    AudioProcessorDefinitionRef::Clap {
                        plugin_id: "org.example.other".to_owned(),
                        schema_version: 1,
                        binary_sha256: Some([7; 32]),
                    }
                ),
            ),
            Err(AudioProcessorRackEditError::IncompatibleRebind)
        );
        assert_eq!(sequence, before);
        assert!(
            apply_audio_processor_rack_edit(&mut sequence, &rebind(expected.clone(), next.clone()))
                .expect("rebind")
                .changed
        );
        let updated = &audio_processor_rack(&sequence, &address).expect("rack").processors[0];
        assert_eq!(updated.id, processor.id);
        assert_eq!(updated.definition, next);
        assert_eq!(updated.parameters, processor.parameters);
        assert_eq!(updated.opaque_state, processor.opaque_state);
        let after = sequence.clone();
        assert_eq!(
            apply_audio_processor_rack_edit(&mut sequence, &rebind(expected, next)),
            Err(AudioProcessorRackEditError::IncompatibleRebind)
        );
        assert_eq!(sequence, after);
    }

    #[test]
    fn vst3_rebind_requires_same_class_and_vendor_and_keeps_author_state() {
        let (mut sequence, track_id, _) = sequence_with_audio_clip();
        let address = AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id },
            rack: AudioChannelStripRack::PreFader,
        };
        let mut processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        processor.definition = AudioProcessorDefinitionRef::Vst3 {
            class_id: "0123456789ABCDEF0123456789ABCDEF".to_owned(),
            vendor: Some("Example".to_owned()),
            schema_version: 1,
            binary_sha256: None,
        };
        processor.opaque_state = Some(vec![1, 2, 3].into());
        let id = insert(&mut sequence, address, processor.clone());
        let expected = processor.definition.clone();
        let rebound = AudioProcessorDefinitionRef::Vst3 {
            class_id: "0123456789ABCDEF0123456789ABCDEF".to_owned(),
            vendor: Some("Example".to_owned()),
            schema_version: 1,
            binary_sha256: Some([9; 32]),
        };
        let edit = |new_definition| {
            request(
                address,
                AudioProcessorRackEdit::RebindNative {
                    processor_id: id,
                    expected_definition: expected.clone(),
                    new_definition,
                },
            )
        };
        let before = sequence.clone();
        assert_eq!(
            apply_audio_processor_rack_edit(
                &mut sequence,
                &edit(AudioProcessorDefinitionRef::Vst3 {
                    class_id: "0123456789ABCDEF0123456789ABCDEF".to_owned(),
                    vendor: Some("Other".to_owned()),
                    schema_version: 1,
                    binary_sha256: Some([9; 32]),
                })
            ),
            Err(AudioProcessorRackEditError::IncompatibleRebind)
        );
        assert_eq!(sequence, before);
        assert!(
            apply_audio_processor_rack_edit(&mut sequence, &edit(rebound.clone()))
                .expect("rebind VST3")
                .changed
        );
        let updated = &audio_processor_rack(&sequence, &address).expect("rack").processors[0];
        assert_eq!(updated.definition, rebound);
        assert_eq!(updated.parameters, processor.parameters);
        assert_eq!(updated.opaque_state, processor.opaque_state);
    }

    #[test]
    fn locked_scope_duplicate_identity_and_bad_anchor_fail_without_partial_mutation() {
        let (mut sequence, track_id, scope_id) = sequence_with_audio_clip();
        let track_address = AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id },
            rack: AudioChannelStripRack::PostFader,
        };
        let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let processor_id = insert(&mut sequence, track_address, processor.clone());

        let missing_anchor = AudioProcessorInstanceId::new();
        let before_bad_anchor = sequence.clone();
        assert_eq!(
            apply_audio_processor_rack_edit(
                &mut sequence,
                &request(
                    track_address,
                    AudioProcessorRackEdit::Move {
                        processor_id,
                        placement: AudioProcessorRackPlacement::Before {
                            processor_id: missing_anchor,
                        },
                    },
                ),
            ),
            Err(AudioProcessorRackEditError::UnknownPlacementAnchor(
                missing_anchor
            ))
        );
        assert_eq!(sequence, before_bad_anchor);

        let output_address = AudioProcessorRackAddress::ChannelStrip {
            owner: AudioChannelStripOwner::ProgramOutput {
                output_id: sequence.audio_program.outputs[0].id,
            },
            rack: AudioChannelStripRack::PostFader,
        };
        let before_duplicate = sequence.clone();
        assert_eq!(
            apply_audio_processor_rack_edit(
                &mut sequence,
                &request(
                    output_address,
                    AudioProcessorRackEdit::Insert {
                        processor,
                        placement: AudioProcessorRackPlacement::End,
                    },
                ),
            ),
            Err(AudioProcessorRackEditError::DuplicateProcessor(
                processor_id
            ))
        );
        assert_eq!(sequence, before_duplicate);

        sequence.audio_tracks[0].is_locked = true;
        let before_locked = sequence.clone();
        assert_eq!(
            apply_audio_processor_rack_edit(
                &mut sequence,
                &request(
                    AudioProcessorRackAddress::ProcessingScope { scope_id },
                    AudioProcessorRackEdit::Insert {
                        processor: AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1,),
                        placement: AudioProcessorRackPlacement::End,
                    },
                ),
            ),
            Err(AudioProcessorRackEditError::LockedProcessingScopeBinding { scope_id, track_id })
        );
        assert_eq!(sequence, before_locked);
    }
}
