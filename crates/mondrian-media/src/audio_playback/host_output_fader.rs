//! Borrow the one actual Media Source publication without rotating PCM time.

use super::*;

impl AudioPlayback {
    /// Publish a terminal Host output fader over this exact installed renderer.
    ///
    /// Media proves only its existing Source/queue/control facts. The caller
    /// additionally owns App admission and an exclusively held actual Runtime
    /// which proves terminal shape, NoEditor/NoNotes/NoNested and its DSP frontier.
    /// This first capability requires completed native bindings; ordinary
    /// sources without a Ready native owner are explicitly outside its scope.
    ///
    /// The supplied verifier is read from that Runtime before this call. No
    /// renderer getter is called here: it could lock the same Runtime again.
    /// Success carries the actual new claim and migrated native record count.
    /// The callback must return Err before any publication, or perform the
    /// Source CAS and infallible ownership moves, release the Source guard,
    /// install the prepared author ticket and return Ok while Runtime/metadata
    /// remain exclusively held. Unwind recovery is not a rollback contract.
    ///
    /// Outer errors are Media admission; inner errors belong to the callback.
    /// Neither rejected layer changes the current claim, FIFO or Media epoch.
    pub fn with_host_output_fader_publication<T, E>(
        &mut self,
        expected: &std::sync::Weak<dyn AudioPcmRenderer>,
        actual: &AudioOwnerInstallationVerifier,
        publish: impl FnOnce(
            &AudioOwnerInstallationVerifier,
            &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
            &ExecutionCancellationToken,
        ) -> Result<(T, AudioOwnerInstallationClaim, usize), E>,
    ) -> Result<Result<T, E>, AudioPlaybackError> {
        self.ensure_source_close_idle()?;
        self.ensure_render_execution_available()?;
        if !self.matches_source_weak(expected) {
            return Err(AudioPlaybackError::HostOutputFaderSourceChanged);
        }
        // in_flight includes completed PCM not yet accepted by the sole poller.
        // Existing diagnostic/handoff fields likewise include their completions.
        // App/Core still enforce the native editor work they actually own.
        if self.in_flight != 0
            || self.native_diagnostic_in_flight.is_some()
            || self.native_author_handoff_in_flight.is_some()
            || self.native_author_state_entry_request.is_some()
            || self.native_control_hold.is_some()
            || self.native_control_output_retirement.is_some()
            || self.native_editor_attachment.is_some()
            || self.render_blocked
            || self.generation_cancellation.is_canceled()
            || self.native_control_cancellation.is_canceled()
            || self.native_owner_binding_status != Some(AudioPcmNativeOwnerBindingStatus::Ready)
        {
            return Err(AudioPlaybackError::HostOutputFaderBusy);
        }
        {
            let queue = self.render_queue.state.lock();
            if queue.stopped || !queue.pending.is_empty() {
                return Err(AudioPlaybackError::HostOutputFaderBusy);
            }
        }
        if !self.owner_installation.owns_registration(&actual.registration()) {
            return Err(AudioOwnerInstallationError::WrongRegistration.into());
        }
        let previous = self
            .owner_installation
            .current_claim()?
            .ok_or(AudioOwnerInstallationError::NotInstalled)?;
        actual.validate(&previous)?;
        let reservation = self
            .owner_installation
            .reserve_install(&actual.registration(), Some(&previous))?;
        let mut authorization =
            self.owner_installation.authorize_runtime_installation(reservation)?;
        let outcome = publish(
            actual,
            &mut authorization,
            &self.native_control_cancellation,
        );
        Ok(match outcome {
            Ok((value, claim, native_bindings)) => {
                // This trusted result carries the actual new claim and the
                // count of records just migrated, never the old Ready cache.
                // No business refusal remains after its successful Source CAS.
                assert!(
                    actual.validate(&claim).is_ok(),
                    "Host fader provider must return its actual current Source publication"
                );
                self.native_owner_binding_status = Some(if native_bindings == 0 {
                    AudioPcmNativeOwnerBindingStatus::Unavailable(
                        AudioPcmNativeOwnerBindingRefusal::UnsupportedAdapter,
                    )
                } else {
                    AudioPcmNativeOwnerBindingStatus::Ready
                });
                Ok(value)
            }
            Err(error) => Err(error),
        })
    }
}
