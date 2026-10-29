//! Canonical native audio author gestures, using the existing Session history.
//!
//! These handles authorize author targets only. Product adapters must additionally
//! prove the exact native object, current runtime owner/epoch, ordered source and
//! quiescent state origin before applying any callback. No native instance, window,
//! event queue, controller, runtime install lease or second history lives here.

use crate::{AuthoringCommit, AuthoringSession, AuthoringSessionId};
use mondrian_core::{
    AudioProcessingScopeId, AudioProcessorInstanceId, AuthoringGestureId, MondrianError, Result,
    SequenceId,
};
use mondrian_timeline::audio::AudioProcessorDefinitionRef;
use mondrian_timeline::{
    apply_audio_processor_rack_edit, inspect_audio_component, inspect_native_audio_authoring,
    AudioChannelStripOwner, AudioChannelStripRack, AudioComponentAddress,
    AudioProcessorRackAddress, AudioProcessorRackEdit, AudioProcessorRackEditRequest,
    NativeAudioAuthorUpdate,
};

/// Exact author owner of a native editor, without an execution identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioAuthorOwner {
    /// Processor on one canonical Channel Strip and insertion stage.
    ChannelStrip {
        /// Stable Track, Bus or Program Output owner.
        owner: AudioChannelStripOwner,
        /// Exact pre- or post-fader Rack.
        rack: AudioChannelStripRack,
    },
    /// One explicit materialization of a shared Processing Scope.
    ProcessingScope {
        /// Shared authored processing definition.
        scope_id: AudioProcessingScopeId,
        /// Exact Clip and Component occurrence that owns this editor surface.
        occurrence: AudioComponentAddress,
    },
}

impl NativeAudioAuthorOwner {
    /// Canonical Rack address; Scope occurrence remains separately checked.
    pub fn rack_address(self) -> AudioProcessorRackAddress {
        match self {
            Self::ChannelStrip { owner, rack } => {
                AudioProcessorRackAddress::ChannelStrip { owner, rack }
            }
            Self::ProcessingScope { scope_id, .. } => {
                AudioProcessorRackAddress::ProcessingScope { scope_id }
            }
        }
    }

    fn admit(
        self,
        session: &AuthoringSession,
        sequence_id: SequenceId,
        processor_id: AudioProcessorInstanceId,
        definition: &AudioProcessorDefinitionRef,
    ) -> Result<()> {
        if session.active_sequence().is_none_or(|sequence| sequence.id != sequence_id) {
            return Err(native_error(
                "native editor author Sequence is no longer active",
            ));
        }
        let sequence = session
            .sequence(sequence_id)
            .ok_or_else(|| native_error("native editor author Sequence is absent"))?;
        if let Self::ProcessingScope { scope_id, occurrence } = self {
            let inspection = inspect_audio_component(sequence, occurrence).map_err(native_error)?;
            if !inspection.is_editable() || inspection.component().processing.scope_id != scope_id {
                return Err(native_error(
                    "native editor Component occurrence is no longer editable or bound",
                ));
            }
        }
        inspect_native_audio_authoring(sequence, &self.rack_address(), processor_id, definition)
            .map_err(native_error)?;
        Ok(())
    }
}

/// An issued handle into the sole Session sequence gesture.
///
/// Private fields prevent rebuilding an old handle against a new Session. It
/// cannot authorize native callbacks by itself: execution object/epoch proof is
/// required independently by the product adapter. It retains no author snapshot.
#[derive(Debug, Clone)]
pub struct NativeAudioAuthorGesture {
    session_id: AuthoringSessionId,
    sequence_id: SequenceId,
    gesture_id: AuthoringGestureId,
    owner: NativeAudioAuthorOwner,
    processor_id: AudioProcessorInstanceId,
    definition: AudioProcessorDefinitionRef,
}

impl NativeAudioAuthorGesture {
    /// Open Session identity owning this gesture.
    pub fn session_id(&self) -> AuthoringSessionId {
        self.session_id
    }
    /// Exact authored Sequence; never inferred from a later callback.
    pub fn sequence_id(&self) -> SequenceId {
        self.sequence_id
    }
    /// Existing history group identity, not a second history authority.
    pub fn gesture_id(&self) -> AuthoringGestureId {
        self.gesture_id
    }
    /// Exact author occurrence/strip inspected at issue.
    pub fn owner(&self) -> NativeAudioAuthorOwner {
        self.owner
    }
    /// Authored processor instance that receives targeted writes.
    pub fn processor_id(&self) -> AudioProcessorInstanceId {
        self.processor_id
    }

    fn admit(&self, session: &AuthoringSession) -> Result<()> {
        if session.session_id() != self.session_id
            || !session.native_sequence_gesture_is_current(self.gesture_id, self.sequence_id)
        {
            return Err(native_error(
                "native author gesture belongs to a stale Session or generation",
            ));
        }
        self.owner.admit(
            session,
            self.sequence_id,
            self.processor_id,
            &self.definition,
        )
    }
}

/// One canonical author target and terminal update supplied by the product.
///
/// These fields do not authenticate native origin, a Runtime frontier or Ack.
#[derive(Debug, Clone)]
pub struct NativeAudioTerminalAuthorMember {
    /// Exact canonical Rack owner; execution occurrence admission is separate.
    pub owner: NativeAudioAuthorOwner,
    /// Authored Processor receiving this update.
    pub processor_id: AudioProcessorInstanceId,
    /// Pinned definition captured for that actual native source.
    pub expected_definition: AudioProcessorDefinitionRef,
    /// Schema-qualified operations and optional coherent opaque replacement.
    pub update: NativeAudioAuthorUpdate,
    /// Actual source finality, independently authenticated by the product.
    pub completion: mondrian_core::NativeAudioAuthorCompletion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalAuthorTarget {
    owner: NativeAudioAuthorOwner,
    processor_id: AudioProcessorInstanceId,
    definition: AudioProcessorDefinitionRef,
}

impl TerminalAuthorTarget {
    fn of(member: &NativeAudioTerminalAuthorMember) -> Self {
        Self {
            owner: member.owner,
            processor_id: member.processor_id,
            definition: member.expected_definition.clone(),
        }
    }

    fn same_author_target(&self, other: &Self) -> bool {
        self.owner.rack_address() == other.owner.rack_address()
            && self.processor_id == other.processor_id
    }

    fn admit(&self, session: &AuthoringSession, sequence: SequenceId) -> Result<()> {
        self.owner.admit(session, sequence, self.processor_id, &self.definition)
    }
}

#[derive(Debug)]
struct FrozenTerminalAuthorMember {
    target: TerminalAuthorTarget,
    completion: mondrian_core::NativeAudioAuthorCompletion,
    supplied_state: bool,
}

#[derive(Debug)]
enum TerminalAuthorGroupPhase {
    Collecting,
    Prepared,
    Published(Vec<FrozenTerminalAuthorMember>),
    Finished,
}

/// Exclusive handle to the Session's existing native-pending Sequence group.
///
/// This owns only author admission and phase metadata. It neither retains a
/// second Document/History nor proves a complete native frontier. Drop never
/// finishes or cancels the Session group.
#[derive(Debug)]
pub struct NativeAudioTerminalAuthorGroup {
    session_id: AuthoringSessionId,
    sequence_id: SequenceId,
    gesture_id: AuthoringGestureId,
    expected_generation: crate::AuthorGeneration,
    first: TerminalAuthorTarget,
    phase: TerminalAuthorGroupPhase,
}

impl NativeAudioTerminalAuthorGroup {
    /// Original Session lifetime; this is not an execution permission.
    pub const fn session_id(&self) -> AuthoringSessionId {
        self.session_id
    }
    /// Sole authored Sequence of this group.
    pub const fn sequence_id(&self) -> SequenceId {
        self.sequence_id
    }
    /// Existing History group identity, not a native Source receipt.
    pub const fn gesture_id(&self) -> AuthoringGestureId {
        self.gesture_id
    }

    fn admit(&self, session: &AuthoringSession) -> Result<()> {
        if session.session_id() != self.session_id
            || session.author_generation() != self.expected_generation
            || !session.native_sequence_gesture_is_current(self.gesture_id, self.sequence_id)
        {
            return Err(native_error("terminal author group is stale or foreign"));
        }
        self.first.admit(session, self.sequence_id)
    }
}

struct PreparedTerminalAuthorCandidate {
    session_id: AuthoringSessionId,
    sequence_id: SequenceId,
    gesture_id: AuthoringGestureId,
    expected_generation: crate::AuthorGeneration,
    before: mondrian_timeline::Sequence,
    members: Vec<FrozenTerminalAuthorMember>,
    prepared: Option<crate::session::PreparedSequenceEdit>,
}

impl PreparedTerminalAuthorCandidate {
    fn sequence(&self) -> &mondrian_timeline::Sequence {
        match &self.prepared {
            Some(prepared) => prepared
                .candidate_sequences()
                .iter()
                .find(|sequence| sequence.id == self.sequence_id)
                .expect("the original prepared edit retains its target Sequence"),
            None => &self.before,
        }
    }
}

/// Owned, non-Clone private candidate for one frozen terminal member set.
///
/// Reuses the original prepared Sequence edit. NoChange retains the real before
/// and group proof as well. Failed commit/discard preserves this custody; Drop
/// never clears the Session's native-pending barrier.
pub struct PreparedNativeAudioTerminalAuthorGroup {
    candidate: Option<PreparedTerminalAuthorCandidate>,
}

impl PreparedNativeAudioTerminalAuthorGroup {
    /// Borrow the unpublished candidate for canonical compilation.
    ///
    /// This is not a current AuthorExecutionSnapshot or native/Source authority.
    /// Successful commit or discard consumes it and returns None thereafter.
    pub fn candidate_sequence(&self) -> Option<&mondrian_timeline::Sequence> {
        self.candidate.as_ref().map(PreparedTerminalAuthorCandidate::sequence)
    }
}

impl AuthoringSession {
    /// Begin a group of native author edits after current author-target admission.
    ///
    /// The caller must already have admitted exact callback/runtime origin and
    /// finished any product UI gesture. Begin itself changes no author state.
    pub fn begin_native_audio_gesture(
        &mut self,
        sequence_id: SequenceId,
        owner: NativeAudioAuthorOwner,
        processor_id: AudioProcessorInstanceId,
        expected_definition: AudioProcessorDefinitionRef,
    ) -> Result<NativeAudioAuthorGesture> {
        owner.admit(self, sequence_id, processor_id, &expected_definition)?;
        let gesture_id = AuthoringGestureId::new();
        self.begin_native_sequence_gesture(gesture_id, sequence_id)?;
        Ok(NativeAudioAuthorGesture {
            session_id: self.session_id(),
            sequence_id,
            gesture_id,
            owner,
            processor_id,
            definition: expected_definition,
        })
    }

    /// Atomically apply targeted parameters/state into the existing bounded group.
    ///
    /// Each value carries its chosen exact owner-time policy. Unkeyed writes
    /// refuse existing keys; qualified writes preserve curves through the sole
    /// canonical automation contract. This method cannot prove native origin,
    /// perform native rollback or install a DSP configuration.
    pub fn update_native_audio_gesture(
        &mut self,
        gesture: &NativeAudioAuthorGesture,
        update: NativeAudioAuthorUpdate,
    ) -> Result<Option<AuthoringCommit>> {
        gesture.admit(self)?;
        let request = AudioProcessorRackEditRequest {
            address: gesture.owner.rack_address(),
            edit: AudioProcessorRackEdit::SetNativeAuthorState {
                processor_id: gesture.processor_id,
                expected_definition: gesture.definition.clone(),
                update,
            },
        };
        let (_, commit) = self.edit_native_sequence_gesture(
            gesture.gesture_id,
            gesture.sequence_id,
            "Edit native audio processor",
            |sequence| apply_audio_processor_rack_edit(sequence, &request).map_err(native_error),
        )?;
        Ok(commit)
    }

