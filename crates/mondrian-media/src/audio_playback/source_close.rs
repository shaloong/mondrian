//! Source-close admission on the original observations queue slot.
//! The ticket is metadata; only AudioPlayback's pending operation owns completion.

use super::*;

/// Opaque identity of one admission for the actual installed renderer.
/// A copy is not a Source permission; every operation revalidates Media's sole slot.
#[derive(Debug, Clone)]
pub struct AudioPcmSourceCloseTicket {
    generation: u64,
    renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
    claim: AudioOwnerInstallationClaim,
}

/// Read-only status of the exact pending close admission.
#[derive(Debug)]
pub enum AudioPcmSourceCloseStatus<'a> {
    /// The original worker has not completed this operation.
    Pending,
    /// The actual query succeeded. Scheduling stays gated until finish/cancel.
    Closed,
    /// Preserve the original error and actual Source; no native suffix was drained.
    Refused(&'a mondrian_core::MondrianError),
}

pub(super) struct SourceCloseAdmission {
    ticket: AudioPcmSourceCloseTicket,
    pub(super) cancellation: ExecutionCancellationToken,
    result: Option<mondrian_core::Result<()>>,
}

pub(super) fn refusal(reason: impl Into<String>) -> mondrian_core::MondrianError {
    mondrian_core::MondrianError::ActionNotExecuted {
        action: "native_author_source_closed".to_owned(),
        reason: reason.into(),
    }
}

pub(super) fn execute(
    renderer: &Arc<dyn AudioPcmRenderer>,
    ticket: &AudioPcmSourceCloseTicket,
    cancellation: &ExecutionCancellationToken,
) -> mondrian_core::Result<()> {
    let validate = || {
        if cancellation.is_canceled() {
            return Err(refusal("source-close admission was cancelled"));
        }
        // Query the actual Runtime registration, against the original opaque claim.
        let verifier = renderer
            .owner_installation_verifier()
            .ok_or_else(|| refusal("source-close Runtime registration is unavailable"))?;
        verifier.validate(&ticket.claim).map_err(|error| refusal(error.to_string()))
    };
    validate()?;
    renderer.validate_native_author_source_closed(cancellation)?;
    validate()
}

impl AudioPlayback {
    pub(super) fn ensure_source_close_idle(&self) -> Result<(), AudioPlaybackError> {
        if self.source_close_admission.is_some() {
            Err(AudioPlaybackError::SourceClosePending)
        } else {
            Ok(())
        }
    }

    fn validate_source_close_obligations(
        &self,
        finishing: Option<&AudioPcmSourceCloseTicket>,
    ) -> Result<(), AudioPlaybackError> {
        let completed_cleanup = matches!(
            self.native_control_hold,
            Some(NativeControlHold::EditorCleanupCompletePaused(_))
        );
        // A completed cleanup record survives Suspend. Only this close epoch's
        // newly issued output obligation can pass to original clear_source;
        // old output retirement must already be settled before request.
        let own_suspend_retirement = completed_cleanup
            && finishing.is_some_and(|ticket| {
                self.native_control_output_retirement.is_some_and(|retirement| {
                    retirement.generation == ticket.generation
                        && retirement.token == self.quiescence_token
                })
            });
        if self.native_editor_attachment.is_some()
            || matches!(self.native_control_hold, Some(hold) if !matches!(hold, NativeControlHold::EditorCleanupCompletePaused(_)))
            || (self.native_control_output_retirement.is_some() && !own_suspend_retirement)
            || self.native_author_handoff_in_flight.is_some()
            || self.native_author_state_entry_request.is_some()
            || self.render_queue.state.lock().pending.iter().any(|work| {
                matches!(
                    work,
                    RenderWork::NativeEditor(_) | RenderWork::NativeAuthorHandoff(_)
                )
            })
        {
            return Err(AudioPlaybackError::SourceClosePending);
        }
        Ok(())
    }

