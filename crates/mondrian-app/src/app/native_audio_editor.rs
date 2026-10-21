//! Product native-editor surface over the actual installed execution owner.
//! UI address facts request one real ResolveAndOpen; only its current observed
//! target binds the surface. This module does not mint native runtime leases.

use super::product_action::AudioNativeEditorPayload;
use super::{AppMessage, AppState};
use mondrian_core::{
    AuthoringTimeDomain, MondrianError, NativeAudioEditorControlIntent as Intent,
    NativeAudioEditorControlOutcome as Outcome, NativeAudioEditorControlRefusal as Refusal,
    NativeAudioEditorControlSubmission as Submission, NativeAudioEditorInsertion as Insertion,
    NativeAudioEditorLocator, NativeAudioEditorOccurrenceOwner as Owner, NativeAudioEditorTarget,
    NativeAudioEditorTitle, ParameterId, Result,
};
use mondrian_editor_state::{NativeAudioAuthorGesture, NativeAudioAuthorOwner};
use mondrian_timeline::{
    inspect_audio_component, inspect_native_audio_authoring, AudioChannelStripOwner,
    AudioChannelStripRack, AudioProcessorRackAddress,
};
use std::collections::BTreeMap;

mod author_projection;
#[cfg(test)]
mod source_custody_contracts;
mod terminal_author;

/// One author transaction owns the App custody slot. Presentation is separate;
/// single-owner gestures and a terminal frontier cannot coexist here.
pub(super) enum NativeAudioAuthorWork {
    Single(NativeAudioSingleAuthorWork),
    Terminal(Box<terminal_author::TerminalAuthorWork>),
}

impl NativeAudioAuthorWork {
    fn single(&self) -> Option<&NativeAudioSingleAuthorWork> {
        match self {
            Self::Single(work) => Some(work),
            Self::Terminal(_) => None,
        }
    }

    fn single_mut(&mut self) -> Option<&mut NativeAudioSingleAuthorWork> {
        match self {
            Self::Single(work) => Some(work),
            Self::Terminal(_) => None,
        }
    }

    pub(super) fn blocks_source_replacement(&self) -> bool {
        match self {
            Self::Single(work) => work.blocks_source_replacement(),
            Self::Terminal(work) => work.blocks_source_replacement(),
        }
    }

    pub(super) fn snapshot_current_for_source(
        &self,
        source: &std::sync::Weak<super::audio_rendering::TimelineAudioPcmRenderer>,
        playback: &super::playback::AppAudioPlayback,
    ) -> bool {
        let actual: std::sync::Weak<dyn mondrian_media::AudioPcmRenderer> = source.clone();
        if !playback.matches_source_weak(&actual) {
            return false;
        }
        match self {
            Self::Single(work) => work.snapshot_current_for_source(source),
            Self::Terminal(work) => work.snapshot_current_for_source(source),
        }
    }

    pub(super) fn retained_state_entry_source(
        &self,
        target: Option<NativeAudioEditorTarget>,
        session: mondrian_editor_state::AuthoringSessionId,
        sequence: mondrian_core::SequenceId,
        playback: &super::playback::AppAudioPlayback,
    ) -> Option<std::sync::Arc<super::audio_rendering::TimelineAudioPcmRenderer>> {
        let work = self.single()?;
        let actual: std::sync::Weak<dyn mondrian_media::AudioPcmRenderer> = work.renderer.clone();
        if !playback.matches_source_weak(&actual) {
            return None;
        }
        work.retained_state_entry_source(target, session, sequence)
    }
}

pub(super) struct NativeAudioEditorProductContext {
    payload: AudioNativeEditorPayload,
    locator: NativeAudioEditorLocator,
    owner: NativeAudioAuthorOwner,
    schemas: BTreeMap<ParameterId, mondrian_core::automation::ParameterSchema>,
    parameter_observation: Option<mondrian_core::NativeAudioParameterObservation>,
    queued: Option<Intent>,
    target: Option<NativeAudioEditorTarget>,
    attachment_target: Option<NativeAudioEditorTarget>,
    // Only a genuine Opened receipt binds this presentation surface.
    surface_renderer: Option<std::sync::Weak<super::audio_rendering::TimelineAudioPcmRenderer>>,
}

/// Custody of the already accepted single author transaction, independent of
/// whether its editor surface is still present. This Weak selects the captured
/// Source; it does not issue an installation claim or keep a native owner alive.
pub(super) struct NativeAudioSingleAuthorWork {
    session_id: mondrian_editor_state::AuthoringSessionId,
    sequence_id: mondrian_core::SequenceId,
    renderer: std::sync::Weak<super::audio_rendering::TimelineAudioPcmRenderer>,
    gesture: Option<NativeAudioAuthorGesture>,
    outstanding: Option<NativeAudioEditorPendingAuthor>,
    state_entry_accepted: bool,
}

struct NativeAudioEditorPendingAuthor {
    proposal: mondrian_core::NativeAudioAuthorProposal,
    programs: Option<std::sync::Arc<[mondrian_audio::NativeAudioAuthorProgram]>>,
    snapshot: std::sync::Arc<mondrian_editor_state::AuthorExecutionSnapshot>,
    installed: Option<NativeAudioEditorTarget>,
    queued: bool,
}

impl NativeAudioSingleAuthorWork {
    pub(super) fn blocks_source_replacement(&self) -> bool {
        self.gesture.is_some() || self.outstanding.is_some()
    }

    fn snapshot_current_for_source(
        &self,
        source: &std::sync::Weak<super::audio_rendering::TimelineAudioPcmRenderer>,
    ) -> bool {
        std::sync::Weak::ptr_eq(source, &self.renderer)
            && self
                .renderer
                .upgrade()
                .is_some_and(|renderer| renderer.native_author_snapshot_current())
    }

    // Presentation may follow an accepted prefix only on its original surface.
    fn matches_surface(&self, context: &NativeAudioEditorProductContext) -> bool {
        self.session_id == context.payload.session_id
            && self.sequence_id == context.payload.sequence_id
            && context
                .surface_renderer
                .as_ref()
                .is_some_and(|surface| std::sync::Weak::ptr_eq(surface, &self.renderer))
            && self.gesture.as_ref().is_none_or(|gesture| {
                gesture.session_id() == context.payload.session_id
                    && gesture.sequence_id() == context.payload.sequence_id
                    && gesture.owner() == context.owner
                    && gesture.processor_id() == context.payload.processor_id
            })
            && self.outstanding.as_ref().is_some_and(|pending| {
                context.locator.matches_target(pending.proposal.target())
                    && (context.target == Some(pending.proposal.target())
                        || pending.installed.is_some_and(|target| context.target == Some(target)))
            })
    }

    /// Source selection only. Media independently verifies the same Arc,
    /// registration and current claim before retaining state-entry work.
    fn retained_state_entry_source(
        &self,
        target: Option<NativeAudioEditorTarget>,
        session: mondrian_editor_state::AuthoringSessionId,
        sequence: mondrian_core::SequenceId,
    ) -> Option<std::sync::Arc<super::audio_rendering::TimelineAudioPcmRenderer>> {
        if self.session_id != session || self.sequence_id != sequence {
            return None;
        }
        if let Some(target) = target {
            let outstanding = self.outstanding.as_ref()?;
            if outstanding.proposal.target() != target && outstanding.installed != Some(target) {
                return None;
            }
        } else if !self.state_entry_accepted {
            return None;
        }
        let renderer = self.renderer.upgrade()?;
        if target.is_none() && !renderer.native_author_snapshot_current() {
            return None;
        }
        Some(renderer)
    }
}

impl NativeAudioEditorProductContext {
    /// Only live surface attachment/control belongs to presentation custody.
    pub(super) fn blocks_source_replacement(&self) -> bool {
        self.queued.is_some() || self.attachment_target.is_some()
    }

    fn observe_attachment(
        &mut self,
        observation: mondrian_core::NativeAudioEditorAttachmentObservation,
        author_outstanding: bool,
    ) {
        if self.attachment_target != Some(observation.previous)
            || !self.locator.matches_target(observation.current)
            || author_outstanding
        {
            return;
        }
        if self.target != Some(observation.current) {
            self.parameter_observation = None;
        }
        self.target = Some(observation.current);
        // Keep the original attachment receipt for already queued Close. The
        // sole Runtime maps only this exact same-object receipt to its current
        // lease. An observed replacement clears presentation, never opens it.
        if !observation.retained {
            self.attachment_target = None;
        }
    }