    /// Seal an explicitly proven native parameter or configuration completion.
    ///
    /// The product must prove a terminal ordered source boundary and coherent
    /// control-side configuration on the exact current native owner. An author
    /// handle alone is not that runtime proof. ParametersOnly completion is
    /// admitted only without old/new opaque state; it cannot save a stale seed.
    /// CoherentConfiguration requires successfully captured opaque bytes.
    /// Invalid completion leaves the pending group and its restore point intact.
    pub fn finish_native_audio_gesture(
        &mut self,
        gesture: &NativeAudioAuthorGesture,
        completion: NativeAudioAuthorUpdate,
        finality: mondrian_core::NativeAudioAuthorCompletion,
    ) -> Result<()> {
        gesture.admit(self)?;

        match finality {
            mondrian_core::NativeAudioAuthorCompletion::Nonterminal => {
                return Err(native_error("native completion is not terminal"));
            }
            mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration => {
                if completion.opaque_state.is_none() {
                    return Err(native_error(
                        "native completion lacks coherent opaque configuration",
                    ));
                }
            }
            mondrian_core::NativeAudioAuthorCompletion::ParametersOnly => {
                let sequence = self
                    .sequence(gesture.sequence_id)
                    .ok_or_else(|| native_error("native completion Sequence is absent"))?;
                let processor = inspect_native_audio_authoring(
                    sequence,
                    &gesture.owner.rack_address(),
                    gesture.processor_id,
                    &gesture.definition,
                )
                .map_err(native_error)?;
                if completion.opaque_state.is_some() || processor.opaque_state.is_some() {
                    return Err(native_error(
                        "parameter completion cannot retain or replace opaque configuration",
                    ));
                }
            }
        }
        self.update_native_audio_gesture(gesture, completion)?;
        self.finish_native_sequence_gesture(gesture.gesture_id)?;
        Ok(())
    }

    /// Cancel only a current admitted group through canonical Session restoration.
    ///
    /// Native runtime reseeding remains an independent exact-owner request. An
    /// unrelated author commit invalidates this handle and is never rolled back.
    pub fn cancel_native_audio_gesture(
        &mut self,
        gesture: &NativeAudioAuthorGesture,
    ) -> Result<Option<AuthoringCommit>> {
        gesture.admit(self)?;
        self.cancel_sequence_gesture(gesture.gesture_id)
    }
}

impl AuthoringSession {
    /// Admit the first real member and open only the original native barrier.
    ///
    /// The product authenticates source origin before calling. This validates
    /// canonical target/schema/finality without publishing its trial Sequence.
    pub fn begin_native_audio_terminal_group(
        &mut self,
        sequence_id: SequenceId,
        first: NativeAudioTerminalAuthorMember,
    ) -> Result<NativeAudioTerminalAuthorGroup> {
        let target = TerminalAuthorTarget::of(&first);
        target.admit(self, sequence_id)?;
        let before = self
            .sequence(sequence_id)
            .ok_or_else(|| native_error("terminal author Sequence is absent"))?;
        admit_terminal_finality(
            before,
            &target,
            first.completion,
            first.update.opaque_state.is_some(),
        )?;
        let mut trial = before.clone();
        apply_terminal_member(&mut trial, first)?;
        let gesture_id = AuthoringGestureId::new();
        self.begin_native_terminal_sequence_gesture(gesture_id, sequence_id)?;
        Ok(NativeAudioTerminalAuthorGroup {
            session_id: self.session_id(),
            sequence_id,
            gesture_id,
            expected_generation: self.author_generation(),
            first: target,
            phase: TerminalAuthorGroupPhase::Collecting,
        })
    }

    /// Freeze all members into one unpublished, group-aware canonical candidate.
    ///
    /// Collecting may add peers before this call. All targets must belong to the
    /// same Sequence, be unique and terminal, and include the first exact target.
    /// Any error leaves Document/History/generation and the original barrier intact.
    pub fn prepare_native_audio_terminal_group(
        &mut self,
        group: &mut NativeAudioTerminalAuthorGroup,
        members: Vec<NativeAudioTerminalAuthorMember>,
    ) -> Result<PreparedNativeAudioTerminalAuthorGroup> {
        group.admit(self)?;
        if !matches!(&group.phase, TerminalAuthorGroupPhase::Collecting) {
            return Err(native_error("terminal author group is not collecting"));
        }
        let before = self
            .sequence(group.sequence_id)
            .cloned()
            .ok_or_else(|| native_error("terminal author Sequence is absent"))?;
        let mut after = before.clone();
        let mut frozen = Vec::with_capacity(members.len());
        for member in members {
            let target = TerminalAuthorTarget::of(&member);
            if frozen
                .iter()
                .any(|prior: &FrozenTerminalAuthorMember| prior.target.same_author_target(&target))
            {
                return Err(native_error("terminal author member is duplicated"));
            }
            target.admit(self, group.sequence_id)?;
            let supplied_state = member.update.opaque_state.is_some();
            admit_terminal_finality(&before, &target, member.completion, supplied_state)?;
            frozen.push(FrozenTerminalAuthorMember {
                target,
                completion: member.completion,
                supplied_state,
            });
            apply_terminal_member(&mut after, member)?;
        }
        admit_terminal_members(self, group, &before, &after, &frozen)?;
        let prepared = self.prepare_native_sequence_candidate(
            group.gesture_id,
            group.sequence_id,
            before.clone(),
            after,
        )?;
        let ticket = PreparedNativeAudioTerminalAuthorGroup {
            candidate: Some(PreparedTerminalAuthorCandidate {
                session_id: self.session_id(),
                sequence_id: group.sequence_id,
                gesture_id: group.gesture_id,
                expected_generation: self.author_generation(),
                before,
                members: frozen,
                prepared,
            }),
        };
        group.phase = TerminalAuthorGroupPhase::Prepared;
        Ok(ticket)
    }

    /// Publish once after all asynchronous preparation has returned successfully.
    ///
    /// Rechecks the actual Session, group, generation, before/current and frozen
    /// members before consuming anything. This does not atomically commit a
    /// native Source; later Source failure retains the accepted Document/barrier.
    pub fn commit_native_audio_terminal_group(
        &mut self,
        group: &mut NativeAudioTerminalAuthorGroup,
        ticket: &mut PreparedNativeAudioTerminalAuthorGroup,
    ) -> Result<Option<AuthoringCommit>> {
        self.admit_native_audio_terminal_candidate(group, ticket)?;
        let candidate = ticket.candidate.take().expect("admitted owned candidate");
        let commit =
            candidate.prepared.map(|prepared| self.install_prepared_sequence_edit(prepared));
        group.expected_generation = self.author_generation();
        group.phase = TerminalAuthorGroupPhase::Published(candidate.members);
        Ok(commit)
    }

    /// Explicitly discard an unpublished candidate so real peers can join.
    ///
    /// The first anchor, Document and native barrier remain. A consumed, stale
    /// or foreign ticket cannot unlock a different or later candidate.
    pub fn discard_native_audio_terminal_candidate(
        &mut self,
        group: &mut NativeAudioTerminalAuthorGroup,
        ticket: &mut PreparedNativeAudioTerminalAuthorGroup,
    ) -> Result<()> {
        self.admit_native_audio_terminal_candidate(group, ticket)?;
        let retired = ticket.candidate.take().expect("admitted owned candidate");
        group.phase = TerminalAuthorGroupPhase::Collecting;
        drop(retired);
        Ok(())
    }

    fn admit_native_audio_terminal_candidate(
        &self,
        group: &NativeAudioTerminalAuthorGroup,
        ticket: &PreparedNativeAudioTerminalAuthorGroup,
    ) -> Result<()> {
        group.admit(self)?;
        if !matches!(&group.phase, TerminalAuthorGroupPhase::Prepared) {
            return Err(native_error(
                "terminal author group has no frozen candidate",
            ));
        }
        let candidate = ticket
            .candidate
            .as_ref()
            .ok_or_else(|| native_error("terminal author candidate was already consumed"))?;
        if candidate.session_id != group.session_id
            || candidate.sequence_id != group.sequence_id
            || candidate.gesture_id != group.gesture_id
            || candidate.expected_generation != group.expected_generation
        {
            return Err(native_error(
                "terminal author candidate belongs to another group",
            ));
        }
        self.admit_prepared_native_sequence_candidate(
            group.gesture_id,
            group.sequence_id,
            candidate.expected_generation,
            &candidate.before,
            candidate.prepared.as_ref(),
        )?;
        admit_terminal_members(
            self,
            group,
            &candidate.before,
            candidate.sequence(),
            &candidate.members,
        )
    }

