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

pub(super) struct NativeAudioEditorProductContext {
    payload: AudioNativeEditorPayload,
    locator: NativeAudioEditorLocator,
    owner: NativeAudioAuthorOwner,
    schemas: BTreeMap<ParameterId, mondrian_core::automation::ParameterSchema>,
    parameter_observation: Option<mondrian_core::NativeAudioParameterObservation>,
    queued: Option<Intent>,
    target: Option<NativeAudioEditorTarget>,
    attachment_target: Option<NativeAudioEditorTarget>,
    gesture: Option<NativeAudioAuthorGesture>,
    outstanding: Option<NativeAudioEditorPendingAuthor>,
    renderer: Option<std::sync::Weak<super::audio_rendering::TimelineAudioPcmRenderer>>,
    state_entry_accepted: bool,
}

struct NativeAudioEditorPendingAuthor {
    proposal: mondrian_core::NativeAudioAuthorProposal,
    programs: Option<std::sync::Arc<[mondrian_audio::NativeAudioAuthorProgram]>>,
    snapshot: std::sync::Arc<mondrian_editor_state::AuthorExecutionSnapshot>,
    installed: Option<NativeAudioEditorTarget>,
    queued: bool,
}

impl NativeAudioEditorProductContext {
    /// An inactive target after a genuine Close is presentation metadata. Only
    /// live attachment/control or undelivered author work blocks Source retirement.
    pub(super) fn blocks_source_replacement(&self) -> bool {
        self.queued.is_some()
            || self.attachment_target.is_some()
            || self.gesture.is_some()
            || self.outstanding.is_some()
    }

    /// Select the captured exact source during an actual Media state-entry
    /// hold. This is metadata selection only: Media verifies the same Arc,
    /// registered source and current claim before retaining any control work.
    pub(super) fn retained_state_entry_source(
        &self,
        target: Option<NativeAudioEditorTarget>,
        session: mondrian_editor_state::AuthoringSessionId,
        sequence: mondrian_core::SequenceId,
    ) -> Option<std::sync::Arc<super::audio_rendering::TimelineAudioPcmRenderer>> {
        if self.payload.session_id != session || self.payload.sequence_id != sequence {
            return None;
        }
        let renderer = self.renderer.as_ref()?.upgrade()?;
        if let Some(target) = target {
            let outstanding = self.outstanding.as_ref()?;
            if outstanding.proposal.target() != target && outstanding.installed != Some(target) {
                return None;
            }
        } else if !self.state_entry_accepted || !renderer.native_author_snapshot_current() {
            return None;
        }
        Some(renderer)
    }
    fn observe_attachment(
        &mut self,
        observation: mondrian_core::NativeAudioEditorAttachmentObservation,
    ) {
        if self.attachment_target != Some(observation.previous)
            || !self.locator.matches_target(observation.current)
            || self.outstanding.is_some()
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
            gesture: None,
            outstanding: None,
            renderer: None,
            state_entry_accepted: false,
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
        if self.native_audio_editor.as_ref().is_some_and(|context| {
            context.queued.is_some()
                || (payload.open && (context.gesture.is_some() || context.outstanding.is_some()))
        }) {
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
                context.renderer = self.native_audio_renderer.clone();
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
                    context.queued.is_some() || context.outstanding.is_some(),
                )
            })
    }
}