    fn same_surface(&self, payload: &AudioNativeEditorPayload) -> bool {
        self.payload.session_id == payload.session_id
            && self.payload.sequence_id == payload.sequence_id
            && self.payload.address == payload.address
            && self.payload.occurrence == payload.occurrence
            && self.payload.processor_id == payload.processor_id
            && self.payload.definition == payload.definition
    }
}

fn failure(reason: impl std::fmt::Display) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "native_audio_editor_product".to_owned(),
        reason: reason.to_string(),
    }
}

fn refusal_key(reason: Refusal) -> &'static str {
    match reason {
        Refusal::UnsupportedAdapter => "native-editor-unavailable",
        Refusal::NoEditor => "native-editor-no-view",
        Refusal::ConfigurationPending => "native-editor-control-pending",
        Refusal::NotInstalled | Refusal::OwnerChanged | Refusal::Cancelled => {
            "native-editor-owner-changed"
        }
        Refusal::InvalidTitle | Refusal::WorkerUnavailable | Refusal::ProcessorRejected => {
            "native-editor-failed"
        }
    }
}

impl AppState {
    /// A successful Source replacement invalidates presentation, without
    /// changing author delivery, attachment or retained state-entry ownership.
    pub(super) fn invalidate_native_audio_parameter_presentation(&mut self) {
        if let Some(context) = &mut self.native_audio_editor {
            context.parameter_observation = None;
        }
    }

    fn prepare_native_audio_editor_context(
        &self,
        payload: AudioNativeEditorPayload,
    ) -> Result<NativeAudioEditorProductContext> {
        if self.authoring_session_id() != Some(payload.session_id)
            || payload.sequence_time.domain != AuthoringTimeDomain::Sequence(payload.sequence_id)
        {
            return Err(failure(
                "native editor belongs to a stale author Session or time domain",
            ));
        }
        let sequence = self
            .active_sequence()
            .filter(|sequence| sequence.id == payload.sequence_id)
            .ok_or_else(|| failure("native editor author Sequence is no longer active"))?;
        let (owner, occurrence, insertion) = match payload.address {
            AudioProcessorRackAddress::ChannelStrip { owner, rack } => {
                if payload.occurrence.is_some() {
                    return Err(failure("channel editor has a Scope occurrence"));
                }
                let occurrence = match owner {
                    AudioChannelStripOwner::Track { track_id } => Owner::Track(track_id),
                    AudioChannelStripOwner::Bus { bus_id } => Owner::Bus(bus_id),
                    AudioChannelStripOwner::ProgramOutput { output_id } => Owner::Output(output_id),
                };
                let insertion = match rack {
                    AudioChannelStripRack::PreFader => Insertion::PreFader,
                    AudioChannelStripRack::PostFader => Insertion::PostFader,
                };
                (
                    NativeAudioAuthorOwner::ChannelStrip { owner, rack },
                    occurrence,
                    insertion,
                )
            }
            AudioProcessorRackAddress::ProcessingScope { scope_id } => {
                let occurrence = payload
                    .occurrence
                    .ok_or_else(|| failure("native Scope editor requires an exact occurrence"))?;
                let inspection = inspect_audio_component(sequence, occurrence).map_err(failure)?;
                if !inspection.is_editable()
                    || inspection.component().processing.scope_id != scope_id
                {
                    return Err(failure(
                        "native Scope editor occurrence changed or is locked",
                    ));
                }
                (
                    NativeAudioAuthorOwner::ProcessingScope { scope_id, occurrence },
                    Owner::Contribution { edit_id: occurrence.edit_id, scope_id },
                    Insertion::Scope,
                )
            }
        };
        let processor = inspect_native_audio_authoring(
            sequence,
            &payload.address,
            payload.processor_id,
            &payload.definition,
        )
        .map_err(failure)?;
        let schemas = processor
            .parameters
            .iter()
            .map(|(id, parameter)| (id.clone(), parameter.schema.clone()))
            .collect();
        Ok(NativeAudioEditorProductContext {
            locator: NativeAudioEditorLocator {
                sequence: payload.sequence_id,
                instance: payload.processor_id,
                owner: occurrence,
                insertion,
            },
            payload,
            owner,
            schemas,
            parameter_observation: None,
            queued: None,
            target: None,
            attachment_target: None,
            surface_renderer: None,
        })
    }

    pub(super) fn control_native_audio_editor_product(
        &mut self,
        payload: AudioNativeEditorPayload,
    ) -> Result<()> {
        self.finish_parameter_gesture()?;
        let prepared =
            self.prepare_native_audio_editor_context(payload.clone())
                .inspect_err(|_error| {
                    self.set_status_hint(AppMessage::new("native-editor-owner-changed"), true);
                })?;
        if self
            .native_audio_editor
            .as_ref()
            .is_some_and(|context| context.queued.is_some())
            || (payload.open
                && self
                    .native_audio_author_work
                    .as_ref()
                    .is_some_and(|work| work.blocks_source_replacement()))
        {
            self.set_status_hint(AppMessage::new("native-editor-control-pending"), false);
            return Err(failure(
                "native author/control work must settle before another editor request",
            ));
        }
        let intent = if payload.open {
            if self.native_audio_editor.as_ref().is_some_and(|context| {
                context.attachment_target.is_some() && !context.same_surface(&payload)
            }) {
                self.set_status_hint(AppMessage::new("native-editor-close-current"), false);
                return Err(failure(
                    "close the actual current editor before selecting another owner",
                ));
            }
            if !self.audio_playback.native_editor_source_available() {
                let anchor = self.authoritative_audio_anchor(
                    &self.playback_engine,
                    self.playback_engine.monotonic_high_water(),
                    "native_audio_editor_open",
                )?;
                let source = self.audio_playback_renderer("native_audio_editor_open")?;
                self.commit_audio_playback("native_audio_editor_open", anchor, source)?;
            }
            Intent::ResolveAndOpen {
                locator: prepared.locator,
                title: NativeAudioEditorTitle::new("Mondrian".to_owned())
                    .map_err(|reason| failure(format!("{reason:?}")))?,
            }
        } else {
            let target = self
                .native_audio_editor
                .as_ref()
                .filter(|context| context.same_surface(&payload))
                .and_then(|context| context.attachment_target)
                .ok_or_else(|| failure("native editor has no exact actual attachment to close"))?;
            Intent::Close { target }
        };
        let submitted = self
            .audio_playback
            .request_native_audio_editor(intent.clone())
            .map_err(failure)?;
        match submitted {
            Submission::Queued => {
                if payload.open {
                    let mut prepared = prepared;
                    prepared.queued = Some(intent);
                    self.native_audio_editor = Some(prepared);
                } else if let Some(context) = &mut self.native_audio_editor {
                    context.queued = Some(intent);
                }
                self.set_status_hint(AppMessage::new("native-editor-opening"), false);
            }
            Submission::Refused(reason) => {
                self.set_status_hint(AppMessage::new(refusal_key(reason)), true)
            }
        }
        Ok(())
    }

    pub(super) fn observe_native_audio_editor_product(
        &mut self,
        renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
        generation: u64,
        intent: Intent,
        outcome: Outcome,
    ) -> Result<()> {
        tracing::debug!(
            ?renderer_id,
            generation,
            ?outcome,
            "native editor control completed"
        );
        let Some(context) = &self.native_audio_editor else {
            return Ok(());
        };
        if context.queued.as_ref() != Some(&intent) {
            return Ok(());
        }
        let current = self.prepare_native_audio_editor_context(context.payload.clone());
        if current.is_err() {
            // Native cleanup has its own exact Runtime admission; stale author
            // metadata never authorizes a callback or transfers this target.
            if let Outcome::Opened { target, .. } = outcome {
                self.audio_playback
                    .request_native_audio_editor(Intent::Close { target })
                    .map_err(failure)?;
            }
            if let Some(context) = &mut self.native_audio_editor {
                context.queued = None;
                context.target = None;
                context.attachment_target = None;
            }
            self.set_status_hint(AppMessage::new("native-editor-owner-changed"), true);
            return Ok(());
        }
        let Some(context) = &mut self.native_audio_editor else {
            return Ok(());
        };
        context.queued = None;
        let key = match outcome {
            Outcome::Opened { target, .. } if context.locator.matches_target(target) => {
                context.surface_renderer = self.native_audio_renderer.clone();
                context.target = Some(target);
                context.attachment_target = Some(target);
                "native-editor-opened"
            }
            Outcome::Closed { target } if context.locator.matches_target(target) => {
                // Retain processor facts for the already-owned author delivery;
                // only the genuine view attachment controls the UI open state.
                context.target = Some(target);
                context.attachment_target = None;
                "native-editor-closed"
            }
            Outcome::Refused(reason) => refusal_key(reason),
            Outcome::ClosePending { .. } => "native-editor-cleanup-pending",
            _ => "native-editor-owner-changed",
        };
        self.set_status_hint(AppMessage::new(key), matches!(outcome, Outcome::Refused(_)));
        Ok(())
    }