    fn source_close_ticket_matches(&self, ticket: &AudioPcmSourceCloseTicket) -> bool {
        self.source_close_admission.as_ref().is_some_and(|pending| {
            pending.ticket.generation == ticket.generation
                && pending.ticket.renderer_id == ticket.renderer_id
                && pending.ticket.claim.generation() == ticket.claim.generation()
        })
    }

    fn validate_source_close_ticket(
        &self,
        ticket: &AudioPcmSourceCloseTicket,
    ) -> Result<(), AudioPlaybackError> {
        if !self.source_close_ticket_matches(ticket)
            || self.generation != ticket.generation
            || self
                .source_close_admission
                .as_ref()
                .is_none_or(|pending| pending.cancellation.is_canceled())
            || self
                .renderer
                .as_ref()
                .is_none_or(|renderer| renderer.materialization_id != ticket.renderer_id)
        {
            return Err(AudioPlaybackError::SourceCloseSuperseded);
        }
        let current = self
            .owner_installation
            .current_claim()?
            .ok_or(AudioOwnerInstallationError::NotInstalled)?;
        // Both claims were issued by this sole slot. Its generations never reuse.
        if current.generation() != ticket.claim.generation() {
            return Err(AudioPlaybackError::SourceCloseSuperseded);
        }
        Ok(())
    }

    /// Queue one source-closed query for the exact actual installed Source.
    ///
    /// Existing native author/editor obligations refuse before retirement.
    /// One original Suspend cancels PCM/ordinary observations, keeping the
    /// Source claim and native-control token. This does not drain or Ack.
    /// The caller handles an actually absent Source without calling this method.
    pub fn request_source_close(
        &mut self,
        expected: &Arc<dyn AudioPcmRenderer>,
        anchor: AudioSamplePosition,
    ) -> Result<AudioPcmSourceCloseTicket, AudioPlaybackError> {
        self.ensure_source_close_idle()?;
        self.validate_reprime_anchor_pure(anchor)?;
        self.ensure_render_execution_available()?;
        self.validate_reprime_output()?;
        self.validate_source_close_obligations(None)?;
        let renderer = self.renderer.as_ref().ok_or(AudioOwnerInstallationError::NotInstalled)?;
        if !Arc::ptr_eq(&renderer.owner, expected) {
            return Err(AudioOwnerInstallationError::WrongRegistration.into());
        }
        let renderer_id = renderer.materialization_id;
        let claim = self
            .owner_installation
            .current_claim()?
            .ok_or(AudioOwnerInstallationError::NotInstalled)?;
        self.suspend_prevalidated(anchor)?;
        // A refused/cancelled close does not implicitly re-enter or resume PCM.
        self.render_blocked = true;
        let ticket = AudioPcmSourceCloseTicket { generation: self.generation, renderer_id, claim };
        let cancellation = self.generation_cancellation.clone();
        let work = RenderWork::NativeObservations(NativeObservationWork {
            generation: ticket.generation,
            renderer_id,
            renderer: Arc::clone(expected),
            cancellation: cancellation.clone(),
            installation_generation: Some(ticket.claim.generation()),
            operation: NativeObservationOperation::SourceClosed(ticket.clone()),
        });
        self.source_close_admission =
            Some(SourceCloseAdmission { ticket: ticket.clone(), cancellation, result: None });
        match self.render_queue.push(work) {
            Ok(()) => Ok(ticket),
            Err(work) => {
                self.retire_source_close_admission();
                self.handoff_retired_render_owners(VecDeque::from([work]), None);
                Err(AudioPlaybackError::ExecutionUnavailable)
            }
        }
    }