impl AppState {
    pub(super) fn observe_native_audio_editor_attachment(
        &mut self,
        observation: mondrian_core::NativeAudioEditorAttachmentObservation,
    ) {
        if let Some(context) = &mut self.native_audio_editor {
            context.observe_attachment(observation);
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

    pub(super) fn observe_native_audio_author_proposal(
        &mut self,
        proposal: mondrian_core::NativeAudioAuthorProposal,
    ) -> Result<()> {
        use mondrian_core::{
            NativeAudioAuthorCallback as Callback, NativeAudioParameterCallbackPhase as Phase,
        };
        use mondrian_timeline::{
            AudioAutomationTarget, NativeAudioAuthorUpdate, NativeAudioParameterWrite,
            NativeAudioParameterWriteMode,
        };
        let Some(context) = self.native_audio_editor.as_ref() else {
            return Ok(());
        };
        if let Some(pending) = &context.outstanding {
            if pending.proposal.same_retained_proposal(&proposal) {
                return self.queue_native_audio_author_handoff();
            }
            // Another public proposal cannot transfer a private retained batch.
            return Err(failure(
                "native author delivery changed before acknowledgement",
            ));
        }
        if context.target.is_none()
            && context.renderer.is_none()
            && context.attachment_target.is_none()
            && context.gesture.is_none()
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
        context
            .renderer
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| failure("native editor's captured renderer has retired"))?;
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
        let mut parameters = Vec::with_capacity(proposal.callbacks().len());
        let mut references = processor
            .parameters
            .iter()
            .map(|(id, parameter)| {
                (
                    id.clone(),
                    parameter.automation.as_ref().map(|curve| !curve.keyframes.is_empty()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for callback in proposal.callbacks() {
            match callback {
                Callback::ParameterReferencesCleared { parameter, flags } => {
                    if flags & !7 != 0 {
                        return Err(failure("native clear flags changed"));
                    }
                    // Modulation-only clear is an explicit NoChange: this
                    // current canonical model has no host modulation binding.
                    if flags & 3 != 0 {
                        references.insert(parameter.clone(), None);
                        parameters.push(
                            mondrian_timeline::NativeAudioParameterOperation::ClearAutomation {
                                parameter_id: parameter.clone(),
                            },
                        );
                    }
                }
                Callback::ParameterValuesRescanned { kind, values } => {
                    // Actual get_value facts refresh only an existing unkeyed
                    // reference. No key is created, keyed playback remains the
                    // sole curve, and a preceding Clear remains absent.
                    if kind.refreshes_values() {
                        for (id, bits) in values {
                            let schema = context
                                .schemas
                                .get(id)
                                .ok_or_else(|| failure("rescan schema changed"))?;
                            let parameter = processor
                                .parameters
                                .get(id)
                                .filter(|actual| actual.schema == *schema)
                                .ok_or_else(|| failure("rescan parameter changed"))?;
                            if references.get(id) != Some(&Some(false)) {
                                continue;
                            }
                            let value = f64::from_bits(*bits);
                            if !value.is_finite() {
                                return Err(failure("rescan value is not finite"));
                            }
                            parameters.push(
                                mondrian_timeline::NativeAudioParameterOperation::from(
                                    NativeAudioParameterWrite {
                                        schema: Box::new(parameter.schema.clone()),
                                        value,
                                        mode: NativeAudioParameterWriteMode::Unkeyed,
                                    },
                                ),
                            );
                        }
                    }
                }
                Callback::Parameter { parameter: id, phase, value_bits }
                | Callback::ParameterAtExecution { parameter: id, phase, value_bits, .. } => {
                    let schema = context.schemas.get(id).ok_or_else(|| {
                        failure("native callback lacks its pinned parameter schema")
                    })?;
                    let parameter = processor
                        .parameters
                        .get(id)
                        .filter(|actual| actual.schema == *schema)
                        .ok_or_else(|| failure("native parameter schema changed"))?;
                    if *phase != Phase::Value {
                        continue;
                    }
                    let value = value_bits
                        .map(f64::from_bits)
                        .filter(|value| value.is_finite())
                        .ok_or_else(|| {
                            failure("native value is not a finite definition-domain scalar")
                        })?;
                    let mode = if let Callback::ParameterAtExecution { position, .. } = callback {
                        let target = AudioAutomationTarget::ProcessorParameter {
                            rack: context.payload.address,
                            processor_id: context.payload.processor_id,
                            parameter_id: id.clone(),
                        };
                        NativeAudioParameterWriteMode::AtOwnerTime {
                            position: mondrian_timeline::audio_automation_execution_time(
                                sequence,
                                &target,
                                *position,
                                context.payload.occurrence,
                            )
                            .map_err(failure)?,
                        }
                    } else if references.get(id).is_none_or(|state| state != &Some(true)) {
                        NativeAudioParameterWriteMode::Unkeyed
                    } else {
                        let target = AudioAutomationTarget::ProcessorParameter {
                            rack: context.payload.address,
                            processor_id: context.payload.processor_id,
                            parameter_id: id.clone(),
                        };
                        NativeAudioParameterWriteMode::AtOwnerTime {
                            position: mondrian_timeline::audio_automation_author_time(
                                sequence,
                                &target,
                                context.payload.sequence_time,
                                context.payload.occurrence,
                            )
                            .map_err(failure)?,
                        }
                    };
                    references.insert(
                        id.clone(),
                        Some(matches!(
                            mode,
                            NativeAudioParameterWriteMode::AtOwnerTime { .. }
                        )),
                    );
                    parameters.push(mondrian_timeline::NativeAudioParameterOperation::from(
                        NativeAudioParameterWrite {
                            schema: Box::new(parameter.schema.clone()),
                            value,
                            mode,
                        },
                    ));
                }
                Callback::DirtyChanged(_) | Callback::GroupStarted | Callback::GroupFinished => {}
            }
        }
        let payload = context.payload.clone();
        let owner = context.owner;
        let gesture = context.gesture.clone();
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
        let update = NativeAudioAuthorUpdate {
            parameters,
            opaque_state: proposal.state().cloned(),
        };
        let commit = author.update_native_audio_gesture(&gesture, update)?;
        // Publish the actual pending author handle immediately after admission.
        // Later resource/compiler refusal retains this restore point/barrier.
        if let Some(context) = &mut self.native_audio_editor {
            context.gesture = Some(gesture);
        }
        let author =
            self.authoring.as_ref().ok_or_else(|| failure("native author Session closed"))?;
        let snapshot = std::sync::Arc::new(author.execution_snapshot());
        // Store the exact already-accepted batch BEFORE any later compilation
        // or runtime binding. A binding failure must retain this same proposal.
        // A preparation refusal cannot cause this batch to enter Session again.
        let context = self
            .native_audio_editor
            .as_mut()
            .ok_or_else(|| failure("native editor closed"))?;
        context.outstanding = Some(NativeAudioEditorPendingAuthor {
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
        let Some(context) = self.native_audio_editor.as_ref() else {
            return Ok(());
        };
        let Some(pending) = &context.outstanding else {
            return Ok(());
        };
        if pending.queued {
            return Ok(());
        }
        let renderer = context
            .renderer
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| failure("captured native renderer retired"))?;
        if pending.programs.is_none() {
            let author =
                self.authoring.as_ref().ok_or_else(|| failure("native author Session closed"))?;
            if pending.snapshot.enter_commit().is_err() {
                return Err(failure("pending native author snapshot changed"));
            }
            let sequence = author
                .sequence(context.payload.sequence_id)
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
            let context = self
                .native_audio_editor
                .as_mut()
                .ok_or_else(|| failure("native editor closed"))?;
            let pending = context
                .outstanding
                .as_mut()
                .ok_or_else(|| failure("native author batch disappeared"))?;
            pending.programs = Some(programs.into());
        }
        let context = self
            .native_audio_editor
            .as_ref()
            .ok_or_else(|| failure("native editor closed"))?;
        let pending = context
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
        if let Some(context) = &mut self.native_audio_editor
            && let Some(pending) = &mut context.outstanding
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
        let Some(context) = &mut self.native_audio_editor else {
            return Ok(false);
        };
        let Some(pending) = &mut context.outstanding else {
            return Ok(false);
        };
        if !pending.proposal.same_retained_proposal(&proposal) {
            return Ok(false);
        }
        pending.queued = false;
        match status {
            Status::Acknowledged { target, continuity } => {
                let state_entry = continuity
                    == mondrian_media::AudioPcmNativeAuthorHandoffContinuity::StateEntryRequired;
                context.target = Some(*target);
                if proposal.terminal() {
                    let gesture = context
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
                    context.gesture = None;
                }
                context.outstanding = None;
                if state_entry {
                    context.state_entry_accepted = true;
                    context.target = None;
                    context.attachment_target = None;
                    self.set_status_hint(AppMessage::new("native-editor-closed"), false);
                } else {
                    self.set_status_hint(AppMessage::new("native-editor-opened"), false);
                }
                return Ok(true);
            }
            Status::InstalledAwaitingAcknowledgement { target, continuity } => {
                if continuity
                    == mondrian_media::AudioPcmNativeAuthorHandoffContinuity::StateEntryRequired
                {
                    context.attachment_target = None;
                }
                context.target = Some(*target);
                pending.installed = Some(*target);
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
            parameters: Default::default(),
            opaque_state: Some(vec![1, 2, 3, 4].into()),
        };
        let processor_id = instance.id;
        let definition = instance.definition.clone();
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(instance);
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
        assert!(context.renderer.is_none());
        assert!(context.gesture.is_none());
        assert!(context.outstanding.is_none());
        assert!(context.parameter_observation.is_none());
        assert!(!context.state_entry_accepted);
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
            gesture: None,
            outstanding: None,
            renderer: None,
            state_entry_accepted: false,
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
        context.observe_attachment(observation);
        assert_eq!(context.target, Some(observation.current));
        assert_eq!(context.attachment_target, Some(old));
        assert_eq!(context.queued, Some(Intent::Close { target: old }));
        assert!(
            context.renderer.is_none(),
            "observation metadata cannot mint a Runtime/author capability"
        );
        let mut newer = observation;
        newer.current.binding = mondrian_core::AudioProcessorBindingId::new();
        newer.current.continuity_epoch = Some(32);
        context.observe_attachment(newer);
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
        context.outstanding = Some(NativeAudioEditorPendingAuthor {
            proposal: proposal.clone(),
            programs: None,
            snapshot: std::sync::Arc::new(session.execution_snapshot()),
            installed: None,
            queued: false,
        });
        let mut retry = newer;
        retry.current.binding = mondrian_core::AudioProcessorBindingId::new();
        retry.current.continuity_epoch = Some(33);
        context.observe_attachment(retry);
        assert_eq!(
            context.target,
            Some(newer.current),
            "outstanding delivery keeps its admitted target"
        );
        assert_eq!(context.attachment_target, Some(old));
        assert!(context
            .outstanding
            .as_ref()
            .expect("retained")
            .proposal
            .same_retained_proposal(&proposal));
        assert_eq!(context.queued, Some(Intent::Close { target: old }));
        // The production exact Handoff Ack clears this field; the same
        // unconsumed original-attachment fact can then refresh on a later Poll.
        context.outstanding = None;
        context.observe_attachment(retry);
        assert_eq!(context.target, Some(retry.current));
        assert_eq!(context.attachment_target, Some(old));
        assert_eq!(context.queued, Some(Intent::Close { target: old }));
    }
    #[test]
    fn native_attachment_app_replacement_clears_visible_view_and_never_implicitly_reopens() {
        let mut context = context();
        let detached = observation(&context, false);
        context.observe_attachment(detached);
        assert_eq!(context.target, Some(detached.current));
        assert_eq!(context.attachment_target, None);
        let mut late = detached;
        late.retained = true;
        context.observe_attachment(late);
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
        context.observe_attachment(wrong);
        assert_eq!(context.target, before);
        let mut stale = observation(&context, true);
        stale.previous.binding = mondrian_core::AudioProcessorBindingId::new();
        context.observe_attachment(stale);
        assert_eq!(context.target, before);
    }
}