    pub(crate) fn native_audio_editor_surface_state(
        &self,
        payload: &AudioNativeEditorPayload,
    ) -> (bool, bool) {
        self.native_audio_editor
            .as_ref()
            .filter(|context| context.same_surface(payload))
            .map_or((false, false), |context| {
                (
                    context.attachment_target.is_some(),
                    context.queued.is_some()
                        || self
                            .native_audio_author_work
                            .as_ref()
                            .and_then(NativeAudioAuthorWork::single)
                            .is_some_and(|work| {
                                work.matches_surface(context) && work.outstanding.is_some()
                            }),
                )
            })
    }
}

impl AppState {
    pub(super) fn observe_native_audio_editor_attachment(
        &mut self,
        observation: mondrian_core::NativeAudioEditorAttachmentObservation,
    ) {
        let author_outstanding = self.native_audio_editor.as_ref().is_some_and(|context| {
            self.native_audio_author_work
                .as_ref()
                .and_then(NativeAudioAuthorWork::single)
                .is_some_and(|work| work.matches_surface(context) && work.outstanding.is_some())
        });
        if let Some(context) = &mut self.native_audio_editor {
            context.observe_attachment(observation, author_outstanding);
        }
    }

    pub(super) fn observe_native_audio_parameter_observation(
        &mut self,
        observation: mondrian_core::NativeAudioParameterObservation,
    ) {
        let Some(context) = &mut self.native_audio_editor else {
            return;
        };
        // Media checked renderer/generation/installation claim. Compare the
        // actual current surface target too; a same class/ParameterId is not an
        // incarnation. A new target makes this previous observation unavailable.
        if context.target == Some(observation.target())
            && context.locator.matches_target(observation.target())
            && context.parameter_observation.as_ref().is_none_or(|previous| {
                previous.target() != observation.target()
                    || (previous.process_id() == observation.process_id()
                        && previous.object_serial() == observation.object_serial()
                        && previous.source_revision() <= observation.source_revision())
            })
        {
            context.parameter_observation = Some(observation);
        }
    }

    pub(crate) fn native_audio_parameter_presentation(
        &self,
        payload: &AudioNativeEditorPayload,
        id: &ParameterId,
    ) -> Option<&mondrian_core::NativeAudioParameterFact> {
        let context = self.native_audio_editor.as_ref()?;
        if !context.same_surface(payload) {
            return None;
        }
        let observation = context.parameter_observation.as_ref()?;
        if context.target != Some(observation.target()) {
            return None;
        }
        observation.facts().iter().rev().find(|fact| &fact.parameter == id)
    }

    /// Read the sole presentation cache after checking the complete static
    /// owner and installed identity. This never queries the plug-in.
    pub(super) fn native_audio_parameter_adoption_snapshot(
        &self,
        scalar: &super::parameter_gesture::AudioScalarGestureTarget,
    ) -> Option<super::parameter_gesture::DisplayedNativeParameterIdentity> {
        use super::parameter_gesture::{AudioScalarOwner, DisplayedNativeParameterIdentity};
        let AudioScalarOwner::Parameter {
            owner, processor_id, definition, parameter_schema,
        } = &scalar.owner else {
            return None;
        };
        let context = self.native_audio_editor.as_ref()?;
        if context.queued.is_some()
            || self.native_audio_author_work.as_ref()
                .is_some_and(NativeAudioAuthorWork::blocks_source_replacement)
            || context.payload.session_id != scalar.lease.session_id
            || context.payload.sequence_id != scalar.lease.sequence_id
            || context.payload.processor_id != *processor_id
            || context.payload.definition != *definition
            || Some(context.payload.address) != scalar.owner.processor_address()
            || context.schemas.get(&parameter_schema.parameter_id) != Some(&**parameter_schema)
        {
            return None;
        }
        let (occurrence, native_owner, insertion) = match *owner {
            mondrian_audio::AudioProcessorInputInspectionTarget::ChannelStrip { owner, rack } => {
                let native_owner = match owner {
                    AudioChannelStripOwner::Track { track_id } => Owner::Track(track_id),
                    AudioChannelStripOwner::Bus { bus_id } => Owner::Bus(bus_id),
                    AudioChannelStripOwner::ProgramOutput { output_id } => Owner::Output(output_id),
                };
                let insertion = match rack {
                    AudioChannelStripRack::PreFader => Insertion::PreFader,
                    AudioChannelStripRack::PostFader => Insertion::PostFader,
                };
                (None, native_owner, insertion)
            }
            mondrian_audio::AudioProcessorInputInspectionTarget::ProcessingScope {
                scope_id, occurrence,
            } => (
                Some(occurrence),
                Owner::Contribution { edit_id: occurrence.edit_id, scope_id },
                Insertion::Scope,
            ),
        };
        let locator = NativeAudioEditorLocator {
            sequence: scalar.lease.sequence_id,
            instance: *processor_id,
            owner: native_owner,
            insertion,
        };
        let observation = context.parameter_observation.as_ref()?;
        if context.payload.occurrence != occurrence
            || context.locator != locator
            || context.target != Some(observation.target())
            || !locator.matches_target(observation.target())
        {
            return None;
        }
        let fact = observation.facts().iter().rev()
            .find(|fact| fact.parameter == parameter_schema.parameter_id)?;
        if !matches!(fact.origin, mondrian_core::NativeAudioParameterObservationOrigin::MainQuery { .. }) {
            return None;
        }
        DisplayedNativeParameterIdentity::from_observation(observation, fact)
    }

