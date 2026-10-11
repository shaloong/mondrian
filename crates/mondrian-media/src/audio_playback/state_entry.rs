//! Actual output retirement before a destructive whole-root author state entry.
//! One cold request is retained from the existing serial queue; this is not a
//! second native queue, state issuer or PCM store.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AuthorStateEntryPhase {
    WaitingForOutput,
    OutputRetired { anchor: AudioSamplePosition },
    InstalledAwaitingAck { anchor: AudioSamplePosition },
    InstalledReady { anchor: AudioSamplePosition },
}

impl AudioPlayback {
    /// Read-only address of the actual retained output-quiescence transition.
    /// This address grants no native or author permission.
    pub fn native_author_state_entry_target(
        &self,
    ) -> Option<mondrian_core::NativeAudioEditorTarget> {
        match self.native_control_hold {
            Some(NativeControlHold::AuthorStateEntry { target, .. })
                if !self.shutdown_requested =>
            {
                Some(target)
            }
            _ => None,
        }
    }
    pub(super) fn retain_state_entry_request(
        &mut self,
        work: Box<native_author::NativeAuthorHandoffWork>,
    ) {
        if self.native_author_state_entry_request.is_some() {
            self.handoff_retired_render_owners(
                VecDeque::from([RenderWork::NativeAuthorHandoff(work)]),
                None,
            );
            return;
        }
        self.native_author_state_entry_request = Some(work);
    }

    // Called at the existing bounded poll after device lifecycle processing.
    // A lifecycle rotation wins this poll; state-entry starts on a later tick.
    pub(super) fn drive_native_author_state_entry(
        &mut self,
        authority: AudioSamplePosition,
        may_rotate: bool,
        events: &mut Vec<AudioPlaybackEvent>,
    ) -> Result<bool, AudioPlaybackError> {
        let Some(work) = self.native_author_state_entry_request.as_ref() else {
            return Ok(false);
        };
        let source_current = self.renderer.as_ref().is_some_and(|renderer| {
            renderer.materialization_id == work.renderer_id
                && Arc::ptr_eq(&renderer.owner, work.request.renderer())
        }) && self
            .owner_installation
            .current_claim()?
            .is_some_and(|claim| claim.generation() == work.expected.generation())
            && !work.cancellation.is_canceled();
        if !source_current {
            if let Some(work) = self.native_author_state_entry_request.take() {
                self.handoff_retired_render_owners(
                    VecDeque::from([RenderWork::NativeAuthorHandoff(work)]),
                    None,
                );
            }
            return Ok(false);
        }
        let target = work.request.expected_target();
        let phase = match self.native_control_hold {
            Some(NativeControlHold::AuthorStateEntry { target: current, phase })
                if current == target =>
            {
                Some(phase)
            }
            Some(NativeControlHold::AuthorPending { target: current, .. }) if current == target => {
                None
            }
            _ => return Ok(false),
        };
        if phase.is_none() {
            if !may_rotate {
                return Ok(false);
            }
            let final_output = self.output.snapshot();
            let final_media_anchor = self.media_anchor;
            let previous_generation = self.generation;
            self.reprime_prevalidated(
                authority,
                false,
                self.renderer.clone(),
                PcmRetirementPolicy::BeginNativeHold,
            )?;
            self.native_control_hold = Some(NativeControlHold::AuthorStateEntry {
                target,
                phase: AuthorStateEntryPhase::WaitingForOutput,
            });
            self.native_control_output_retirement = Some(NativeControlOutputRetirement {
                generation: self.generation,
                token: self.quiescence_token,
                stream_generation: final_output.map(|snapshot| snapshot.stream_generation),
                media_anchor: final_media_anchor,
                acknowledged: false,
            });
            self.render_blocked = true;
            self.recovery_preroll = false;
            if let Some(work) = &mut self.native_author_state_entry_request {
                work.generation = self.generation;
            }
            events.push(AudioPlaybackEvent::NativeAuthorStateEntryRequired {
                previous_generation,
                pending_generation: self.generation,
                final_output,
                final_media_anchor,
            });
            return Ok(true);
        }
        if let Some(AuthorStateEntryPhase::OutputRetired { .. }) = phase {
            let Some(mut work) = self.native_author_state_entry_request.take() else {
                return Ok(false);
            };
            // This private bit is set only after the actual output token AND
            // original serial PCM work are retired and the clock anchor sealed.
            work.output_quiescent = true;
            work.generation = self.generation;
            let identity = (work.generation, work.renderer_id);
            match self.render_queue.push(RenderWork::NativeAuthorHandoff(work)) {
                Ok(()) => self.native_author_handoff_in_flight = Some(identity),
                Err(RenderWork::NativeAuthorHandoff(work)) => {
                    self.native_author_state_entry_request = Some(work)
                }
                Err(work) => self.handoff_retired_render_owners(VecDeque::from([work]), None),
            }
        }
        Ok(false)
    }

    pub(super) fn author_state_entry_output_retired(&mut self, anchor: AudioSamplePosition) {
        if let Some(NativeControlHold::AuthorStateEntry {
            target,
            phase: AuthorStateEntryPhase::WaitingForOutput,
        }) = self.native_control_hold
        {
            self.native_control_hold = Some(NativeControlHold::AuthorStateEntry {
                target,
                phase: AuthorStateEntryPhase::OutputRetired { anchor },
            });
        }
    }

    pub(super) fn publish_author_state_entry_install(
        &mut self,
        target: mondrian_core::NativeAudioEditorTarget,
        acknowledged: bool,
    ) {
        if let Some(NativeControlHold::AuthorStateEntry { phase, .. }) = self.native_control_hold {
            let anchor = match phase {
                AuthorStateEntryPhase::OutputRetired { anchor }
                | AuthorStateEntryPhase::InstalledAwaitingAck { anchor }
                | AuthorStateEntryPhase::InstalledReady { anchor } => anchor,
                AuthorStateEntryPhase::WaitingForOutput => return,
            };
            self.native_control_hold = Some(NativeControlHold::AuthorStateEntry {
                target,
                phase: if acknowledged {
                    AuthorStateEntryPhase::InstalledReady { anchor }
                } else {
                    AuthorStateEntryPhase::InstalledAwaitingAck { anchor }
                },
            });
            self.render_blocked = true;
        }
    }

    /// Release an acknowledged explicit state-entry transition after its exact
    /// output retirement. `anchor` is the current transport authority: a later
    /// user seek may supersede the retired output's historical anchor.
    /// This always admits fresh Enter/preroll, never Continue.
    /// Transport permission is separately retained/retired by PlaybackEngine.
    pub fn finish_native_author_state_entry(
        &mut self,
        target: mondrian_core::NativeAudioEditorTarget,
        anchor: AudioSamplePosition,
    ) -> Result<bool, AudioPlaybackError> {
        if !matches!(self.native_control_hold,Some(NativeControlHold::AuthorStateEntry {
            target:current,phase:AuthorStateEntryPhase::InstalledReady {..},
        }) if current==target)
            || self.native_control_output_retirement.is_some()
            || self.in_flight != 0
            || !self
                .owner_installation
                .current_claim()?
                .is_some_and(|claim| claim.generation() == target.installed_generation)
        {
            return Ok(false);
        }
        self.validate_reprime_anchor_pure(anchor)?;
        self.ensure_render_execution_available()?;
        self.validate_reprime_output()?;
        self.reprime_prevalidated(
            anchor,
            false,
            self.renderer.clone(),
            PcmRetirementPolicy::FreshContinuity,
        )?;
        self.native_control_hold = None;
        self.render_blocked = false;
        Ok(true)
    }
}