    /// Inspect this operation without consuming its result or releasing its gate.
    /// The original Core error remains owned by Media until explicit retirement.
    pub fn source_close_status<'a>(
        &'a self,
        ticket: &AudioPcmSourceCloseTicket,
    ) -> Result<AudioPcmSourceCloseStatus<'a>, AudioPlaybackError> {
        self.validate_source_close_ticket(ticket)?;
        match self.source_close_admission.as_ref().and_then(|pending| pending.result.as_ref()) {
            None => Ok(AudioPcmSourceCloseStatus::Pending),
            Some(Ok(())) => Ok(AudioPcmSourceCloseStatus::Closed),
            Some(Err(error)) => Ok(AudioPcmSourceCloseStatus::Refused(error)),
        }
    }

    /// Detach only after this actual Source/Runtime/claim completed its query.
    /// This uses the original clear/revocation/retirement path and does not
    /// re-query, reload, drain or acknowledge native author state.
    pub fn finish_source_close(
        &mut self,
        ticket: &AudioPcmSourceCloseTicket,
        anchor: AudioSamplePosition,
    ) -> Result<(), AudioPlaybackError> {
        self.validate_source_close_ticket(ticket)?;
        self.validate_source_close_obligations(Some(ticket))?;
        self.ensure_render_execution_available()?;
        if !matches!(
            self.source_close_admission.as_ref().and_then(|pending| pending.result.as_ref()),
            Some(Ok(()))
        ) {
            return Err(AudioPlaybackError::SourceCloseNotClosed);
        }
        // The serial worker authenticated the actual Runtime before and after
        // the query. Finish uses only the original slot/claim: no foreign getter
        // may block this caller behind a retained alias's Runtime mutex.
        let pending = self.source_close_admission.take();
        let result = self.clear_source(anchor);
        // Original clear can fail at foreign output preflight. Keep the gate
        // only if the original slot/epoch/claim still exists; never fake rollback.
        if result.is_err()
            && self.generation == ticket.generation
            && self
                .renderer
                .as_ref()
                .is_some_and(|renderer| renderer.materialization_id == ticket.renderer_id)
            && self.owner_installation.current_claim().is_ok_and(|claim| {
                claim.is_some_and(|claim| claim.generation() == ticket.claim.generation())
            })
        {
            self.source_close_admission = pending;
        }
        result
    }

    /// Cancel this admission while retaining its actual Source and native suffix.
    /// PCM stays paused/blocked; a later explicit prepare/reprime owns any entry.
    pub fn cancel_source_close(
        &mut self,
        ticket: &AudioPcmSourceCloseTicket,
    ) -> Result<(), AudioPlaybackError> {
        if !self.source_close_ticket_matches(ticket) {
            return Err(AudioPlaybackError::SourceCloseSuperseded);
        }
        self.retire_source_close_admission();
        if self.generation == ticket.generation {
            self.generation_cancellation = ExecutionCancellationToken::new();
        }
        Ok(())
    }

    pub(super) fn retire_source_close_result(&self, result: mondrian_core::Result<()>) {
        if let Err(error) = result {
            enqueue_retired_render_owners(
                &self.render_retirement_queue,
                &self.foreign_owner_retirements,
                RetiredRenderOwners {
                    pending: VecDeque::new(),
                    renderers: VecDeque::new(),
                    errors: VecDeque::from([error]),
                },
            );
        }
    }

    pub(super) fn retire_source_close_admission(&mut self) {
        if let Some(pending) = self.source_close_admission.take() {
            pending.cancellation.cancel();
            if let Some(result) = pending.result {
                self.retire_source_close_result(result);
            }
        }
    }

    pub(super) fn accept_source_close_completion(
        &mut self,
        generation: u64,
        renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
        installation_generation: Option<std::num::NonZeroU64>,
        result: mondrian_core::Result<()>,
    ) {
        let expected = self.source_close_admission.as_ref().map(|pending| pending.ticket.clone());
        if let Some(ticket) = expected
            && ticket.generation == generation
            && ticket.renderer_id == renderer_id
            && installation_generation == Some(ticket.claim.generation())
            && self.validate_source_close_ticket(&ticket).is_ok()
            && self
                .source_close_admission
                .as_ref()
                .is_some_and(|pending| pending.result.is_none())
        {
            if let Some(pending) = &mut self.source_close_admission {
                pending.result = Some(result);
            }
        } else {
            self.retire_source_close_result(result);
            self.stale_completion_count = self.stale_completion_count.saturating_add(1);
        }
    }
}