    pub(super) fn observe_native_audio_author_proposal(
        &mut self,
        proposal: mondrian_core::NativeAudioAuthorProposal,
    ) -> Result<()> {
        if matches!(&self.native_audio_author_work, Some(NativeAudioAuthorWork::Terminal(work)) if work.blocks_source_replacement())
        {
            return Err(failure(
                "terminal native author work requires its original delivery route",
            ));
        }
        if let Some(pending) = self
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .and_then(|work| work.outstanding.as_ref())
        {
            if pending.proposal.same_retained_proposal(&proposal) {
                return self.queue_native_audio_author_handoff();
            }
            return Err(failure(
                "native author delivery changed before acknowledgement",
            ));
        }
        let Some(context) = self.native_audio_editor.as_ref() else {
            // A new prefix still needs the original GUI admission path. Already
            // accepted Work above remains serviceable without presentation.
            return Ok(());
        };
        if context.target.is_none()
            && context.surface_renderer.is_none()
            && context.attachment_target.is_none()
            && self
                .native_audio_author_work
                .as_ref()
                .and_then(NativeAudioAuthorWork::single)
                .is_none_or(|work| work.gesture.is_none())
            && matches!(
                &context.queued,
                Some(Intent::ResolveAndOpen { locator, .. }) if *locator == context.locator
            )
            && proposal.sequence() == context.payload.sequence_id
            && context.locator.matches_target(proposal.target())
        {
            self.prepare_native_audio_editor_context(context.payload.clone())?;
            // The sole Runtime retains and reoffers this proposal. Only the
            // actual Opened receipt binds the surface; installing author state
            // now would supersede the claim carried by that Open completion.
            return Ok(());
        }
        if context.target != Some(proposal.target())
            || proposal.sequence() != context.payload.sequence_id
            || !context.locator.matches_target(proposal.target())
        {
            return Err(failure(
                "native proposal belongs to another installed editor",
            ));
        }
        self.prepare_native_audio_editor_context(context.payload.clone())?;
        // Capturing the historical surface identity admits only the canonical
        // author prefix. This instantaneous liveness check grants no native
        // permission. Queueing requires the actual Media slot before upgrading
        // this Weak or constructing native work.
        let captured_renderer = context
            .surface_renderer
            .as_ref()
            .filter(|renderer| renderer.strong_count() != 0)
            .ok_or_else(|| failure("native editor's captured renderer has retired"))?
            .clone();
        if let Some(work) =
            self.native_audio_author_work.as_ref().and_then(NativeAudioAuthorWork::single)
            && let Some(gesture) = &work.gesture
            && (gesture.session_id() != context.payload.session_id
                || gesture.sequence_id() != context.payload.sequence_id
                || gesture.owner() != context.owner
                || gesture.processor_id() != context.payload.processor_id
                || !std::sync::Weak::ptr_eq(&work.renderer, &captured_renderer))
        {
            return Err(failure(
                "native author gesture belongs to another captured owner",
            ));
        }
        let sequence = self
            .active_sequence()
            .filter(|sequence| sequence.id == proposal.sequence())
            .ok_or_else(|| failure("native author Sequence changed"))?;
        let processor = inspect_native_audio_authoring(
            sequence,
            &context.payload.address,
            context.payload.processor_id,
            &context.payload.definition,
        )
        .map_err(failure)?;
        let update = author_projection::project_update(
            sequence,
            context.owner,
            processor,
            Some(&context.schemas),
            &proposal,
            Some(context.payload.sequence_time),
        )?;
        let payload = context.payload.clone();
        let owner = context.owner;
        let gesture = self
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .and_then(|work| work.gesture.clone());
        let author =
            self.authoring.as_mut().ok_or_else(|| failure("native author Session closed"))?;
        let gesture = match gesture {
            Some(gesture) => gesture,
            None => author.begin_native_audio_gesture(
                payload.sequence_id,
                owner,
                payload.processor_id,
                payload.definition.clone(),
            )?,
        };
        // Begin has already installed the sole Session barrier. Retain its
        // handle BEFORE update, whose validation may still fail.
        if self
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .is_none_or(|work| work.gesture.is_none())
        {
            self.native_audio_author_work =
                Some(NativeAudioAuthorWork::Single(NativeAudioSingleAuthorWork {
                    session_id: payload.session_id,
                    sequence_id: payload.sequence_id,
                    renderer: captured_renderer,
                    gesture: Some(gesture.clone()),
                    outstanding: None,
                    state_entry_accepted: false,
                }));
        }
        let commit = self
            .authoring
            .as_mut()
            .ok_or_else(|| failure("native author Session closed"))?
            .update_native_audio_gesture(&gesture, update)?;
        let author =
            self.authoring.as_ref().ok_or_else(|| failure("native author Session closed"))?;
        let snapshot = std::sync::Arc::new(author.execution_snapshot());
        // Store the accepted prefix before later compilation or binding.
        self.native_audio_author_work
            .as_mut()
            .and_then(NativeAudioAuthorWork::single_mut)
            .ok_or_else(|| failure("native author work disappeared"))?
            .outstanding = Some(NativeAudioEditorPendingAuthor {
            proposal,
            programs: None,
            snapshot,
            installed: None,
            queued: false,
        });
        if let Some(commit) = commit {
            self.consume_native_audio_authoring_commit(commit)?;
        }
        self.queue_native_audio_author_handoff()
    }

    fn queue_native_audio_author_handoff(&mut self) -> Result<()> {
        use mondrian_media::AudioPcmNativeAuthorHandoffIntent as Intent;
        let Some(work) =
            self.native_audio_author_work.as_ref().and_then(NativeAudioAuthorWork::single)
        else {
            return Ok(());
        };
        let Some(pending) = &work.outstanding else {
            return Ok(());
        };
        if pending.queued {
            return Ok(());
        }
        // Completion delivery can precede a worker-disconnected event in one
        // poll. Only the actual slot, not that historical event, keeps a strong
        // owner alive while this UI path temporarily upgrades its captured Weak.
        let actual: std::sync::Weak<dyn mondrian_media::AudioPcmRenderer> = work.renderer.clone();
        if !self.audio_playback.matches_source_weak(&actual) {
            self.set_status_hint(AppMessage::new("native-editor-control-pending"), true);
            return Ok(());
        }
        let renderer = work
            .renderer
            .upgrade()
            .ok_or_else(|| failure("captured native renderer retired"))?;
        if pending.programs.is_none() {
            let author =
                self.authoring.as_ref().ok_or_else(|| failure("native author Session closed"))?;
            if author.session_id() != work.session_id || pending.snapshot.enter_commit().is_err() {
                return Err(failure("pending native author snapshot changed"));
            }
            let sequence = author
                .sequence(work.sequence_id)
                .ok_or_else(|| failure("native author Sequence disappeared"))?;
            let mut programs = Vec::with_capacity(sequence.audio_program.outputs.len());
            for output in &sequence.audio_program.outputs {
                programs.push(mondrian_audio::NativeAudioAuthorProgram {
                    sequence: sequence.id,
                    program: std::sync::Arc::new(
                        mondrian_audio::compile_audio_program(
                            sequence,
                            renderer.native_author_compile_request(output.id),
                        )
                        .map_err(failure)?,
                    ),
                });
            }
            let work = self
                .native_audio_author_work
                .as_mut()
                .and_then(NativeAudioAuthorWork::single_mut)
                .ok_or_else(|| failure("native author work disappeared"))?;
            let pending = work
                .outstanding
                .as_mut()
                .ok_or_else(|| failure("native author batch disappeared"))?;
            pending.programs = Some(programs.into());
        }
        let work = self
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .ok_or_else(|| failure("native author work disappeared"))?;
        let pending = work
            .outstanding
            .as_ref()
            .ok_or_else(|| failure("native author batch disappeared"))?;
        let programs = pending
            .programs
            .as_ref()
            .ok_or_else(|| failure("native author Program preparation absent"))?;
        let target = pending.installed.unwrap_or_else(|| pending.proposal.target());
        let intent = if pending.installed.is_some() {
            Intent::Acknowledge
        } else {
            Intent::Install
        };
        let request = renderer.native_author_handoff_request(
            pending.proposal.clone(),
            target,
            std::sync::Arc::clone(&pending.snapshot),
            std::sync::Arc::clone(programs),
            intent,
        );
        let queued = self
            .audio_playback
            .request_native_audio_author_handoff(request)
            .map_err(failure)?;
        if let Some(work) = self
            .native_audio_author_work
            .as_mut()
            .and_then(NativeAudioAuthorWork::single_mut)
            && let Some(pending) = &mut work.outstanding
        {
            pending.queued = queued;
        }
        if !queued {
            self.set_status_hint(AppMessage::new("native-editor-control-pending"), false);
        }
        Ok(())
    }