    /// Finish the one original History group only after every actual native Ack.
    ///
    /// The product must prove those Acks and Source closure separately. This
    /// method validates author custody only and performs no member state writes.
    pub fn finish_native_audio_terminal_group(
        &mut self,
        group: &mut NativeAudioTerminalAuthorGroup,
    ) -> Result<()> {
        group.admit(self)?;
        let TerminalAuthorGroupPhase::Published(members) = &group.phase else {
            return Err(native_error("terminal author group was not published"));
        };
        let current = self
            .sequence(group.sequence_id)
            .ok_or_else(|| native_error("terminal author Sequence is absent"))?;
        admit_terminal_members(self, group, current, current, members)?;
        self.finish_native_sequence_gesture(group.gesture_id)?;
        group.phase = TerminalAuthorGroupPhase::Finished;
        Ok(())
    }
}

fn apply_terminal_member(
    sequence: &mut mondrian_timeline::Sequence,
    member: NativeAudioTerminalAuthorMember,
) -> Result<()> {
    apply_audio_processor_rack_edit(
        sequence,
        &AudioProcessorRackEditRequest {
            address: member.owner.rack_address(),
            edit: AudioProcessorRackEdit::SetNativeAuthorState {
                processor_id: member.processor_id,
                expected_definition: member.expected_definition,
                update: member.update,
            },
        },
    )
    .map(|_| ())
    .map_err(native_error)
}

fn admit_terminal_finality(
    before: &mondrian_timeline::Sequence,
    target: &TerminalAuthorTarget,
    completion: mondrian_core::NativeAudioAuthorCompletion,
    supplied_state: bool,
) -> Result<()> {
    use mondrian_core::NativeAudioAuthorCompletion as Completion;
    let processor = inspect_native_audio_authoring(
        before,
        &target.owner.rack_address(),
        target.processor_id,
        &target.definition,
    )
    .map_err(native_error)?;
    match completion {
        Completion::Nonterminal => Err(native_error("terminal author member is not terminal")),
        Completion::CoherentConfiguration if !supplied_state => Err(native_error(
            "terminal author member lacks coherent opaque state",
        )),
        Completion::ParametersOnly if supplied_state || processor.opaque_state.is_some() => Err(
            native_error("parameter-only member cannot retain or replace opaque state"),
        ),
        _ => Ok(()),
    }
}

fn admit_terminal_members(
    session: &AuthoringSession,
    group: &NativeAudioTerminalAuthorGroup,
    before: &mondrian_timeline::Sequence,
    after: &mondrian_timeline::Sequence,
    members: &[FrozenTerminalAuthorMember],
) -> Result<()> {
    if members.is_empty()
        || before.id != group.sequence_id
        || after.id != group.sequence_id
        || !members.iter().any(|member| member.target == group.first)
    {
        return Err(native_error(
            "terminal author member set is empty or lacks its first target",
        ));
    }
    for (index, member) in members.iter().enumerate() {
        if members[..index]
            .iter()
            .any(|prior| prior.target.same_author_target(&member.target))
        {
            return Err(native_error("terminal author member is duplicated"));
        }
        member.target.admit(session, group.sequence_id)?;
        admit_terminal_finality(
            before,
            &member.target,
            member.completion,
            member.supplied_state,
        )?;
        let current = inspect_native_audio_authoring(
            after,
            &member.target.owner.rack_address(),
            member.target.processor_id,
            &member.target.definition,
        )
        .map_err(native_error)?;
        if member.completion == mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration
            && current.opaque_state.is_none()
        {
            return Err(native_error(
                "terminal author candidate lost coherent opaque state",
            ));
        }
        if member.completion == mondrian_core::NativeAudioAuthorCompletion::ParametersOnly
            && current.opaque_state.is_some()
        {
            return Err(native_error(
                "parameter-only candidate contains opaque state",
            ));
        }
    }
    Ok(())
}

fn native_error(reason: impl std::fmt::Display) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "native_audio_authoring".to_owned(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_assets::AssetLibrary;
    use mondrian_core::automation::{
        ParameterInvalidValuePolicy, ParameterNumericContract, ParameterSchema, ParameterUnit,
        PropertyValue,
    };
    use mondrian_core::{
        AssetId, AudioProcessorInstanceId, AudioSourceComponentId, AuthoringList, ParameterId,
        ProjectColorEnvironment, ProjectSettings, TimelineTime,
    };
    use mondrian_project::{
        load_project_archive, save_project_archive_from_open_library_with_publication,
        ProjectArchivePublication, ProjectDocument,
    };
    use mondrian_timeline::audio::{AudioProcessorInstance, AudioProcessorParameter};
    use mondrian_timeline::{
        audio_processor_rack, AudioProcessorRackPlacement, Clip, NativeAudioParameterWrite,
        Sequence, SequenceCollection, SequenceSettings,
    };

    struct Fixture {
        session: AuthoringSession,
        sequence_id: SequenceId,
        owner: NativeAudioAuthorOwner,
        processor_id: AudioProcessorInstanceId,
        definition: AudioProcessorDefinitionRef,
        parameter_ids: [ParameterId; 2],
        root: tempfile::TempDir,
    }

    impl Fixture {
        fn new(scope: bool) -> Self {
            Self::with_initial_configuration(scope, Some(vec![1, 2]))
        }
        fn with_initial_configuration(scope: bool, configuration: Option<Vec<u8>>) -> Self {
            let root = tempfile::tempdir().expect("runtime");
            let library = AssetLibrary::open(root.path().join("library")).expect("Library");
            let mut sequence = Sequence::new("Native author contract");
            let sequence_id = sequence.id;
            let track_id = sequence.audio_tracks[0].id;
            let owner = if scope {
                let clip = Clip::new(
                    AssetId::new(),
                    TimelineTime::ZERO,
                    TimelineTime::new(4, 1).expect("duration"),
                )
                .expect("Clip");
                sequence
                    .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
                    .expect("audio Clip");
                let clip = &sequence.audio_tracks[0].clips[0];
                let component = &clip.audio_components[0];
                NativeAudioAuthorOwner::ProcessingScope {
                    scope_id: component.processing.scope_id,
                    occurrence: AudioComponentAddress {
                        track_id,
                        clip_id: clip.id,
                        edit_id: component.id,
                    },
                }
            } else {
                NativeAudioAuthorOwner::ChannelStrip {
                    owner: AudioChannelStripOwner::Track { track_id },
                    rack: AudioChannelStripRack::PreFader,
                }
            };
            let definition = AudioProcessorDefinitionRef::Vst3 {
                class_id: "1234567890ABCDEF1234567890ABCDEF".to_owned(),
                vendor: Some("Contract fixture".to_owned()),
                schema_version: 1,
                binary_sha256: Some([1; 32]),
            };
            let parameter_ids = [
                ParameterId::new("vst3.param.5").expect("ID"),
                ParameterId::new("vst3.param.9").expect("ID"),
            ];
            let mut processor = AudioProcessorInstance {
                id: AudioProcessorInstanceId::new(),
                definition: definition.clone(),
                bypassed: false,
                parameters: Default::default(),
                opaque_state: configuration.map(AuthoringList::from),
            };
            for (parameter_id, value) in parameter_ids.iter().zip([0.25, 0.75]) {
                let schema =
                    ParameterSchema::v1(parameter_id.clone(), PropertyValue::Double(value))
                        .with_numeric_contract(
                            ParameterUnit::Unitless,
                            ParameterNumericContract::closed(
                                0.0,
                                1.0,
                                None,
                                ParameterInvalidValuePolicy::Reject,
                            )
                            .expect("native continuous range"),
                        );
                processor.parameters.insert(
                    parameter_id.clone(),
                    AudioProcessorParameter::from_schema(schema).expect("native author schema"),
                );
            }
            let processor_id = processor.id;
            apply_audio_processor_rack_edit(
                &mut sequence,
                &AudioProcessorRackEditRequest {
                    address: owner.rack_address(),
                    edit: AudioProcessorRackEdit::Insert {
                        processor,
                        placement: AudioProcessorRackPlacement::End,
                    },
                },
            )
            .expect("insert exact author instance");
            let document = ProjectDocument::new(
                "Native author project",
                SequenceCollection::new(sequence),
                ProjectColorEnvironment::default(),
                SequenceSettings::default(),
                ProjectSettings::default(),
            );
            let session = AuthoringSession::new_unsaved(
                document,
                root.path().join("native.mdp"),
                root.path().to_owned(),
                library,
            )
            .expect("sole Session");
            Self {
                session,
                sequence_id,
                owner,
                processor_id,
                definition,
                parameter_ids,
                root,
            }
        }

        fn begin(&mut self) -> NativeAudioAuthorGesture {
            self.session
                .begin_native_audio_gesture(
                    self.sequence_id,
                    self.owner,
                    self.processor_id,
                    self.definition.clone(),
                )
                .expect("author target")
        }

        fn processor(&self) -> &AudioProcessorInstance {
            &audio_processor_rack(
                self.session.sequence(self.sequence_id).expect("Sequence"),
                &self.owner.rack_address(),
            )
            .expect("Rack")
            .processors[0]
        }

        fn update(
            &self,
            values: &[(usize, f64)],
            bytes: Option<Vec<u8>>,
        ) -> NativeAudioAuthorUpdate {
            NativeAudioAuthorUpdate {
                parameters: values
                    .iter()
                    .map(|(index, value)| {
                        mondrian_timeline::NativeAudioParameterOperation::from(
                            NativeAudioParameterWrite {
                                schema: Box::new(
                                    self.processor().parameters[&self.parameter_ids[*index]]
                                        .schema
                                        .clone(),
                                ),
                                value: *value,
                                mode: mondrian_timeline::NativeAudioParameterWriteMode::Unkeyed,
                            },
                        )
                    })
                    .collect(),
                opaque_state: bytes.map(AuthoringList::from),
            }
        }
    }

    #[test]
    fn native_audio_author_group_parameters_and_opaque_share_one_undo_redo() {
        let mut f = Fixture::new(false);
        let original = f.session.sequence(f.sequence_id).expect("origin").clone();
        let history = f.session.history().diagnostics();
        let gesture = f.begin();
        let first = f.update(&[(0, 0.4)], None);
        assert!(f
            .session
            .update_native_audio_gesture(&gesture, first)
            .expect("first native value")
            .is_some());
        let second = f.update(&[(1, 0.6)], Some(vec![4, 5, 6]));
        assert!(f
            .session
            .update_native_audio_gesture(&gesture, second)
            .expect("second value and state")
            .is_some());

        let completion = f.update(&[], Some(vec![4, 5, 6]));
        f.session
            .finish_native_audio_gesture(
                &gesture,
                completion,
                mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration,
            )
            .expect("group End");
        assert_eq!(
            f.processor().parameters[&f.parameter_ids[0]]
                .automation
                .as_ref()
                .expect("bound host curve")
                .default_value
                .to_bits(),
            0.4_f64.to_bits()
        );
        assert_eq!(
            f.processor().parameters[&f.parameter_ids[1]]
                .automation
                .as_ref()
                .expect("bound host curve")
                .default_value
                .to_bits(),
            0.6_f64.to_bits()
        );
        assert_eq!(
            f.processor().opaque_state.as_deref().map(Vec::as_slice),
            Some(&[4, 5, 6][..])
        );
        let final_state = f.session.sequence(f.sequence_id).expect("final").clone();
        assert_eq!(
            f.session.history().diagnostics().undo_entries,
            history.undo_entries + 1,
            "all native group changes use sole Session history"
        );
        f.session.undo().expect("Undo native group").expect("changed");
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("Undo")
            .author_state_eq_ignoring_revision(&original));
        f.session.redo().expect("Redo native group").expect("changed");
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("Redo")
            .author_state_eq_ignoring_revision(&final_state));
    }

    #[test]
    fn native_audio_author_cancel_preserves_origin_and_redo_branch() {
        let mut f = Fixture::new(false);
        f.session
            .edit_sequence(f.sequence_id, "original branch", |sequence| {
                sequence.name = "Branch".to_owned();
                Ok(())
            })
            .expect("branch");
        f.session.undo().expect("Undo branch");
        let original = f.session.sequence(f.sequence_id).expect("origin").clone();
        let history = f.session.history().diagnostics();
        let gesture = f.begin();
        let update = f.update(&[(0, 0.9), (1, 0.1)], Some(vec![9, 1]));
        f.session
            .update_native_audio_gesture(&gesture, update)
            .expect("live author group");
        assert!(f.session.cancel_native_audio_gesture(&gesture).expect("cancel").is_some());
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("cancelled")
            .author_state_eq_ignoring_revision(&original));
        let after = f.session.history().diagnostics();
        assert_eq!(
            (after.undo_entries, after.redo_entries),
            (history.undo_entries, history.redo_entries)
        );
        f.session.redo().expect("original Redo").expect("branch still retained");
        assert_eq!(
            f.session.sequence(f.sequence_id).expect("restored branch").name,
            "Branch"
        );
    }

    #[test]
    fn native_audio_author_save_archive_reopens_exact_state_and_rejects_old_session() {
        let mut f = Fixture::new(false);
        let gesture = f.begin();
        let update = f.update(
            &[(0, 0.12345678901234568), (1, 0.875)],
            Some(vec![0, 255, 13, 10]),
        );
        f.session
            .update_native_audio_gesture(&gesture, update)
            .expect("native author update");

        let completion = f.update(&[], Some(vec![0, 255, 13, 10]));
        f.session
            .finish_native_audio_gesture(
                &gesture,
                completion,
                mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration,
            )
            .expect("settle before save");
        let snapshot = f.session.snapshot().expect("author snapshot");
        let archive = f.root.path().join("saved-native.mdp");
        let sqlite = snapshot
            .asset_library
            .snapshot_database(
                snapshot.asset_library_revision,
                &f.root.path().join("snapshot-anchor.db"),
            )
            .expect("consistent SQLite");
        let mut reader = sqlite.try_clone_reader().expect("retained SQLite object");
        save_project_archive_from_open_library_with_publication(
            &snapshot.document,
            &mut reader,
            &archive,
            ProjectArchivePublication::CreateNew,
        )
        .expect("durable real mdp");
        let reopened_root = f.root.path().join("reopened");
        let loaded = load_project_archive(&archive, &reopened_root.join("library"))
            .expect("read actual mdp");
        let library = AssetLibrary::open(reopened_root.join("library")).expect("extracted Library");
        let mut reopened =
            AuthoringSession::open_saved(loaded.document, archive, reopened_root, library)
                .expect("new saved Session");
        let current = audio_processor_rack(
            reopened.sequence(f.sequence_id).expect("Sequence"),
            &f.owner.rack_address(),
        )
        .expect("Rack")
        .processors[0]
            .clone();
        assert_eq!(
            current,
            *f.processor(),
            "persistence preserves exact schemas, parameter values and opaque bytes"
        );
        let generation = reopened.author_generation();
        assert!(
            reopened
                .update_native_audio_gesture(&gesture, f.update(&[(0, 0.2)], None))
                .is_err(),
            "same Project reopen cannot admit callbacks from old Session"
        );
        assert_eq!(reopened.author_generation(), generation);
        assert!(!reopened.history().can_undo());
    }

    #[test]
    fn native_audio_author_invalid_batch_and_schema_changes_are_atomic() {
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, 1.1] {
            let mut f = Fixture::new(false);
            let gesture = f.begin();
            let original = f.session.sequence(f.sequence_id).expect("origin").clone();
            let generation = f.session.author_generation();
            let history = f.session.history().diagnostics();
            let update = f.update(&[(0, 0.4), (1, invalid)], Some(vec![8]));
            assert!(f.session.update_native_audio_gesture(&gesture, update).is_err());
            assert_eq!(f.session.author_generation(), generation);
            assert!(f
                .session
                .sequence(f.sequence_id)
                .expect("atomic refusal")
                .author_state_eq_ignoring_revision(&original));
            assert_eq!(
                f.session.history().diagnostics().undo_entries,
                history.undo_entries
            );
        }
        let mut f = Fixture::new(false);
        let gesture = f.begin();
        let original = f.session.sequence(f.sequence_id).expect("origin").clone();
        let mut update = f.update(&[(0, 0.4)], Some(vec![8]));
        let mondrian_timeline::NativeAudioParameterOperation::Value { write } =
            &mut update.parameters[0]
        else {
            panic!("value fixture")
        };
        write.schema.default_value = PropertyValue::Double(0.5);
        assert!(f.session.update_native_audio_gesture(&gesture, update).is_err());
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("schema refusal")
            .author_state_eq_ignoring_revision(&original));
        let update = f.update(&[(0, 0.4), (0, 0.7)], Some(vec![8]));
        f.session
            .update_native_audio_gesture(&gesture, update)
            .expect("explicit ordered values");
        assert_eq!(
            f.processor().parameters[&f.parameter_ids[0]]
                .automation
                .as_ref()
                .expect("bound curve")
                .default_value
                .to_bits(),
            0.7_f64.to_bits()
        );
    }

    #[test]
    fn native_audio_author_stale_generation_definition_lock_and_occurrence_never_rollback_other_edits(
    ) {
        for case in 0..4 {
            let mut f = Fixture::new(true);
            let gesture = f.begin();
            let update = f.update(&[(0, 0.4)], Some(vec![3]));
            f.session
                .update_native_audio_gesture(&gesture, update)
                .expect("first valid gesture update");
            let owner = f.owner;
            let processor_id = f.processor_id;
            assert!(f.session.edit_sequence(f.sequence_id, "cannot implicitly seal native group", |_sequence| -> mondrian_core::Result<()> {
                panic!("pending native state must refuse before running ordinary author closure")
            }).is_err());
            f.session
                .cancel_native_audio_gesture(&gesture)
                .expect("explicitly cancel coherent origin before independent edit");
            f.session
                .edit_sequence(f.sequence_id, "independent author change", |sequence| {
                    match case {
                        0 => sequence.name = "Independent".to_owned(),
                        1 => {
                            let NativeAudioAuthorOwner::ProcessingScope { scope_id, .. } = owner
                            else {
                                panic!("Scope fixture")
                            };
                            let processor = sequence
                                .audio_program
                                .processing_scopes
                                .iter_mut()
                                .find(|scope| scope.id == scope_id)
                                .expect("Scope")
                                .processors
                                .processors
                                .iter_mut()
                                .find(|processor| processor.id == processor_id)
                                .expect("Processor");
                            let AudioProcessorDefinitionRef::Vst3 { binary_sha256, .. } =
                                &mut processor.definition
                            else {
                                panic!("VST3")
                            };
                            *binary_sha256 = Some([2; 32]);
                        }
                        2 => sequence.audio_tracks[0].is_locked = true,
                        3 => {
                            sequence.audio_tracks[0].clips[0].audio_components[0].id =
                                mondrian_core::AudioComponentEditId::new()
                        }
                        _ => unreachable!(),
                    }
                    Ok(())
                })
                .expect("independent canonical edit");
            let current = f.session.sequence(f.sequence_id).expect("current").clone();
            let generation = f.session.author_generation();
            let history = f.session.history().diagnostics();
            let update = f.update(&[(0, 0.6)], Some(vec![4]));
            assert!(
                f.session.update_native_audio_gesture(&gesture, update).is_err(),
                "old editor cannot write after independent author change case{case}"
            );
            assert!(
                f.session.cancel_native_audio_gesture(&gesture).is_err(),
                "stale cancellation cannot restore other author's state"
            );
            assert_eq!(f.session.author_generation(), generation);
            assert_eq!(
                f.session.history().diagnostics().undo_entries,
                history.undo_entries
            );
            assert!(f
                .session
                .sequence(f.sequence_id)
                .expect("current retained")
                .author_state_eq_ignoring_revision(&current));
        }
    }

    #[test]
    fn native_audio_author_keyed_parameter_is_explicitly_refused_without_static_or_opaque_write() {
        let mut f = Fixture::new(false);
        let id = f.parameter_ids[0].clone();
        let owner = f.owner;
        f.session
            .edit_sequence(f.sequence_id, "key parameter", |sequence| {
                let NativeAudioAuthorOwner::ChannelStrip {
                    owner: AudioChannelStripOwner::Track { track_id },
                    ..
                } = owner
                else {
                    panic!("Track fixture")
                };
                sequence
                    .audio_program
                    .track_channels
                    .get_mut(&track_id)
                    .expect("strip")
                    .strip
                    .pre_fader
                    .processors[0]
                    .parameters
                    .get_mut(&id)
                    .expect("parameter")
                    .automation
                    .as_mut()
                    .expect("bound host curve")
                    .keyframes
                    .push(mondrian_core::ExactAutomationKeyframe::linear(
                        TimelineTime::ZERO,
                        0.25,
                    ));
                Ok(())
            })
            .expect("canonical key");
        let gesture = f.begin();
        let original = f.session.sequence(f.sequence_id).expect("origin").clone();
        let update = f.update(&[(0, 0.5), (1, 0.5)], Some(vec![5]));
        assert!(
            f.session.update_native_audio_gesture(&gesture, update).is_err(),
            "keyed native callback requires a chosen canonical automation write policy"
        );
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("refused")
            .author_state_eq_ignoring_revision(&original));
        let request = AudioProcessorRackEditRequest {
            address: owner.rack_address(),
            edit: AudioProcessorRackEdit::SetNativeAuthorState {
                processor_id: f.processor_id,
                expected_definition: f.definition.clone(),
                update: f.update(&[(0, 0.5)], None),
            },
        };
        let mut candidate = original.clone();
        assert!(
            matches!(apply_audio_processor_rack_edit(&mut candidate, &request), Err(mondrian_timeline::AudioProcessorRackEditError::NativeAutomationConflict(parameter)) if parameter == id)
        );
        assert!(candidate.author_state_eq_ignoring_revision(&original));
    }

    #[test]
    fn native_audio_author_noop_keeps_generation_history_and_existing_opaque_state() {
        let mut f = Fixture::new(false);
        let gesture = f.begin();
        let generation = f.session.author_generation();
        let history = f.session.history().diagnostics();
        let update = f.update(&[(0, 0.25), (1, 0.75)], None);
        assert!(f.session.update_native_audio_gesture(&gesture, update).expect("noop").is_none());

        let completion = f.update(&[], Some(vec![1, 2]));
        f.session
            .finish_native_audio_gesture(
                &gesture,
                completion,
                mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration,
            )
            .expect("zero group End");
        assert_eq!(f.session.author_generation(), generation);
        assert_eq!(
            f.session.history().diagnostics().undo_entries,
            history.undo_entries
        );
        assert_eq!(
            f.processor().opaque_state.as_deref().map(Vec::as_slice),
            Some(&[1, 2][..])
        );
    }

    #[test]
    fn native_audio_author_pending_boundary_blocks_save_and_unrelated_author_work() {
        let mut f = Fixture::new(false);
        let original = f.session.sequence(f.sequence_id).expect("origin").clone();
        let gesture = f.begin();
        let update = f.update(&[(0, 0.9)], None);
        f.session
            .update_native_audio_gesture(&gesture, update)
            .expect("live native scalar");
        assert!(f.session.has_pending_native_authoring());
        let current = f.session.document().clone();
        let generation = f.session.author_generation();
        let history = f.session.history().diagnostics();
        assert!(
            f.session.snapshot().is_err(),
            "changed scalar plus old opaque seed cannot enter persistence"
        );
        assert!(f
            .session
            .edit_sequence(
                f.sequence_id,
                "unrelated edit",
                |_sequence| -> mondrian_core::Result<()> {
                    panic!("pending boundary must refuse before the author closure")
                }
            )
            .is_err());
        assert!(f
            .session
            .prepare_sequence_edit(
                f.sequence_id,
                "resource publication",
                |_sequence| -> mondrian_core::Result<()> {
                    panic!("pending boundary must refuse before preparing dependent publication")
                }
            )
            .is_err());
        assert!(f
            .session
            .commit_project_snapshot("unrelated Project edit", current.clone(), current.clone())
            .is_err());
        assert!(f.session.undo().is_err());
        assert!(f.session.redo().is_err());
        assert!(
            f.session.end_sequence_gesture(gesture.gesture_id()).is_err(),
            "ordinary End must not invent native completion"
        );
        assert!(f
            .session
            .begin_sequence_gesture(AuthoringGestureId::new(), f.sequence_id)
            .is_err());
        assert!(f
            .session
            .switch_active_sequence(f.sequence_id, crate::SequenceNavigationIntent::ReplaceRoot)
            .is_err());
        assert_eq!(f.session.document(), &current);
        assert_eq!(f.session.author_generation(), generation);
        assert_eq!(f.session.history().diagnostics(), history);
        f.session
            .cancel_native_audio_gesture(&gesture)
            .expect("explicit current cancel")
            .expect("restore scalar");
        assert!(!f.session.has_pending_native_authoring());
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("restored")
            .author_state_eq_ignoring_revision(&original));
        f.session.snapshot().expect("coherent cancelled state can save");
        f.session
            .edit_sequence(f.sequence_id, "new independent edit", |sequence| {
                sequence.name = "Independent after cancellation".to_owned();
                Ok(())
            })
            .expect("author work resumes");
        let independent = f.session.document().clone();
        assert!(f.session.cancel_native_audio_gesture(&gesture).is_err());
        assert_eq!(
            f.session.document(),
            &independent,
            "late native cancellation cannot undo independent work"
        );
    }

    #[test]
    fn native_audio_author_completion_requires_state_and_preserves_pending_on_invalid_batch() {
        let mut f = Fixture::new(false);
        let original = f.session.sequence(f.sequence_id).expect("origin").clone();
        let history = f.session.history().diagnostics();
        let gesture = f.begin();
        let update = f.update(&[(0, 0.8)], None);
        f.session.update_native_audio_gesture(&gesture, update).expect("native scalar");
        let current = f.session.document().clone();
        let generation = f.session.author_generation();
        let missing = f.update(&[], None);
        assert!(f
            .session
            .finish_native_audio_gesture(
                &gesture,
                missing,
                mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration
            )
            .is_err());
        let invalid = f.update(&[(1, f64::NAN)], Some(vec![8]));
        assert!(f
            .session
            .finish_native_audio_gesture(
                &gesture,
                invalid,
                mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration
            )
            .is_err());
        assert!(f.session.has_pending_native_authoring());
        assert!(f.session.snapshot().is_err());
        assert_eq!(f.session.document(), &current);
        assert_eq!(f.session.author_generation(), generation);
        let coherent = f.update(&[(1, 0.2)], Some(vec![8, 2, 255]));
        f.session
            .finish_native_audio_gesture(
                &gesture,
                coherent,
                mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration,
            )
            .expect("terminal native state");
        assert!(!f.session.has_pending_native_authoring());
        assert_eq!(
            f.session.history().diagnostics().undo_entries,
            history.undo_entries + 1
        );
        assert_eq!(
            f.processor().parameters[&f.parameter_ids[0]]
                .automation
                .as_ref()
                .expect("bound host curve")
                .default_value,
            0.8
        );
        assert_eq!(
            f.processor().parameters[&f.parameter_ids[1]]
                .automation
                .as_ref()
                .expect("bound host curve")
                .default_value,
            0.2
        );
        assert_eq!(
            f.processor().opaque_state.as_deref().map(Vec::as_slice),
            Some(&[8, 2, 255][..])
        );
        let complete = f.session.snapshot().expect("terminal state can persist").document;
        f.session.undo().expect("native Undo").expect("one coherent group");
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("Undo")
            .author_state_eq_ignoring_revision(&original));
        assert_eq!(
            f.session.history().diagnostics().undo_entries,
            history.undo_entries
        );
        f.session.redo().expect("native Redo").expect("one coherent group");
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("Redo")
            .author_state_eq_ignoring_revision(
                complete.sequences.sequence(f.sequence_id).expect("complete Sequence")
            ));
    }

    #[test]
    fn native_audio_author_old_handle_cannot_cancel_reused_interactive_identity() {
        let mut f = Fixture::new(false);
        let gesture = f.begin();
        f.session.cancel_native_audio_gesture(&gesture).expect("empty native cancel");
        f.session
            .begin_sequence_gesture(gesture.gesture_id(), f.sequence_id)
            .expect("new interactive kind");
        f.session
            .edit_sequence_in_gesture(
                gesture.gesture_id(),
                f.sequence_id,
                "user drag",
                |sequence| {
                    sequence.name = "Current interactive gesture".to_owned();
                    Ok(())
                },
            )
            .expect("actual interactive edit");
        let current = f.session.document().clone();
        assert!(f.session.cancel_native_audio_gesture(&gesture).is_err());
        assert_eq!(f.session.document(), &current);
        assert!(f.session.sequence_gesture_is_current(gesture.gesture_id(), f.sequence_id));
        f.session
            .cancel_sequence_gesture(gesture.gesture_id())
            .expect("current interactive cancel");
    }

    fn native_exact_seed_keys(
        f: &mut Fixture,
        index: usize,
        keys: Vec<mondrian_core::ExactAutomationKeyframe>,
    ) {
        let owner = f.owner;
        let processor_id = f.processor_id;
        let parameter_id = f.parameter_ids[index].clone();
        f.session
            .edit_sequence(f.sequence_id, "Prepare exact author curve", |sequence| {
                for keyframe in keys {
                    mondrian_timeline::apply_audio_automation_edit(
                        sequence,
                        &mondrian_timeline::AudioAutomationEditRequest {
                            target: mondrian_timeline::AudioAutomationTarget::ProcessorParameter {
                                rack: owner.rack_address(),
                                processor_id,
                                parameter_id: parameter_id.clone(),
                            },
                            edit: mondrian_timeline::AudioAutomationEdit::UpsertKeyframe {
                                keyframe,
                            },
                        },
                    )
                    .map_err(native_error)?;
                }
                Ok(())
            })
            .expect("canonical seed curve");
    }

    fn native_exact_position(f: &Fixture, time: TimelineTime) -> mondrian_core::DomainTime {
        let target = mondrian_timeline::AudioAutomationTarget::ProcessorParameter {
            rack: f.owner.rack_address(),
            processor_id: f.processor_id,
            parameter_id: f.parameter_ids[0].clone(),
        };
        let inspection = mondrian_timeline::inspect_audio_automation(
            f.session.sequence(f.sequence_id).expect("Sequence"),
            &target,
        )
        .expect("canonical owner domain");
        mondrian_core::DomainTime::new(inspection.time_domain(), time)
    }

    fn native_exact_update(
        f: &Fixture,
        values: &[(usize, f64)],
        position: mondrian_core::DomainTime,
        state: Option<Vec<u8>>,
    ) -> NativeAudioAuthorUpdate {
        let mut update = f.update(values, state);
        for operation in &mut update.parameters {
            let mondrian_timeline::NativeAudioParameterOperation::Value { write } = operation
            else {
                panic!("value fixture")
            };
            write.mode = mondrian_timeline::NativeAudioParameterWriteMode::AtOwnerTime { position };
        }
        update
    }

    #[test]
    fn native_audio_author_exact_key_preserves_manual_geometry_and_one_group_undo() {
        let mut f = Fixture::new(false);
        let time = TimelineTime::new(1001, 30000).expect("exact NTSC author time");
        let mut middle = mondrian_core::ExactAutomationKeyframe::linear(time, 0.5);
        middle.interpolation_to_next = mondrian_core::AutomationSegmentInterpolation::Bezier;
        middle.in_handle = Some(mondrian_core::ExactBezierHandle {
            time_offset: TimelineTime::new(-1, 60000).expect("incoming exact handle"),
            value_offset: -0.025,
        });
        middle.out_handle = Some(mondrian_core::ExactBezierHandle {
            time_offset: TimelineTime::new(1, 60000).expect("outgoing exact handle"),
            value_offset: 0.025,
        });
        native_exact_seed_keys(
            &mut f,
            0,
            vec![
                mondrian_core::ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.25),
                middle.clone(),
                mondrian_core::ExactAutomationKeyframe::linear(
                    TimelineTime::new(1, 10).expect("later"),
                    0.75,
                ),
            ],
        );
        let original = f.session.sequence(f.sequence_id).expect("origin").clone();
        let initial = f.processor().parameters[&f.parameter_ids[0]]
            .automation
            .as_ref()
            .expect("bound curve")
            .clone();
        let history = f.session.history().diagnostics();
        let position = native_exact_position(&f, time);
        let gesture = f.begin();
        let first = native_exact_update(&f, &[(0, 0.6)], position, None);
        f.session
            .update_native_audio_gesture(&gesture, first)
            .expect("exact keyed native edit");
        assert!(
            f.session.snapshot().is_err(),
            "partial curve and old seed cannot save"
        );
        let mut second = f.update(&[(1, 0.4)], None);
        let mondrian_timeline::NativeAudioParameterOperation::Value { write } =
            &mut second.parameters[0]
        else {
            panic!("value fixture")
        };
        write.mode = mondrian_timeline::NativeAudioParameterWriteMode::Unkeyed;
        f.session
            .update_native_audio_gesture(&gesture, second)
            .expect("same multi-parameter author group");
        let mut expected = initial.clone();
        expected.keyframes[1].value = 0.6;
        assert_eq!(
            f.processor().parameters[&f.parameter_ids[0]].automation.as_ref(),
            Some(&expected),
            "identity, exact time, handles, tangent mode and neighbors remain authored"
        );
        let completion = f.update(&[], Some(vec![6, 4, 255]));
        f.session
            .finish_native_audio_gesture(
                &gesture,
                completion,
                mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration,
            )
            .expect("coherent terminal seed");
        assert_eq!(
            f.session.history().diagnostics().undo_entries,
            history.undo_entries + 1
        );
        let complete = f.session.sequence(f.sequence_id).expect("complete").clone();
        f.session.undo().expect("Undo").expect("single exact group");
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("Undo origin")
            .author_state_eq_ignoring_revision(&original));
        f.session.redo().expect("Redo").expect("single exact group");
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("Redo result")
            .author_state_eq_ignoring_revision(&complete));
    }

    #[test]
    fn native_audio_author_exact_insert_uses_definition_hold_and_distinct_rational_times() {
        let mut f = Fixture::new(false);
        let NativeAudioAuthorOwner::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id },
            ..
        } = f.owner
        else {
            panic!("Track owner")
        };
        let id = f.parameter_ids[0].clone();
        f.session
            .edit_sequence(f.sequence_id, "Hold-only native schema", |sequence| {
                sequence
                    .audio_program
                    .track_channels
                    .get_mut(&track_id)
                    .expect("strip")
                    .strip
                    .pre_fader
                    .processors[0]
                    .parameters
                    .get_mut(&id)
                    .expect("parameter")
                    .schema
                    .allowed_interpolations = vec![mondrian_core::ParameterInterpolation::Hold];
                Ok(())
            })
            .expect("valid definition preference");
        let gesture = f.begin();
        let a = TimelineTime::new(1, 30000).expect("first exact time");
        let b = TimelineTime::new(2, 30000).expect("second exact time");
        for (time, value) in [(a, 0.4), (b, 0.7)] {
            let update =
                native_exact_update(&f, &[(0, value)], native_exact_position(&f, time), None);
            f.session
                .update_native_audio_gesture(&gesture, update)
                .expect("definition-admitted exact key");
        }
        let curve = f.processor().parameters[&id].automation.as_ref().expect("bound curve");
        assert_eq!(
            curve.default_value.to_bits(),
            0.25_f64.to_bits(),
            "automation insertion does not replace static fallback"
        );
        assert_eq!(
            curve.keyframes.len(),
            2,
            "distinct rational times cannot collapse to a display frame"
        );
        assert_eq!((curve.keyframes[0].time, curve.keyframes[1].time), (a, b));
        assert_ne!(curve.keyframes[0].id, curve.keyframes[1].id);
        assert!(curve
            .keyframes
            .iter()
            .all(|key| key.interpolation_to_next
                == mondrian_core::AutomationSegmentInterpolation::Hold));
        let completion = f.update(&[], Some(vec![4, 7]));
        f.session
            .finish_native_audio_gesture(
                &gesture,
                completion,
                mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration,
            )
            .expect("closed exact author state");
    }

    #[test]
    fn native_audio_author_exact_wrong_owner_domain_refuses_complete_batch() {
        for scope in [false, true] {
            let mut f = Fixture::new(scope);
            let original = f.session.sequence(f.sequence_id).expect("origin").clone();
            let generation = f.session.author_generation();
            let history = f.session.history().diagnostics();
            let correct = native_exact_position(&f, TimelineTime::new(1001, 30000).expect("time"));
            let wrong = mondrian_core::DomainTime::new(
                if scope {
                    mondrian_core::AuthoringTimeDomain::Sequence(f.sequence_id)
                } else {
                    mondrian_core::AuthoringTimeDomain::AudioProcessingScope(
                        AudioProcessingScopeId::new(),
                    )
                },
                correct.time,
            );
            let gesture = f.begin();
            let mut update =
                native_exact_update(&f, &[(0, 0.4), (1, 0.6)], correct, Some(vec![4, 6]));
            let mondrian_timeline::NativeAudioParameterOperation::Value { write } =
                &mut update.parameters[1]
            else {
                panic!("value fixture")
            };
            write.mode =
                mondrian_timeline::NativeAudioParameterWriteMode::AtOwnerTime { position: wrong };
            assert!(
                f.session.update_native_audio_gesture(&gesture, update).is_err(),
                "wrong exact author domain cannot publish first parameter or opaque bytes"
            );
            assert_eq!(f.session.author_generation(), generation);
            assert_eq!(f.session.history().diagnostics(), history);
            assert!(f
                .session
                .sequence(f.sequence_id)
                .expect("refused")
                .author_state_eq_ignoring_revision(&original));
            f.session.cancel_native_audio_gesture(&gesture).expect("empty cancelled group");
        }
    }

    #[test]
    fn native_audio_author_exact_nonanimatable_static_and_keyed_policies_remain_distinct() {
        let mut f = Fixture::new(false);
        let NativeAudioAuthorOwner::ChannelStrip {
            owner: AudioChannelStripOwner::Track { track_id },
            ..
        } = f.owner
        else {
            panic!("Track owner")
        };
        let id = f.parameter_ids[0].clone();
        f.session
            .edit_sequence(
                f.sequence_id,
                "Non-automatable native parameter",
                |sequence| {
                    sequence
                        .audio_program
                        .track_channels
                        .get_mut(&track_id)
                        .expect("strip")
                        .strip
                        .pre_fader
                        .processors[0]
                        .parameters
                        .get_mut(&id)
                        .expect("parameter")
                        .schema
                        .is_animatable = false;
                    Ok(())
                },
            )
            .expect("valid non-automatable schema");
        let original = f.session.sequence(f.sequence_id).expect("origin").clone();
        let gesture = f.begin();
        let wrong = native_exact_update(
            &f,
            &[(0, 0.6)],
            mondrian_core::DomainTime::new(
                mondrian_core::AuthoringTimeDomain::Sequence(f.sequence_id),
                TimelineTime::ZERO,
            ),
            Some(vec![6]),
        );
        assert!(f.session.update_native_audio_gesture(&gesture, wrong).is_err());
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("refused")
            .author_state_eq_ignoring_revision(&original));
        let static_write = f.update(&[(0, 0.6)], None);
        f.session
            .update_native_audio_gesture(&gesture, static_write)
            .expect("ordinary admitted static parameter");
        assert!(f.processor().parameters[&id]
            .automation
            .as_ref()
            .expect("bound host curve")
            .keyframes
            .is_empty());
        let completion = f.update(&[], Some(vec![6]));
        f.session
            .finish_native_audio_gesture(
                &gesture,
                completion,
                mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration,
            )
            .expect("static terminal state");
        assert_eq!(
            f.processor().parameters[&id]
                .automation
                .as_ref()
                .expect("bound host curve")
                .default_value
                .to_bits(),
            0.6_f64.to_bits()
        );
    }

    #[test]
    fn native_audio_author_exact_cancel_restores_curves_seed_and_redo_branch() {
        let mut f = Fixture::new(true);
        native_exact_seed_keys(
            &mut f,
            0,
            vec![mondrian_core::ExactAutomationKeyframe::linear(
                TimelineTime::new(1, 3).expect("key"),
                0.25,
            )],
        );
        f.session
            .edit_sequence(f.sequence_id, "Independent redo branch", |sequence| {
                sequence.name = "Branch".to_owned();
                Ok(())
            })
            .expect("branch");
        f.session.undo().expect("retain Redo").expect("branch edit");
        let original = f.session.sequence(f.sequence_id).expect("origin").clone();
        let history = f.session.history().diagnostics();
        let position = native_exact_position(&f, TimelineTime::new(1, 3).expect("Scope time"));
        let gesture = f.begin();
        let update = native_exact_update(&f, &[(0, 0.8), (1, 0.2)], position, None);
        f.session
            .update_native_audio_gesture(&gesture, update)
            .expect("shared Scope exact live group");
        f.session
            .cancel_native_audio_gesture(&gesture)
            .expect("cancel exact author group")
            .expect("restored");
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("cancelled")
            .author_state_eq_ignoring_revision(&original));
        let after = f.session.history().diagnostics();
        assert_eq!(
            (after.undo_entries, after.redo_entries),
            (history.undo_entries, history.redo_entries)
        );
        f.session.redo().expect("original Redo").expect("branch still retained");
        assert_eq!(
            f.session.sequence(f.sequence_id).expect("branch").name,
            "Branch"
        );
    }

    #[test]
    fn native_audio_author_exact_save_reopen_retains_keys_and_coherent_seed() {
        let mut f = Fixture::new(true);
        let gesture = f.begin();
        let position = native_exact_position(
            &f,
            TimelineTime::new(1001, 30000).expect("exact Scope time"),
        );
        let update = native_exact_update(
            &f,
            &[(0, 0.12345678901234568), (1, 0.875)],
            position,
            Some(vec![0, 255, 13, 10]),
        );
        f.session
            .finish_native_audio_gesture(
                &gesture,
                update,
                mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration,
            )
            .expect("complete exact state");
        let snapshot = f.session.snapshot().expect("coherent author snapshot");
        let archive = f.root.path().join("exact-native.mdp");
        let sqlite = snapshot
            .asset_library
            .snapshot_database(
                snapshot.asset_library_revision,
                &f.root.path().join("exact-anchor.db"),
            )
            .expect("consistent SQLite");
        let mut reader = sqlite.try_clone_reader().expect("retained SQLite object");
        save_project_archive_from_open_library_with_publication(
            &snapshot.document,
            &mut reader,
            &archive,
            ProjectArchivePublication::CreateNew,
        )
        .expect("durable actual mdp");
        let reopened = f.root.path().join("exact-reopened");
        let loaded =
            load_project_archive(&archive, &reopened.join("library")).expect("actual archive");
        let current = &audio_processor_rack(
            loaded.document.sequences.sequence(f.sequence_id).expect("Sequence"),
            &f.owner.rack_address(),
        )
        .expect("Scope Rack")
        .processors[0];
        assert_eq!(
            current,
            f.processor(),
            "real archive preserves exact key IDs/times/value bits/schema and opaque state"
        );
        assert_eq!(
            current.parameters[&f.parameter_ids[0]]
                .automation
                .as_ref()
                .expect("bound host curve")
                .keyframes[0]
                .time,
            position.time
        );
        assert_eq!(
            current.parameters[&f.parameter_ids[0]]
                .automation
                .as_ref()
                .expect("bound host curve")
                .keyframes[0]
                .value
                .to_bits(),
            0.12345678901234568_f64.to_bits()
        );
    }

    fn clear_reference(
        parameter_id: ParameterId,
    ) -> mondrian_timeline::NativeAudioParameterOperation {
        mondrian_timeline::NativeAudioParameterOperation::ClearAutomation { parameter_id }
    }

    #[test]
    fn native_parameter_references_clear_value_source_order_and_atomic_failure() {
        for clear_last in [false, true] {
            let mut f = Fixture::new(false);
            let original = f.processor().clone();
            let gesture = f.begin();
            let value = f.update(&[(0, 0.9)], None).parameters.remove(0);
            let clear = clear_reference(f.parameter_ids[0].clone());
            let parameters = if clear_last {
                vec![value, clear]
            } else {
                vec![clear, value]
            };
            f.session
                .update_native_audio_gesture(
                    &gesture,
                    NativeAudioAuthorUpdate { parameters, opaque_state: None },
                )
                .expect("ordered native operation batch");
            let current = &f.processor().parameters[&f.parameter_ids[0]];
            assert_eq!(
                current.schema,
                original.parameters[&f.parameter_ids[0]].schema
            );
            assert_eq!(current.ui, original.parameters[&f.parameter_ids[0]].ui);
            assert_eq!(
                current.automation.as_ref().map(|curve| curve.default_value.to_bits()),
                (!clear_last).then_some(0.9_f64.to_bits())
            );
            assert_eq!(
                f.processor().parameters[&f.parameter_ids[1]],
                original.parameters[&f.parameter_ids[1]]
            );
            assert!(
                f.session.snapshot().is_err(),
                "unsettled native state is not a saved seed"
            );
        }
        let mut f = Fixture::new(false);
        let original = f.processor().clone();
        let generation = f.session.author_generation();
        let gesture = f.begin();
        let invalid = f.update(&[(1, f64::NAN)], Some(vec![9])).parameters.remove(0);
        assert!(f
            .session
            .update_native_audio_gesture(
                &gesture,
                NativeAudioAuthorUpdate {
                    parameters: vec![clear_reference(f.parameter_ids[0].clone()), invalid],
                    opaque_state: Some(AuthoringList::from(vec![9])),
                }
            )
            .is_err());
        assert_eq!(
            *f.processor(),
            original,
            "invalid suffix cannot publish Clear or opaque bytes"
        );
        assert_eq!(f.session.author_generation(), generation);
    }

    #[test]
    fn native_parameter_references_clear_undo_redo_and_real_archive_reopen() {
        let mut f = Fixture::new(false);
        let original = f.session.sequence(f.sequence_id).expect("origin").clone();
        let gesture = f.begin();
        f.session
            .update_native_audio_gesture(
                &gesture,
                NativeAudioAuthorUpdate {
                    parameters: vec![clear_reference(f.parameter_ids[0].clone())],
                    opaque_state: None,
                },
            )
            .expect("remove host reference");
        f.session
            .finish_native_audio_gesture(
                &gesture,
                f.update(&[], Some(vec![9, 7, 0])),
                mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration,
            )
            .expect("settle authored opaque state");
        let cleared = f.session.sequence(f.sequence_id).expect("cleared").clone();
        assert!(f.processor().parameters[&f.parameter_ids[0]].automation.is_none());
        f.session.undo().expect("Undo").expect("one native transaction");
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("Undo origin")
            .author_state_eq_ignoring_revision(&original));
        f.session.redo().expect("Redo").expect("one native transaction");
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("Redo cleared")
            .author_state_eq_ignoring_revision(&cleared));
        let snapshot = f.session.snapshot().expect("coherent saved author snapshot");
        let archive = f.root.path().join("cleared-native-reference.mdp");
        let sqlite = snapshot
            .asset_library
            .snapshot_database(
                snapshot.asset_library_revision,
                &f.root.path().join("clear-snapshot.db"),
            )
            .expect("consistent database");
        let mut reader = sqlite.try_clone_reader().expect("snapshot reader");
        save_project_archive_from_open_library_with_publication(
            &snapshot.document,
            &mut reader,
            &archive,
            ProjectArchivePublication::CreateNew,
        )
        .expect("real durable archive");
        let reopened_root = f.root.path().join("clear-reopened");
        let loaded =
            load_project_archive(&archive, &reopened_root.join("library")).expect("real reopen");
        let library = AssetLibrary::open(reopened_root.join("library")).expect("reopened Library");
        let reopened =
            AuthoringSession::open_saved(loaded.document, archive, reopened_root, library)
                .expect("fresh Session");
        let current = &audio_processor_rack(
            reopened.sequence(f.sequence_id).expect("Sequence"),
            &f.owner.rack_address(),
        )
        .expect("same Rack")
        .processors[0];
        assert_eq!(current, f.processor());
        assert!(current.parameters[&f.parameter_ids[0]].automation.is_none());
    }

    #[test]
    fn native_parameter_references_unknown_and_repeated_clear_never_create_parameters() {
        let mut f = Fixture::new(false);
        let gesture = f.begin();
        let id = f.parameter_ids[0].clone();
        let absent = ParameterId::new("vst3.param.4294967294").expect("stable removed ID");
        f.session
            .update_native_audio_gesture(
                &gesture,
                NativeAudioAuthorUpdate {
                    parameters: vec![
                        clear_reference(id.clone()),
                        clear_reference(absent.clone()),
                        clear_reference(id.clone()),
                    ],
                    opaque_state: None,
                },
            )
            .expect("clear existing and removed references");
        assert_eq!(f.processor().parameters.len(), 2);
        assert!(!f.processor().parameters.contains_key(&absent));
        assert!(f.processor().parameters[&id].automation.is_none());
        let generation = f.session.author_generation();
        f.session
            .update_native_audio_gesture(
                &gesture,
                NativeAudioAuthorUpdate {
                    parameters: vec![clear_reference(id), clear_reference(absent)],
                    opaque_state: None,
                },
            )
            .expect("already clear no-op");
        assert_eq!(f.session.author_generation(), generation);
    }

    #[test]
    fn native_parameter_references_explicit_null_and_mandatory_current_field() {
        let mut f = Fixture::new(false);
        let gesture = f.begin();
        f.session
            .update_native_audio_gesture(
                &gesture,
                NativeAudioAuthorUpdate {
                    parameters: vec![clear_reference(f.parameter_ids[0].clone())],
                    opaque_state: None,
                },
            )
            .expect("clear");
        let parameter = &f.processor().parameters[&f.parameter_ids[0]];
        let mut value = serde_json::to_value(parameter).expect("current canonical JSON");
        assert!(value["automation"].is_null());
        assert_eq!(
            serde_json::from_value::<AudioProcessorParameter>(value.clone())
                .expect("explicit null"),
            *parameter
        );
        value.as_object_mut().expect("object").remove("automation");
        assert!(
            serde_json::from_value::<AudioProcessorParameter>(value).is_err(),
            "missing field is not an old-format compatibility branch"
        );
    }

    #[test]
    fn native_audio_author_parameter_completion_undo_save_reopen_has_no_opaque_seed() {
        use mondrian_core::NativeAudioAuthorCompletion as Completion;
        let mut f = Fixture::with_initial_configuration(false, None);
        let original = f.session.sequence(f.sequence_id).expect("original author").clone();
        let before = f.session.history().diagnostics().undo_entries;
        let gesture = f.begin();
        f.session
            .update_native_audio_gesture(&gesture, f.update(&[(0, 0.8125), (1, 0.125)], None))
            .expect("canonical native parameter writes");
        f.session
            .finish_native_audio_gesture(&gesture, f.update(&[], None), Completion::ParametersOnly)
            .expect("explicit actual parameter-only closure");
        assert!(
            f.processor().opaque_state.is_none(),
            "no fake empty configuration"
        );
        assert_eq!(f.session.history().diagnostics().undo_entries, before + 1);
        let expected = f.session.sequence(f.sequence_id).expect("accepted author").clone();
        f.session.undo().expect("sole Undo").expect("one native gesture");
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("Undo")
            .author_state_eq_ignoring_revision(&original));
        f.session.redo().expect("sole Redo").expect("one native gesture");
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("Redo")
            .author_state_eq_ignoring_revision(&expected));
        let snapshot = f.session.snapshot().expect("closed native group admits persistence");
        let archive = f.root.path().join("stateless-native.mdp");
        let sqlite = snapshot
            .asset_library
            .snapshot_database(
                snapshot.asset_library_revision,
                &f.root.path().join("stateless-anchor.db"),
            )
            .expect("consistent SQLite");
        let mut reader = sqlite.try_clone_reader().expect("owned SQLite reader");
        save_project_archive_from_open_library_with_publication(
            &snapshot.document,
            &mut reader,
            &archive,
            ProjectArchivePublication::CreateNew,
        )
        .expect("durable mdp publication");
        let reopened_root = f.root.path().join("stateless-reopened");
        let loaded =
            load_project_archive(&archive, &reopened_root.join("library")).expect("real mdp read");
        let library = AssetLibrary::open(reopened_root.join("library")).expect("reopened Library");
        let reopened =
            AuthoringSession::open_saved(loaded.document, archive, reopened_root, library)
                .expect("saved Session");
        let actual = &audio_processor_rack(
            reopened.sequence(f.sequence_id).expect("saved Sequence"),
            &f.owner.rack_address(),
        )
        .expect("same authored Rack")
        .processors[0];
        assert!(actual.opaque_state.is_none());
        for (index, expected) in [(0, 0.8125_f64), (1, 0.125_f64)] {
            assert_eq!(
                actual.parameters[&f.parameter_ids[index]]
                    .automation
                    .as_ref()
                    .expect("real saved canonical reference")
                    .default_value
                    .to_bits(),
                expected.to_bits()
            );
        }
        assert_eq!(
            actual,
            f.processor(),
            "same IDs, schema and values persist through archive"
        );
    }

    #[test]
    fn native_audio_author_parameter_completion_refuses_stale_or_supplied_opaque_seed() {
        use mondrian_core::NativeAudioAuthorCompletion as Completion;
        for old_state in [None, Some(vec![1, 2])] {
            let mut f = Fixture::with_initial_configuration(false, old_state.clone());
            let gesture = f.begin();
            f.session
                .update_native_audio_gesture(&gesture, f.update(&[(0, 0.8125)], None))
                .expect("partial scalar");
            let generation = f.session.author_generation();
            let current = f.session.sequence(f.sequence_id).expect("pending").clone();
            let completion = f.update(
                &[],
                if old_state.is_none() {
                    Some(vec![9])
                } else {
                    None
                },
            );
            assert!(f
                .session
                .finish_native_audio_gesture(&gesture, completion, Completion::ParametersOnly)
                .is_err());
            assert_eq!(f.session.author_generation(), generation);
            assert!(f
                .session
                .sequence(f.sequence_id)
                .expect("same pending author")
                .author_state_eq_ignoring_revision(&current));
            assert!(
                f.session.snapshot().is_err(),
                "invalid finality does not clear NativeStatePending"
            );
            f.session
                .cancel_native_audio_gesture(&gesture)
                .expect("real Session restoration");
        }
    }

    #[test]
    fn native_audio_author_nonterminal_completion_cannot_seal_parameter_history() {
        let mut f = Fixture::with_initial_configuration(false, None);
        let gesture = f.begin();
        f.session
            .update_native_audio_gesture(&gesture, f.update(&[(0, 0.625)], None))
            .expect("partial scalar");
        let generation = f.session.author_generation();
        assert!(f
            .session
            .finish_native_audio_gesture(
                &gesture,
                f.update(&[], None),
                mondrian_core::NativeAudioAuthorCompletion::Nonterminal
            )
            .is_err());
        assert_eq!(f.session.author_generation(), generation);
        assert!(f.session.snapshot().is_err());
        f.session
            .cancel_native_audio_gesture(&gesture)
            .expect("cancel same history group");
    }

    #[test]
    fn native_stateless_reference_clear_rescan_history_archive_reopen() {
        let mut f = Fixture::with_initial_configuration(false, None);
        let original = f.session.sequence(f.sequence_id).expect("original author").clone();
        let before = f.session.history().diagnostics().undo_entries;
        let gesture = f.begin();
        let rescan_unkeyed = f.update(&[(1, 0.625)], None).parameters.remove(0);
        f.session
            .update_native_audio_gesture(
                &gesture,
                NativeAudioAuthorUpdate {
                    parameters: vec![clear_reference(f.parameter_ids[0].clone()), rescan_unkeyed],
                    opaque_state: None,
                },
            )
            .expect("ordered host-reference transaction, no private state requirement");
        f.session
            .finish_native_audio_gesture(
                &gesture,
                f.update(&[], None),
                mondrian_core::NativeAudioAuthorCompletion::ParametersOnly,
            )
            .expect("actual parameter-only closure");
        assert!(f.processor().parameters[&f.parameter_ids[0]].automation.is_none());
        assert_eq!(
            f.processor().parameters[&f.parameter_ids[1]]
                .automation
                .as_ref()
                .expect("existing unkeyed reference refreshed")
                .default_value
                .to_bits(),
            0.625_f64.to_bits()
        );
        assert!(f.processor().opaque_state.is_none());
        assert_eq!(f.session.history().diagnostics().undo_entries, before + 1);
        let expected = f.session.sequence(f.sequence_id).expect("accepted author").clone();
        f.session.undo().expect("Undo").expect("sole reference transaction");
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("Undo")
            .author_state_eq_ignoring_revision(&original));
        f.session.redo().expect("Redo").expect("same transaction");
        assert!(f
            .session
            .sequence(f.sequence_id)
            .expect("Redo")
            .author_state_eq_ignoring_revision(&expected));
        let snapshot = f.session.snapshot().expect("no unsettled private state");
        let archive = f.root.path().join("stateless-clear-values.mdp");
        let sqlite = snapshot
            .asset_library
            .snapshot_database(
                snapshot.asset_library_revision,
                &f.root.path().join("stateless-clear-values.db"),
            )
            .expect("consistent SQLite");
        let mut reader = sqlite.try_clone_reader().expect("owned reader");
        save_project_archive_from_open_library_with_publication(
            &snapshot.document,
            &mut reader,
            &archive,
            ProjectArchivePublication::CreateNew,
        )
        .expect("actual mdp save");
        let reopened_root = f.root.path().join("stateless-clear-reopened");
        let loaded = load_project_archive(&archive, &reopened_root.join("library"))
            .expect("actual archive read");
        let library =
            AssetLibrary::open(reopened_root.join("library")).expect("actual reopened library");
        let reopened =
            AuthoringSession::open_saved(loaded.document, archive, reopened_root, library)
                .expect("saved Session");
        let actual = &audio_processor_rack(
            reopened.sequence(f.sequence_id).expect("same saved Sequence"),
            &f.owner.rack_address(),
        )
        .expect("same authored rack")
        .processors[0];
        assert_eq!(actual, f.processor());
        assert!(actual.parameters[&f.parameter_ids[0]].automation.is_none());
        assert!(actual.opaque_state.is_none());
    }

    // Canonical Session controls only: no native Source, Runtime frontier or Ack
    // is fabricated by these metadata fixtures.
    fn terminal_member(
        fixture: &Fixture,
        owner: NativeAudioAuthorOwner,
        processor_id: AudioProcessorInstanceId,
        values: &[(usize, f64)],
        state: Option<Vec<u8>>,
        completion: mondrian_core::NativeAudioAuthorCompletion,
    ) -> NativeAudioTerminalAuthorMember {
        NativeAudioTerminalAuthorMember {
            owner,
            processor_id,
            expected_definition: fixture.definition.clone(),
            update: fixture.update(values, state),
            completion,
        }
    }

    fn terminal_peer(fixture: &mut Fixture) -> (NativeAudioAuthorOwner, AudioProcessorInstanceId) {
        let mut peer = fixture.processor().clone();
        peer.id = AudioProcessorInstanceId::new();
        let id = peer.id;
        let output_id =
            fixture.session.sequence(fixture.sequence_id).unwrap().audio_program.outputs[0].id;
        let owner = NativeAudioAuthorOwner::ChannelStrip {
            owner: AudioChannelStripOwner::ProgramOutput { output_id },
            rack: AudioChannelStripRack::PreFader,
        };
        let request = AudioProcessorRackEditRequest {
            address: owner.rack_address(),
            edit: AudioProcessorRackEdit::Insert {
                processor: peer,
                placement: AudioProcessorRackPlacement::End,
            },
        };
        fixture
            .session
            .edit_sequence(fixture.sequence_id, "prepare canonical peer", |sequence| {
                apply_audio_processor_rack_edit(sequence, &request).map_err(native_error)
            })
            .expect("second canonical owner");
        (owner, id)
    }

    #[test]
    fn native_audio_terminal_group_prepares_and_publishes_members_once() {
        use mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration as Coherent;
        let mut f = Fixture::new(false);
        let (peer_owner, peer_id) = terminal_peer(&mut f);
        let a = terminal_member(
            &f,
            f.owner,
            f.processor_id,
            &[(0, 0.4)],
            Some(vec![3]),
            Coherent,
        );
        let b = terminal_member(
            &f,
            peer_owner,
            peer_id,
            &[(1, 0.6)],
            Some(vec![4]),
            Coherent,
        );
        let original = f.session.document().clone();
        let generation = f.session.author_generation();
        let history = f.session.history().diagnostics();
        let mut group =
            f.session.begin_native_audio_terminal_group(f.sequence_id, a.clone()).unwrap();
        assert_eq!(f.session.document(), &original);
        assert_eq!(f.session.history().diagnostics(), history);
        assert_eq!(f.session.author_generation(), generation);
        assert!(f.session.has_pending_native_authoring());
        let mut ticket =
            f.session.prepare_native_audio_terminal_group(&mut group, vec![a, b]).unwrap();
        assert_eq!(
            f.session.document(),
            &original,
            "private candidate has not published A or B"
        );
        let candidate = ticket.candidate_sequence().unwrap();
        for (owner, id, state) in [
            (f.owner, f.processor_id, &[3_u8][..]),
            (peer_owner, peer_id, &[4_u8][..]),
        ] {
            let processor =
                inspect_native_audio_authoring(candidate, &owner.rack_address(), id, &f.definition)
                    .unwrap();
            assert_eq!(processor.opaque_state.as_ref().unwrap().as_slice(), state);
        }
        let commit = f
            .session
            .commit_native_audio_terminal_group(&mut group, &mut ticket)
            .unwrap()
            .unwrap();
        assert_eq!(commit.generation.get(), generation.get() + 1);
        assert!(ticket.candidate_sequence().is_none());
        assert_eq!(
            f.session.history().diagnostics().undo_entries,
            history.undo_entries + 1
        );
        assert!(
            f.session.snapshot().is_err(),
            "accepted Document still waits for external Acks"
        );
        assert!(f.session.undo().is_err());
        assert!(f.session.commit_native_audio_terminal_group(&mut group, &mut ticket).is_err());
        let published = f.session.sequence(f.sequence_id).unwrap().clone();
        let published_generation = f.session.author_generation();
        // Product-side all-Ack proof is a precondition, not evidence supplied by this test.
        f.session.finish_native_audio_terminal_group(&mut group).unwrap();
        assert!(!f.session.has_pending_native_authoring());
        assert!(f.session.finish_native_audio_terminal_group(&mut group).is_err());
        assert_eq!(
            f.session.author_generation(),
            published_generation,
            "finish never rewrites member state"
        );
        assert_eq!(
            f.session.history().diagnostics().undo_entries,
            history.undo_entries + 1
        );
        f.session.undo().unwrap().unwrap();
        assert!(
            f.session.sequence(f.sequence_id).unwrap().author_state_eq_ignoring_revision(
                original.sequences.sequence(f.sequence_id).unwrap()
            )
        );
        f.session.redo().unwrap().unwrap();
        assert!(f
            .session
            .sequence(f.sequence_id)
            .unwrap()
            .author_state_eq_ignoring_revision(&published));
    }

    #[test]
    fn native_audio_terminal_group_preserves_save_completion_during_preparation() {
        use mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration as Coherent;
        let mut f = Fixture::new(false);
        // Capture the save snapshot before the native barrier opens; its later
        // public completion remains legal while the group is pending.
        let saving = f.session.snapshot().unwrap();
        let original = f.session.sequence(f.sequence_id).unwrap().clone();
        let history = f.session.history().diagnostics();
        let member = terminal_member(
            &f,
            f.owner,
            f.processor_id,
            &[(0, 0.4)],
            Some(vec![3]),
            Coherent,
        );
        let mut group = f
            .session
            .begin_native_audio_terminal_group(f.sequence_id, member.clone())
            .unwrap();
        let mut ticket =
            f.session.prepare_native_audio_terminal_group(&mut group, vec![member]).unwrap();
        let saved_revision = saving.document.document_revision + 1;
        let mut persisted_meta = saving.document.meta.clone();
        persisted_meta.updated_at += chrono::Duration::seconds(1);
        let saved_updated_at = persisted_meta.updated_at;
        let saved_file = f.root.path().join("completed-save-as.mdp");
        f.session
            .mark_saved(
                saving.generation,
                saved_revision,
                saving.asset_library_revision,
                persisted_meta,
                Some(saved_file.clone()),
            )
            .unwrap();
        assert_eq!(f.session.author_generation(), saving.generation);
        assert_eq!(f.session.sequence(f.sequence_id).unwrap(), &original);
        assert_eq!(f.session.history().diagnostics(), history);
        assert_eq!(f.session.document().document_revision, saved_revision);
        assert_eq!(f.session.document().meta.updated_at, saved_updated_at);
        assert_eq!(f.session.project_file(), saved_file.as_path());
        assert_eq!(f.session.saved_generation(), Some(saving.generation));
        assert!(!f.session.is_dirty());
        assert!(f.session.has_pending_native_authoring());

        let commit = f
            .session
            .commit_native_audio_terminal_group(&mut group, &mut ticket)
            .unwrap()
            .unwrap();
        assert_eq!(commit.generation.get(), saving.generation.get() + 1);
        assert!(ticket.candidate_sequence().is_none());
        assert_eq!(f.session.document().document_revision, saved_revision);
        assert_eq!(f.session.document().meta.updated_at, saved_updated_at);
        assert_eq!(f.session.project_file(), saved_file.as_path());
        assert_eq!(f.session.saved_generation(), Some(saving.generation));
        assert!(f
            .session
            .manual_save_baseline_covers(saving.generation, saving.asset_library_revision));
        assert!(!f
            .session
            .manual_save_baseline_covers(commit.generation, saving.asset_library_revision));
        assert!(
            f.session.is_dirty(),
            "native publication is newer than the save"
        );
        assert_eq!(
            f.session.history().diagnostics().undo_entries,
            history.undo_entries + 1
        );
        assert_eq!(
            f.processor().parameters[&f.parameter_ids[0]]
                .automation
                .as_ref()
                .unwrap()
                .default_value
                .to_bits(),
            0.4_f64.to_bits()
        );
        assert_eq!(
            f.processor().opaque_state.as_ref().unwrap().as_slice(),
            &[3]
        );
        let published = f.session.sequence(f.sequence_id).unwrap().clone();
        // Actual native Acks remain the product caller's precondition.
        f.session.finish_native_audio_terminal_group(&mut group).unwrap();
        f.session
            .snapshot()
            .expect("certificate still proves the published author state");
        f.session.undo().unwrap().unwrap();
        assert!(f
            .session
            .sequence(f.sequence_id)
            .unwrap()
            .author_state_eq_ignoring_revision(&original));
        f.session.redo().unwrap().unwrap();
        assert!(f
            .session
            .sequence(f.sequence_id)
            .unwrap()
            .author_state_eq_ignoring_revision(&published));
        assert_eq!(
            f.session.history().diagnostics().undo_entries,
            history.undo_entries + 1
        );
        assert_eq!(f.session.document().document_revision, saved_revision);
        assert_eq!(f.session.document().meta.updated_at, saved_updated_at);
        assert_eq!(f.session.project_file(), saved_file.as_path());
        assert_eq!(f.session.saved_generation(), Some(saving.generation));
    }

    #[test]
    fn native_audio_terminal_group_rejects_invalid_member_sets_without_publication() {
        use mondrian_core::NativeAudioAuthorCompletion as Completion;
        let mut f = Fixture::new(false);
        let (peer_owner, peer_id) = terminal_peer(&mut f);
        let a = terminal_member(
            &f,
            f.owner,
            f.processor_id,
            &[(0, 0.4)],
            Some(vec![3]),
            Completion::CoherentConfiguration,
        );
        let b = terminal_member(
            &f,
            peer_owner,
            peer_id,
            &[(1, 0.6)],
            Some(vec![4]),
            Completion::CoherentConfiguration,
        );
        let mut bad_schema = b.clone();
        let mondrian_timeline::NativeAudioParameterOperation::Value { write } =
            &mut bad_schema.update.parameters[0]
        else {
            unreachable!()
        };
        write.schema.schema_version += 1;
        let mut nonterminal = b.clone();
        nonterminal.completion = Completion::Nonterminal;
        let mut foreign = b.clone();
        foreign.processor_id = AudioProcessorInstanceId::new();
        let original = f.session.document().clone();
        let generation = f.session.author_generation();
        let history = f.session.history().diagnostics();
        let mut group =
            f.session.begin_native_audio_terminal_group(f.sequence_id, a.clone()).unwrap();
        for members in [
            vec![],
            vec![a.clone(), a.clone()],
            vec![b.clone()],
            vec![a.clone(), bad_schema],
            vec![a.clone(), nonterminal],
            vec![a.clone(), foreign],
        ] {
            assert!(f.session.prepare_native_audio_terminal_group(&mut group, members).is_err());
            assert_eq!(f.session.document(), &original);
            assert_eq!(f.session.author_generation(), generation);
            assert_eq!(f.session.history().diagnostics(), history);
            assert!(f.session.has_pending_native_authoring());
        }
        let ticket = f.session.prepare_native_audio_terminal_group(&mut group, vec![a, b]).unwrap();
        assert!(
            ticket.candidate_sequence().is_some(),
            "invalid B did not publish or freeze A"
        );
        drop(ticket);
        drop(group);
        assert!(
            f.session.has_pending_native_authoring(),
            "Drop never invents native completion"
        );
        assert_eq!(f.session.document(), &original);
    }

    #[test]
    fn native_audio_terminal_group_ticket_preserves_foreign_and_before_current_refusals() {
        use mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration as Coherent;
        let mut f = Fixture::new(false);
        let a = terminal_member(
            &f,
            f.owner,
            f.processor_id,
            &[(0, 0.4)],
            Some(vec![3]),
            Coherent,
        );
        let original = f.session.document().clone();
        let mut other = AuthoringSession::new_unsaved(
            original.clone(),
            f.root.path().join("same-project-new-session.mdp"),
            f.root.path().to_owned(),
            f.session.asset_library().clone(),
        )
        .unwrap();
        assert_eq!(other.author_generation(), f.session.author_generation());
        assert_ne!(other.session_id(), f.session.session_id());
        let mut group =
            f.session.begin_native_audio_terminal_group(f.sequence_id, a.clone()).unwrap();
        let mut ticket =
            f.session.prepare_native_audio_terminal_group(&mut group, vec![a]).unwrap();
        let foreign_history = other.history().diagnostics();
        assert!(other.commit_native_audio_terminal_group(&mut group, &mut ticket).is_err());
        assert!(other.discard_native_audio_terminal_candidate(&mut group, &mut ticket).is_err());
        assert_eq!(other.document(), &original);
        assert_eq!(other.history().diagnostics(), foreign_history);
        assert!(!other.has_pending_native_authoring());
        assert!(
            ticket.candidate_sequence().is_some(),
            "foreign refusal retains the original ticket"
        );
        let generation = f.session.author_generation();
        let history = f.session.history().diagnostics();
        // Existing fixture-only seam isolates the required before/current check
        // even when an out-of-contract edit failed to advance the generation.
        f.session
            .document_mut_for_test_fixture()
            .sequences
            .sequence_mut(f.sequence_id)
            .unwrap()
            .name = "changed behind ticket".into();
        let changed = f.session.document().clone();
        assert!(f.session.commit_native_audio_terminal_group(&mut group, &mut ticket).is_err());
        assert!(f
            .session
            .discard_native_audio_terminal_candidate(&mut group, &mut ticket)
            .is_err());
        assert_eq!(f.session.document(), &changed);
        assert_eq!(f.session.author_generation(), generation);
        assert_eq!(f.session.history().diagnostics(), history);
        assert!(f.session.has_pending_native_authoring());
        assert!(ticket.candidate_sequence().is_some());
    }

    #[test]
    fn native_audio_terminal_group_discard_and_late_tickets_keep_new_candidate_frozen() {
        use mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration as Coherent;
        let mut f = Fixture::new(false);
        let a = terminal_member(
            &f,
            f.owner,
            f.processor_id,
            &[(0, 0.4)],
            Some(vec![3]),
            Coherent,
        );
        let original = f.session.document().clone();
        let generation = f.session.author_generation();
        let mut old_group =
            f.session.begin_native_audio_terminal_group(f.sequence_id, a.clone()).unwrap();
        let mut old = f
            .session
            .prepare_native_audio_terminal_group(&mut old_group, vec![a.clone()])
            .unwrap();
        f.session
            .discard_native_audio_terminal_candidate(&mut old_group, &mut old)
            .unwrap();
        assert!(old.candidate_sequence().is_none());
        assert!(f.session.has_pending_native_authoring());
        let mut later = f
            .session
            .prepare_native_audio_terminal_group(&mut old_group, vec![a.clone()])
            .unwrap();
        assert!(f
            .session
            .discard_native_audio_terminal_candidate(&mut old_group, &mut old)
            .is_err());
        assert!(
            later.candidate_sequence().is_some(),
            "consumed ticket cannot thaw its successor"
        );
        // Explicit original Session cancellation makes the still-owned prepared
        // ticket stale without a publication. A new original group has a new ID.
        f.session.cancel_sequence_gesture(old_group.gesture_id()).unwrap();
        let mut new_group =
            f.session.begin_native_audio_terminal_group(f.sequence_id, a.clone()).unwrap();
        let mut current =
            f.session.prepare_native_audio_terminal_group(&mut new_group, vec![a]).unwrap();
        assert!(f
            .session
            .commit_native_audio_terminal_group(&mut old_group, &mut later)
            .is_err());
        assert!(f
            .session
            .commit_native_audio_terminal_group(&mut new_group, &mut later)
            .is_err());
        assert!(f
            .session
            .discard_native_audio_terminal_candidate(&mut new_group, &mut later)
            .is_err());
        assert!(later.candidate_sequence().is_some());
        assert!(current.candidate_sequence().is_some());
        assert_eq!(f.session.document(), &original);
        assert_eq!(f.session.author_generation(), generation);
        assert!(f.session.has_pending_native_authoring());
        f.session
            .discard_native_audio_terminal_candidate(&mut new_group, &mut current)
            .unwrap();
        assert!(current.candidate_sequence().is_none());
        assert!(
            f.session.finish_native_audio_terminal_group(&mut new_group).is_err(),
            "discard is not finish"
        );
        // Generation staleness is independent of equal-generation group IDs.
        f.session.cancel_sequence_gesture(new_group.gesture_id()).unwrap();
        f.session
            .edit_sequence(f.sequence_id, "new author generation", |sequence| {
                sequence.name = "newer canonical author".into();
                Ok(())
            })
            .unwrap();
        let newer = f.session.document().clone();
        assert!(f
            .session
            .commit_native_audio_terminal_group(&mut old_group, &mut later)
            .is_err());
        assert_eq!(f.session.document(), &newer);
        assert!(later.candidate_sequence().is_some());
    }

    #[test]
    fn native_audio_terminal_group_nochange_keeps_barrier_until_one_finish() {
        use mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration as Coherent;
        let mut f = Fixture::new(false);
        let a = terminal_member(&f, f.owner, f.processor_id, &[], Some(vec![1, 2]), Coherent);
        let original = f.session.document().clone();
        let generation = f.session.author_generation();
        let history = f.session.history().diagnostics();
        let mut group =
            f.session.begin_native_audio_terminal_group(f.sequence_id, a.clone()).unwrap();
        let mut ticket =
            f.session.prepare_native_audio_terminal_group(&mut group, vec![a]).unwrap();
        assert!(ticket
            .candidate_sequence()
            .unwrap()
            .author_state_eq_ignoring_revision(f.session.sequence(f.sequence_id).unwrap()));
        assert!(f
            .session
            .commit_native_audio_terminal_group(&mut group, &mut ticket)
            .unwrap()
            .is_none());
        assert!(ticket.candidate_sequence().is_none());
        assert_eq!(f.session.document(), &original);
        assert_eq!(f.session.author_generation(), generation);
        assert_eq!(f.session.history().diagnostics(), history);
        assert!(f.session.snapshot().is_err());
        assert!(f.session.undo().is_err());
        assert!(f.session.has_pending_native_authoring());
        f.session.finish_native_audio_terminal_group(&mut group).unwrap();
        assert!(!f.session.has_pending_native_authoring());
        assert_eq!(f.session.document(), &original);
        assert_eq!(f.session.history().diagnostics(), history);
        assert!(f.session.snapshot().is_ok());

        let mut stale = Fixture::new(false);
        let same = terminal_member(
            &stale,
            stale.owner,
            stale.processor_id,
            &[],
            Some(vec![1, 2]),
            Coherent,
        );
        let mut group = stale
            .session
            .begin_native_audio_terminal_group(stale.sequence_id, same.clone())
            .unwrap();
        let mut ticket = stale
            .session
            .prepare_native_audio_terminal_group(&mut group, vec![same])
            .unwrap();
        // The NoChange branch has no generic PreparedSequenceEdit; its own
        // before/current proof must still survive until the asynchronous commit.
        stale
            .session
            .document_mut_for_test_fixture()
            .sequences
            .sequence_mut(stale.sequence_id)
            .unwrap()
            .name = "later before/current".into();
        let changed = stale.session.document().clone();
        assert!(stale
            .session
            .commit_native_audio_terminal_group(&mut group, &mut ticket)
            .is_err());
        assert!(ticket.candidate_sequence().is_some());
        assert_eq!(stale.session.document(), &changed);
        assert!(stale.session.has_pending_native_authoring());
    }

    #[test]
    fn native_audio_terminal_group_finality_distinguishes_absent_and_empty_state() {
        use mondrian_core::NativeAudioAuthorCompletion as Completion;
        let mut f = Fixture::new(false);
        for (state, completion) in [
            (Some(vec![3]), Completion::Nonterminal),
            (None, Completion::CoherentConfiguration),
            (None, Completion::ParametersOnly),
        ] {
            let a = terminal_member(&f, f.owner, f.processor_id, &[], state, completion);
            let original = f.session.document().clone();
            assert!(f.session.begin_native_audio_terminal_group(f.sequence_id, a).is_err());
            assert_eq!(f.session.document(), &original);
            assert!(!f.session.has_pending_native_authoring());
        }
        let ui = AuthoringGestureId::new();
        f.session.begin_sequence_gesture(ui, f.sequence_id).unwrap();
        let blocked = terminal_member(
            &f,
            f.owner,
            f.processor_id,
            &[],
            Some(vec![1, 2]),
            Completion::CoherentConfiguration,
        );
        assert!(f.session.begin_native_audio_terminal_group(f.sequence_id, blocked).is_err());
        assert!(
            f.session.sequence_gesture_is_current(ui, f.sequence_id),
            "background native begin cannot finish a UI group"
        );
        f.session.end_sequence_gesture(ui).unwrap();
        let empty = terminal_member(
            &f,
            f.owner,
            f.processor_id,
            &[],
            Some(vec![]),
            Completion::CoherentConfiguration,
        );
        let mut group = f
            .session
            .begin_native_audio_terminal_group(f.sequence_id, empty.clone())
            .unwrap();
        let mut ticket =
            f.session.prepare_native_audio_terminal_group(&mut group, vec![empty]).unwrap();
        f.session
            .commit_native_audio_terminal_group(&mut group, &mut ticket)
            .unwrap()
            .unwrap();
        assert_eq!(
            f.processor().opaque_state.as_ref().map(|bytes| bytes.len()),
            Some(0)
        );
        f.session.finish_native_audio_terminal_group(&mut group).unwrap();

        let mut stateless = Fixture::with_initial_configuration(false, None);
        let value = terminal_member(
            &stateless,
            stateless.owner,
            stateless.processor_id,
            &[(0, 0.5)],
            None,
            Completion::ParametersOnly,
        );
        let mut group = stateless
            .session
            .begin_native_audio_terminal_group(stateless.sequence_id, value.clone())
            .unwrap();
        let mut ticket = stateless
            .session
            .prepare_native_audio_terminal_group(&mut group, vec![value])
            .unwrap();
        stateless
            .session
            .commit_native_audio_terminal_group(&mut group, &mut ticket)
            .unwrap()
            .unwrap();
        assert!(stateless.processor().opaque_state.is_none());
        stateless.session.finish_native_audio_terminal_group(&mut group).unwrap();
    }
}