    pub(super) fn observe_native_audio_author_handoff(
        &mut self,
        proposal: mondrian_core::NativeAudioAuthorProposal,
        status: mondrian_media::AudioPcmNativeAuthorHandoffStatus,
    ) -> Result<bool> {
        use mondrian_media::AudioPcmNativeAuthorHandoffStatus as Status;
        let Some(work) = self
            .native_audio_author_work
            .as_mut()
            .and_then(NativeAudioAuthorWork::single_mut)
        else {
            return Ok(false);
        };
        let Some(pending) = &mut work.outstanding else {
            return Ok(false);
        };
        if !pending.proposal.same_retained_proposal(&proposal) {
            return Ok(false);
        }
        pending.queued = false;
        // Compute before consuming the original prefix; never apply a late A
        // receipt to a replacement B surface, even in the same Sequence.
        let surface_matches = self
            .native_audio_editor
            .as_ref()
            .is_some_and(|context| work.matches_surface(context));
        match status {
            Status::Acknowledged { target, continuity } => {
                let state_entry = continuity
                    == mondrian_media::AudioPcmNativeAuthorHandoffContinuity::StateEntryRequired;
                if proposal.terminal() {
                    let gesture = work
                        .gesture
                        .as_ref()
                        .ok_or_else(|| failure("native terminal group is absent"))?;
                    let author =
                        self.authoring.as_mut().ok_or_else(|| failure("native Session closed"))?;
                    author.finish_native_audio_gesture(
                        gesture,
                        mondrian_timeline::NativeAudioAuthorUpdate {
                            parameters: Vec::new(),
                            opaque_state: proposal.state().cloned(),
                        },
                        proposal.completion(),
                    )?;
                    work.gesture = None;
                }
                work.outstanding = None;
                if state_entry {
                    work.state_entry_accepted = true;
                }
                if surface_matches {
                    let context = self.native_audio_editor.as_mut().expect("matched surface");
                    context.target = if state_entry { None } else { Some(*target) };
                    if state_entry {
                        context.attachment_target = None;
                        self.set_status_hint(AppMessage::new("native-editor-closed"), false);
                    } else if context.attachment_target.is_some() {
                        self.set_status_hint(AppMessage::new("native-editor-opened"), false);
                    }
                }
                return Ok(true);
            }
            Status::InstalledAwaitingAcknowledgement { target, continuity } => {
                work.outstanding.as_mut().expect("matched prefix").installed = Some(*target);
                if surface_matches {
                    let context = self.native_audio_editor.as_mut().expect("matched surface");
                    if continuity
                        == mondrian_media::AudioPcmNativeAuthorHandoffContinuity::StateEntryRequired
                    {
                        context.attachment_target = None;
                    }
                    context.target = Some(*target);
                }
                self.set_status_hint(AppMessage::new("native-editor-control-pending"), false);
            }
            Status::Refused(reason) => {
                tracing::warn!(?reason, "native author handoff retained for coherent retry");
                self.set_status_hint(AppMessage::new("native-editor-control-pending"), true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod attachment_continuity_contracts {
    use super::*;
    use std::num::{NonZeroU32, NonZeroU64};

    fn pending_open() -> (
        tempfile::TempDir,
        AppState,
        mondrian_core::NativeAudioAuthorProposal,
    ) {
        pending_open_with_parameter(false)
    }

    pub(super) fn pending_open_with_parameter(
        with_parameter: bool,
    ) -> (
        tempfile::TempDir,
        AppState,
        mondrian_core::NativeAudioAuthorProposal,
    ) {
        let directory = tempfile::tempdir().expect("actual author Session fixture");
        let library = mondrian_assets::AssetLibrary::open(directory.path().join("library"))
            .expect("actual library");
        let mut state = AppState::with_audio_processor_resolver(std::sync::Arc::new(
            mondrian_audio::BuiltInAudioProcessorResolver,
        ));
        let mut sequence = mondrian_timeline::Sequence::new("Pending native editor");
        let sequence_id = sequence.id;
        let output = sequence.audio_program.outputs[0].id;
        let instance = mondrian_timeline::audio::AudioProcessorInstance {
            id: mondrian_core::AudioProcessorInstanceId::new(),
            definition: mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
                plugin_id: "owned pending-open metadata".into(),
                schema_version: 1,
                // Author metadata admission requires a nonzero pinned revision;
                // this ordinary control never resolves or loads that binary.
                binary_sha256: Some([7; 32]),
            },
            bypassed: false,
            parameters: if with_parameter {
                use mondrian_core::automation::{
                    ParameterInvalidValuePolicy, ParameterNumericContract, ParameterSchema,
                    ParameterUnit,
                };
                let id = ParameterId::new("clap.param.7").unwrap();
                let schema =
                    ParameterSchema::v1(id.clone(), mondrian_core::PropertyValue::Double(0.25))
                        .with_numeric_contract(
                            ParameterUnit::Unitless,
                            ParameterNumericContract::closed(
                                0.0,
                                1.0,
                                None,
                                ParameterInvalidValuePolicy::Reject,
                            )
                            .unwrap(),
                        );
                BTreeMap::from([(
                    id,
                    mondrian_timeline::audio::AudioProcessorParameter::from_schema(schema).unwrap(),
                )])
                .into()
            } else {
                Default::default()
            },
            opaque_state: Some(vec![1, 2, 3, 4].into()),
        };
        let processor_id = instance.id;
        let definition = instance.definition.clone();
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
        if with_parameter {
            let mut peer = sequence.audio_program.outputs[0].strip.pre_fader.processors[0].clone();
            peer.id = mondrian_core::AudioProcessorInstanceId::new();
            sequence.audio_program.outputs[0].strip.pre_fader.processors.push(peer);
        }
        state.test_set_sequence(Some(sequence));
        state.test_set_asset_library(Some(library));
        let payload = AudioNativeEditorPayload {
            session_id: state.authoring_session_id().expect("actual author Session"),
            sequence_id,
            address: AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::ProgramOutput { output_id: output },
                rack: AudioChannelStripRack::PreFader,
            },
            occurrence: None,
            processor_id,
            definition,
            sequence_time: mondrian_core::DomainTime::new(
                AuthoringTimeDomain::Sequence(sequence_id),
                mondrian_core::TimelineTime::ZERO,
            ),
            open: true,
        };
        let mut context = state.prepare_native_audio_editor_context(payload).unwrap();
        context.queued = Some(Intent::ResolveAndOpen {
            locator: context.locator,
            title: NativeAudioEditorTitle::new("Mondrian".to_owned()).unwrap(),
        });
        let target = NativeAudioEditorTarget {
            instance: context.locator.instance,
            owner: context.locator.owner,
            insertion: context.locator.insertion,
            ..self::context().target.unwrap()
        };
        let proposal = early_proposal(target, sequence_id);
        state.native_audio_editor = Some(context);
        (directory, state, proposal)
    }

    fn early_proposal(
        target: NativeAudioEditorTarget,
        sequence: mondrian_core::SequenceId,
    ) -> mondrian_core::NativeAudioAuthorProposal {
        mondrian_core::NativeAudioAuthorProposal::project(
            target,
            sequence,
            1,
            mondrian_core::NativeAudioAuthorBoundaryFacts {
                completion: mondrian_core::NativeAudioAuthorCompletion::Nonterminal,
                open_parameter_edits: 0,
                group_depth: 1,
            },
            vec![mondrian_core::NativeAudioAuthorCallback::GroupStarted],
            None,
        )
        .expect("bounded proposal DTO, not an execution grant")
    }

    fn observe_early_without_authority(
        state: &mut AppState,
        proposal: mondrian_core::NativeAudioAuthorProposal,
        accepted: bool,
    ) {
        let author = state.authoring.as_ref().unwrap();
        let generation = author.author_generation();
        let history = author.history().diagnostics();
        let dirty = author.is_dirty();
        let document = serde_json::to_value(author.document()).unwrap();
        let queued = state.native_audio_editor.as_ref().unwrap().queued.clone();
        assert_eq!(
            state.observe_native_audio_author_proposal(proposal).is_ok(),
            accepted
        );
        let author = state.authoring.as_ref().unwrap();
        assert_eq!(author.author_generation(), generation);
        assert_eq!(author.history().diagnostics(), history);
        assert_eq!(author.is_dirty(), dirty);
        assert_eq!(serde_json::to_value(author.document()).unwrap(), document);
        let context = state.native_audio_editor.as_ref().unwrap();
        assert_eq!(
            context.queued, queued,
            "only the real Open completion consumes this intent"
        );
        assert!(context.target.is_none());
        assert!(context.attachment_target.is_none());
        assert!(context.surface_renderer.is_none());
        assert!(state.native_audio_author_work.is_none());
        assert!(context.parameter_observation.is_none());
        assert!(state.native_audio_renderer.is_none());
    }

    #[test]
    fn pending_open_defers_repeated_proposal_without_author_transaction_or_ack() {
        let (_directory, mut state, proposal) = pending_open();
        observe_early_without_authority(&mut state, proposal.clone(), true);
        observe_early_without_authority(&mut state, proposal, true);
    }

    #[test]
    fn pending_open_rejects_foreign_proposal_and_absent_or_foreign_open_intent() {
        for case in 0..5 {
            let (_directory, mut state, proposal) = pending_open();
            let context = state.native_audio_editor.as_mut().unwrap();
            let proposal = match case {
                0 => early_proposal(proposal.target(), mondrian_core::SequenceId::new()),
                1 => early_proposal(
                    NativeAudioEditorTarget {
                        instance: mondrian_core::AudioProcessorInstanceId::new(),
                        ..proposal.target()
                    },
                    proposal.sequence(),
                ),
                2 => {
                    context.queued = None; // Also the state after a refused Open.
                    proposal
                }
                3 => {
                    context.queued = Some(Intent::ResolveAndOpen {
                        locator: NativeAudioEditorLocator {
                            instance: mondrian_core::AudioProcessorInstanceId::new(),
                            ..context.locator
                        },
                        title: NativeAudioEditorTitle::new("Mondrian".to_owned()).unwrap(),
                    });
                    proposal
                }
                4 => {
                    context.queued = Some(Intent::Close { target: proposal.target() });
                    proposal
                }
                _ => unreachable!(),
            };
            observe_early_without_authority(&mut state, proposal, false);
        }
    }

    #[test]
    fn pending_open_revalidates_author_session_and_processor_metadata() {
        for stale_session in [true, false] {
            let (_directory, mut state, proposal) = pending_open();
            let context = state.native_audio_editor.as_mut().unwrap();
            if stale_session {
                context.payload.session_id = self::context().payload.session_id;
            } else {
                context.payload.definition =
                    mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
                        plugin_id: "changed definition".into(),
                        schema_version: 1,
                        binary_sha256: None,
                    };
            }
            observe_early_without_authority(&mut state, proposal, false);
        }
    }

    fn context() -> NativeAudioEditorProductContext {
        let sequence = mondrian_core::SequenceId::new();
        let bus = mondrian_core::MixBusId::new();
        let instance = mondrian_core::AudioProcessorInstanceId::new();
        let owner = mondrian_timeline::AudioChannelStripOwner::Bus { bus_id: bus };
        let rack = mondrian_timeline::AudioChannelStripRack::PreFader;
        let target = NativeAudioEditorTarget {
            root_plan: mondrian_core::AudioPreparedPlanId::new(),
            root_session: mondrian_core::AudioRenderSessionId::new(),
            local_plan: mondrian_core::AudioPreparedPlanId::new(),
            local_session: mondrian_core::AudioRenderSessionId::new(),
            instance,
            owner: Owner::Bus(bus),
            insertion: Insertion::PreFader,
            continuity_epoch: None,
            binding: mondrian_core::AudioProcessorBindingId::new(),
            installed_generation: NonZeroU64::new(7).expect("metadata"),
        };
        let session_id = serde_json::from_str("\"00000000-0000-4000-8000-000000000007\"")
            .expect("current-format author lifetime metadata, not an execution grant");
        NativeAudioEditorProductContext {
            payload: AudioNativeEditorPayload {
                session_id,
                sequence_id: sequence,
                address: mondrian_timeline::AudioProcessorRackAddress::ChannelStrip { owner, rack },
                occurrence: None,
                processor_id: instance,
                definition: mondrian_timeline::audio::AudioProcessorDefinitionRef::Clap {
                    plugin_id: "owned fixture".into(),
                    schema_version: 1,
                    binary_sha256: None,
                },
                sequence_time: mondrian_core::DomainTime::new(
                    AuthoringTimeDomain::Sequence(sequence),
                    mondrian_core::TimelineTime::ZERO,
                ),
                open: true,
            },
            locator: NativeAudioEditorLocator {
                sequence,
                instance,
                owner: Owner::Bus(bus),
                insertion: Insertion::PreFader,
            },
            owner: NativeAudioAuthorOwner::ChannelStrip { owner, rack },
            schemas: BTreeMap::new(),
            parameter_observation: None,
            queued: None,
            target: Some(target),
            attachment_target: Some(target),
            surface_renderer: None,
        }
    }
    fn observation(
        context: &NativeAudioEditorProductContext,
        retained: bool,
    ) -> mondrian_core::NativeAudioEditorAttachmentObservation {
        let previous = context.attachment_target.expect("actual cached attachment metadata");
        let mut current = previous;
        current.binding = mondrian_core::AudioProcessorBindingId::new();
        current.continuity_epoch = Some(31);
        mondrian_core::NativeAudioEditorAttachmentObservation {
            previous,
            current,
            process_id: NonZeroU32::new(std::process::id()).expect("PID"),
            object_serial: NonZeroU64::new(3).expect("object"),
            retained,
        }
    }
    #[test]
    fn native_attachment_app_same_view_updates_author_target_and_keeps_queued_close_receipt() {
        let mut context = context();
        let observation = observation(&context, true);
        let old = observation.previous;
        context.queued = Some(Intent::Close { target: old });
        context.observe_attachment(observation, false);
        assert_eq!(context.target, Some(observation.current));
        assert_eq!(context.attachment_target, Some(old));
        assert_eq!(context.queued, Some(Intent::Close { target: old }));
        assert!(
            context.surface_renderer.is_none(),
            "observation metadata cannot mint a Runtime/author capability"
        );
        let mut newer = observation;
        newer.current.binding = mondrian_core::AudioProcessorBindingId::new();
        newer.current.continuity_epoch = Some(32);
        context.observe_attachment(newer, false);
        assert_eq!(context.target, Some(newer.current));
        assert_eq!(context.attachment_target, Some(old));

        // A real Session issues the snapshot held by the private outstanding
        // DTO; this fixture does not manufacture an execution/native lease.
        let directory = tempfile::tempdir().expect("actual author Session fixture");
        let library = mondrian_assets::AssetLibrary::open(directory.path().join("library"))
            .expect("actual library");
        let sequence = mondrian_timeline::Sequence::new("Attachment observation retry");
        let sequence_id = sequence.id;
        let document = mondrian_project::ProjectDocument::new(
            "Attachment retry",
            mondrian_timeline::SequenceCollection::new(sequence),
            mondrian_core::ProjectColorEnvironment::default(),
            mondrian_timeline::SequenceSettings::default(),
            mondrian_core::ProjectSettings::default(),
        );
        let session = mondrian_editor_state::AuthoringSession::new_unsaved(
            document,
            directory.path().join("project.mdp"),
            directory.path().to_owned(),
            library,
        )
        .expect("actual Session");
        let proposal = mondrian_core::NativeAudioAuthorProposal::project(
            newer.current,
            sequence_id,
            1,
            mondrian_core::NativeAudioAuthorBoundaryFacts {
                completion: mondrian_core::NativeAudioAuthorCompletion::Nonterminal,
                open_parameter_edits: 0,
                group_depth: 1,
            },
            vec![mondrian_core::NativeAudioAuthorCallback::GroupStarted],
            None,
        )
        .expect("bounded outstanding DTO, not author permission");
        let mut outstanding = Some(NativeAudioEditorPendingAuthor {
            proposal: proposal.clone(),
            programs: None,
            snapshot: std::sync::Arc::new(session.execution_snapshot()),
            installed: None,
            queued: false,
        });
        let mut retry = newer;
        retry.current.binding = mondrian_core::AudioProcessorBindingId::new();
        retry.current.continuity_epoch = Some(33);
        context.observe_attachment(retry, outstanding.is_some());
        assert_eq!(
            context.target,
            Some(newer.current),
            "outstanding delivery keeps its admitted target"
        );
        assert_eq!(context.attachment_target, Some(old));
        assert!(outstanding
            .as_ref()
            .expect("retained")
            .proposal
            .same_retained_proposal(&proposal));
        assert_eq!(context.queued, Some(Intent::Close { target: old }));
        // The production exact Handoff Ack clears this field; the same
        // unconsumed original-attachment fact can then refresh on a later Poll.
        outstanding = None;
        context.observe_attachment(retry, outstanding.is_some());
        assert_eq!(context.target, Some(retry.current));
        assert_eq!(context.attachment_target, Some(old));
        assert_eq!(context.queued, Some(Intent::Close { target: old }));
    }
    #[test]
    fn native_attachment_app_replacement_clears_visible_view_and_never_implicitly_reopens() {
        let mut context = context();
        let detached = observation(&context, false);
        context.observe_attachment(detached, false);
        assert_eq!(context.target, Some(detached.current));
        assert_eq!(context.attachment_target, None);
        let mut late = detached;
        late.retained = true;
        context.observe_attachment(late, false);
        assert_eq!(
            context.attachment_target, None,
            "late prior fact cannot reopen a rebuilt object"
        );
        assert_eq!(context.target, Some(detached.current));
        assert!(context.queued.is_none());
    }
    #[test]
    fn native_attachment_app_other_owner_or_prior_receipt_never_refreshes_current_surface() {
        let mut context = context();
        let before = context.target;
        let mut wrong = observation(&context, true);
        wrong.current.instance = mondrian_core::AudioProcessorInstanceId::new();
        context.observe_attachment(wrong, false);
        assert_eq!(context.target, before);
        let mut stale = observation(&context, true);
        stale.previous.binding = mondrian_core::AudioProcessorBindingId::new();
        context.observe_attachment(stale, false);
        assert_eq!(context.target, before);
    }

    // These ordinary controls exercise actual Session/Timeline custody through
    // the App handlers. Their Core target/status DTOs model already-admitted
    // Media results; they do not certify a native object, install or ACK.
    pub(super) fn opened_author() -> (
        tempfile::TempDir,
        AppState,
        mondrian_core::NativeAudioAuthorProposal,
        std::sync::Arc<super::super::audio_rendering::TimelineAudioPcmRenderer>,
    ) {
        let (directory, mut state, proposal) = pending_open_with_parameter(true);
        let mut program = state.active_sequence().unwrap().clone();
        program.audio_program.outputs[0].strip.pre_fader.processors.clear();
        let mut renderer = super::super::audio_rendering::TimelineAudioPcmRenderer::new(
            program.clone(),
            vec![program],
            state.asset_library_handle().unwrap(),
            std::sync::Arc::clone(&state.audio_source_cache),
            state.execution_resources.decision().audio.runtime_grant,
            mondrian_audio::AudioAuditionOverlay::default(),
            state.audio_sample_rate,
            mondrian_core::AudioChannelLayout::Stereo,
        )
        .expect("actual builtin Timeline source, no native qualification");
        renderer.bind_author_snapshot(state.authoring.as_ref().unwrap().execution_snapshot());
        let renderer = std::sync::Arc::new(renderer);
        let context = state.native_audio_editor.as_mut().unwrap();
        context.queued = None;
        context.target = Some(proposal.target());
        context.attachment_target = Some(proposal.target());
        context.surface_renderer = Some(std::sync::Arc::downgrade(&renderer));
        state.native_audio_renderer = Some(std::sync::Arc::downgrade(&renderer));
        (directory, state, proposal, renderer)
    }

    fn accepted_author() -> (
        tempfile::TempDir,
        AppState,
        mondrian_core::NativeAudioAuthorProposal,
        std::sync::Arc<super::super::audio_rendering::TimelineAudioPcmRenderer>,
    ) {
        let fixture = opened_author();
        let (directory, mut state, proposal, renderer) = fixture;
        state
            .observe_native_audio_author_proposal(proposal.clone())
            .expect("original App admission and compilation; no Media source was installed");
        let work = state
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .unwrap();
        assert!(work.gesture.is_some());
        assert!(work.outstanding.as_ref().unwrap().proposal.same_retained_proposal(&proposal));
        assert!(!work.outstanding.as_ref().unwrap().queued);
        (directory, state, proposal, renderer)
    }

    fn ack(target: NativeAudioEditorTarget) -> mondrian_media::AudioPcmNativeAuthorHandoffStatus {
        mondrian_media::AudioPcmNativeAuthorHandoffStatus::Acknowledged {
            target: Box::new(target),
            continuity: mondrian_media::AudioPcmNativeAuthorHandoffContinuity::Preserved,
        }
    }

    #[test]
    fn native_author_work_without_surface_retries_prefix_and_keeps_nonterminal_gesture() {
        let (_directory, mut state, proposal, _renderer) = accepted_author();
        let gesture = state
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .unwrap()
            .gesture
            .as_ref()
            .unwrap()
            .gesture_id();
        state.native_audio_editor = None;
        let selected = state
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .unwrap()
            .retained_state_entry_source(
                Some(proposal.target()),
                state.authoring_session_id().unwrap(),
                proposal.sequence(),
            )
            .expect("captured selection survives presentation removal, not a native grant");
        assert!(std::sync::Arc::ptr_eq(&selected, &_renderer));
        state.observe_native_audio_author_proposal(proposal.clone()).unwrap();
        assert!(state
            .observe_native_audio_author_handoff(proposal.clone(), ack(proposal.target()))
            .unwrap());
        let work = state
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .unwrap();
        assert_eq!(work.gesture.as_ref().unwrap().gesture_id(), gesture);
        assert!(work.outstanding.is_none());
        assert!(work.blocks_source_replacement());
        assert!(
            state.native_audio_editor.is_none(),
            "ACK cannot manufacture Opened"
        );
        assert!(
            state.prepare_project_audio_handoff().is_err(),
            "no surface does not release the actual Session barrier"
        );
    }

    #[test]
    fn native_author_work_late_ack_cannot_attach_or_retarget_replacement_surface() {
        let (_directory, mut state, proposal, _renderer) = accepted_author();
        let mut other = context();
        // Same Session/Sequence is insufficient for a different canonical owner.
        other.payload.session_id = state.authoring_session_id().unwrap();
        other.payload.sequence_id = proposal.sequence();
        other.locator.sequence = proposal.sequence();
        other.surface_renderer = state.native_audio_renderer.clone();
        let target = other.target;
        let attachment = other.attachment_target;
        state.native_audio_editor = Some(other);
        let installed = NativeAudioEditorTarget {
            binding: mondrian_core::AudioProcessorBindingId::new(),
            ..proposal.target()
        };
        use mondrian_media::{
            AudioPcmNativeAuthorHandoffContinuity as Continuity,
            AudioPcmNativeAuthorHandoffStatus as Status,
        };
        assert!(!state
            .observe_native_audio_author_handoff(
                proposal.clone(),
                Status::InstalledAwaitingAcknowledgement {
                    target: Box::new(installed),
                    continuity: Continuity::Preserved,
                }
            )
            .unwrap());
        assert_eq!(
            state
                .native_audio_author_work
                .as_ref()
                .and_then(NativeAudioAuthorWork::single)
                .unwrap()
                .outstanding
                .as_ref()
                .unwrap()
                .installed,
            Some(installed)
        );
        assert!(state.observe_native_audio_author_handoff(proposal, ack(installed)).unwrap());
        let context = state.native_audio_editor.as_ref().unwrap();
        assert_eq!(context.target, target);
        assert_eq!(context.attachment_target, attachment);
        assert!(state
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .unwrap()
            .gesture
            .is_some());
    }

    #[test]
    fn native_author_work_rejects_foreign_prefix_and_refusal_preserves_exact_custody() {
        let (_directory, mut state, proposal, _renderer) = accepted_author();
        let gesture = state
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .unwrap()
            .gesture
            .as_ref()
            .unwrap()
            .gesture_id();
        let foreign = early_proposal(proposal.target(), proposal.sequence());
        assert!(!foreign.same_retained_proposal(&proposal));
        assert!(state.observe_native_audio_author_proposal(foreign.clone()).is_err());
        assert!(!state
            .observe_native_audio_author_handoff(foreign, ack(proposal.target()))
            .unwrap());
        state.native_audio_editor = None;
        assert!(!state
            .observe_native_audio_author_handoff(
                proposal.clone(),
                mondrian_media::AudioPcmNativeAuthorHandoffStatus::Refused(
                    mondrian_media::AudioPcmNativeAuthorHandoffRefusal::ProcessorRejected
                )
            )
            .unwrap());
        let work = state
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .unwrap();
        assert_eq!(work.gesture.as_ref().unwrap().gesture_id(), gesture);
        assert!(work.outstanding.as_ref().unwrap().proposal.same_retained_proposal(&proposal));
        assert!(work.blocks_source_replacement());
    }

    #[test]
    fn native_author_work_existing_gesture_rejects_same_sequence_other_owner_or_processor() {
        for other_owner in [true, false] {
            let (_directory, mut state, proposal, _renderer) = accepted_author();
            state
                .observe_native_audio_author_handoff(proposal.clone(), ack(proposal.target()))
                .unwrap();
            let original = state
                .native_audio_author_work
                .as_ref()
                .and_then(NativeAudioAuthorWork::single)
                .unwrap()
                .gesture
                .as_ref()
                .unwrap()
                .clone();
            let peer = state.active_sequence().unwrap().audio_program.outputs[0]
                .strip
                .pre_fader
                .processors[1]
                .id;
            let context = state.native_audio_editor.as_mut().unwrap();
            let mut next_target = proposal.target();
            if other_owner {
                // Two racks of the same real output are distinct author owners.
                let AudioProcessorRackAddress::ChannelStrip { owner, .. } = context.payload.address
                else {
                    panic!("strip")
                };
                context.owner = NativeAudioAuthorOwner::ChannelStrip {
                    owner,
                    rack: AudioChannelStripRack::PostFader,
                };
            } else {
                context.payload.processor_id = peer;
                context.locator.instance = peer;
                next_target.instance = peer;
                context.target = Some(next_target);
                context.attachment_target = Some(next_target);
            }
            let before =
                serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
            let error = state
                .observe_native_audio_author_proposal(early_proposal(
                    next_target,
                    proposal.sequence(),
                ))
                .expect_err("existing gesture cannot transfer owner");
            assert!(
                error.to_string().contains("another captured owner"),
                "{error:?}"
            );
            let work = state
                .native_audio_author_work
                .as_ref()
                .and_then(NativeAudioAuthorWork::single)
                .unwrap();
            assert_eq!(
                work.gesture.as_ref().unwrap().gesture_id(),
                original.gesture_id()
            );
            assert!(work.outstanding.is_none());
            assert_eq!(
                serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
                before
            );
        }
    }

    #[test]
    fn native_author_work_update_failure_retains_new_gesture_and_original_document() {
        let (_directory, mut state, proposal, _renderer) = opened_author();
        let id = ParameterId::new("clap.param.7").unwrap();
        let invalid = mondrian_core::NativeAudioAuthorProposal::project(
            proposal.target(),
            proposal.sequence(),
            1,
            mondrian_core::NativeAudioAuthorBoundaryFacts {
                completion: mondrian_core::NativeAudioAuthorCompletion::Nonterminal,
                open_parameter_edits: 0,
                group_depth: 1,
            },
            vec![mondrian_core::NativeAudioAuthorCallback::Parameter {
                parameter: id,
                phase: mondrian_core::NativeAudioParameterCallbackPhase::Value,
                value_bits: Some(2.0_f64.to_bits()),
            }],
            None,
        )
        .unwrap();
        let before = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
        let generation = state.authoring.as_ref().unwrap().author_generation();
        let error = state
            .observe_native_audio_author_proposal(invalid)
            .expect_err("Session range validation after begin");
        assert!(
            matches!(&error, MondrianError::WorkflowStepFailed { step_id, .. } if step_id == "native_audio_authoring"),
            "{error:?}"
        );
        let work = state
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .expect("begin custody survives update error");
        assert!(work.gesture.is_some());
        assert!(work.outstanding.is_none());
        assert!(work.blocks_source_replacement());
        assert_eq!(
            state.authoring.as_ref().unwrap().author_generation(),
            generation
        );
        assert_eq!(
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
            before
        );
        assert!(state.prepare_project_audio_handoff().is_err());
    }

    #[test]
    fn native_author_work_finish_failure_retains_prefix_and_open_is_busy_but_close_can_queue() {
        let (_directory, mut state, proposal, _renderer) = accepted_author();
        let payload = state.native_audio_editor.as_ref().unwrap().payload.clone();
        let queued = state.native_audio_editor.as_ref().unwrap().queued.clone();
        assert!(state.control_native_audio_editor_product(payload.clone()).is_err());
        assert_eq!(state.native_audio_editor.as_ref().unwrap().queued, queued);
        let mut close = payload;
        close.open = false;
        // No Media source in this model: the original queue can refuse Close,
        // but the App must reach it instead of the author-busy Open gate.
        state.control_native_audio_editor_product(close).unwrap();
        assert!(state
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .unwrap()
            .outstanding
            .as_ref()
            .unwrap()
            .proposal
            .same_retained_proposal(&proposal));
        // A terminal ParametersOnly result cannot preserve this real processor's
        // opaque seed. Session refusal must not consume the gesture/prefix.
        let terminal = mondrian_core::NativeAudioAuthorProposal::project(
            proposal.target(),
            proposal.sequence(),
            1,
            mondrian_core::NativeAudioAuthorBoundaryFacts {
                completion: mondrian_core::NativeAudioAuthorCompletion::ParametersOnly,
                open_parameter_edits: 0,
                group_depth: 0,
            },
            vec![mondrian_core::NativeAudioAuthorCallback::GroupFinished],
            None,
        )
        .unwrap();
        state
            .observe_native_audio_author_handoff(proposal.clone(), ack(proposal.target()))
            .unwrap();
        state.observe_native_audio_author_proposal(terminal.clone()).unwrap();
        assert!(state
            .observe_native_audio_author_handoff(terminal.clone(), ack(terminal.target()))
            .is_err());
        let work = state
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .unwrap();
        assert!(work.gesture.is_some());
        assert!(work.outstanding.as_ref().unwrap().proposal.same_retained_proposal(&terminal));
    }

    #[test]
    fn native_author_work_terminal_ack_without_context_finishes_once_and_is_not_busy() {
        let (_directory, mut state, proposal, _renderer) = accepted_author();
        state
            .observe_native_audio_author_handoff(proposal.clone(), ack(proposal.target()))
            .unwrap();
        let terminal = mondrian_core::NativeAudioAuthorProposal::project(
            proposal.target(),
            proposal.sequence(),
            2,
            mondrian_core::NativeAudioAuthorBoundaryFacts {
                completion: mondrian_core::NativeAudioAuthorCompletion::CoherentConfiguration,
                open_parameter_edits: 0,
                group_depth: 0,
            },
            vec![mondrian_core::NativeAudioAuthorCallback::GroupFinished],
            Some(vec![9, 8].into()),
        )
        .unwrap();
        state.observe_native_audio_author_proposal(terminal.clone()).unwrap();
        state.native_audio_editor = None;
        assert!(state
            .observe_native_audio_author_handoff(terminal.clone(), ack(terminal.target()))
            .unwrap());
        let work = state
            .native_audio_author_work
            .as_ref()
            .and_then(NativeAudioAuthorWork::single)
            .unwrap();
        assert!(work.gesture.is_none());
        assert!(work.outstanding.is_none());
        assert!(
            !work.blocks_source_replacement(),
            "a settled slot is not a busy transaction"
        );
        let history = state.authoring.as_ref().unwrap().history().diagnostics();
        assert_eq!(history.undo_entries, 1);
        assert_eq!(history.pending_entries, 0);
        assert!(!state
            .observe_native_audio_author_handoff(terminal.clone(), ack(terminal.target()))
            .unwrap());
        assert_eq!(
            state.authoring.as_ref().unwrap().history().diagnostics(),
            history
        );
        assert!(state.native_audio_editor.is_none());
    }

    #[test]
    fn native_author_work_actual_source_close_clears_both_slots_only_after_success() {
        for serial_close in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let library =
                mondrian_assets::AssetLibrary::open(directory.path().join("library")).unwrap();
            let mut state = AppState::with_audio_processor_resolver(std::sync::Arc::new(
                mondrian_audio::BuiltInAudioProcessorResolver,
            ));
            let mut sequence = mondrian_timeline::Sequence::new("Settled author custody close");
            sequence.audio_program.outputs[0].strip.pre_fader.processors.push(
                mondrian_timeline::audio::AudioProcessorInstance::built_in(
                    mondrian_timeline::audio::BUILTIN_GAIN_DEFINITION_ID,
                    1,
                ),
            );
            let sequence_id = sequence.id;
            state.test_set_sequence(Some(sequence));
            state.test_set_asset_library(Some(library));
            state.synchronize_playback_authoring_binding().unwrap();
            let source =
                state.audio_playback_renderer("custody Source-close control").unwrap().unwrap();
            let anchor = mondrian_core::AudioSamplePosition::new(
                0,
                mondrian_core::AudioSampleRate::new(state.audio_sample_rate).unwrap(),
            );
            state
                .commit_audio_playback("custody Source-close control", anchor, Some(source))
                .unwrap();
            assert!(state.project_audio_source_is_current());
            let mut surface = context();
            surface.attachment_target = None;
            state.native_audio_editor = Some(surface);
            state.native_audio_author_work =
                Some(NativeAudioAuthorWork::Single(NativeAudioSingleAuthorWork {
                    session_id: state.authoring_session_id().unwrap(),
                    sequence_id,
                    renderer: state.native_audio_renderer.clone().unwrap(),
                    gesture: None,
                    outstanding: None,
                    state_entry_accepted: true,
                }));
            // No native processor is modeled: this is the original registered
            // builtin Source/Media Closed or clear boundary, not native proof.
            if serial_close {
                let ticket = state.begin_project_audio_close().unwrap().unwrap();
                assert!(
                    state.finish_project_audio_close(&ticket).is_err(),
                    "a queued query is not Closed"
                );
                assert!(state.native_audio_editor.is_some());
                assert!(state.native_audio_author_work.is_some());
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                loop {
                    state.poll_project_audio_close_queue().unwrap();
                    match state.audio_playback.source_close_status(&ticket).unwrap() {
                        mondrian_media::AudioPcmSourceCloseStatus::Closed => break,
                        mondrian_media::AudioPcmSourceCloseStatus::Pending => {}
                        mondrian_media::AudioPcmSourceCloseStatus::Refused(error) => {
                            panic!("{error:?}")
                        }
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "original Source-close query did not settle"
                    );
                    std::thread::yield_now();
                }
                assert!(
                    state.native_audio_author_work.is_some(),
                    "Closed observation alone does not clear App custody"
                );
                state.finish_project_audio_close(&ticket).unwrap();
            } else {
                state.prepare_project_audio_handoff().unwrap();
            }
            assert!(state.native_audio_editor.is_none());
            assert!(state.native_audio_author_work.is_none());
            assert!(!state.audio_playback.native_editor_source_available());
            assert!(state.shutdown_audio_playback_and_wait().all_workers_terminated());
        }
    }
}
