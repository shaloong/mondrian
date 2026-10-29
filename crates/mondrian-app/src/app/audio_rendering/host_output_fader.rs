//! Retained Host math publication; no native proposal, Bind or Ack is minted.
use super::*;

impl TimelineAudioPcmRenderer {
    /// Cold control observation of the actual installed factories. The original
    /// declaration getter may enter an opaque adapter; this is not RT or
    /// statefulness evidence. Both facts come from the same Runtime lock.
    pub(in crate::app) fn host_output_fader_native_requirement(
        &self,
    ) -> mondrian_core::Result<(mondrian_core::NativeChildSettlementRequirement, bool)> {
        let state = self.state.try_lock().ok_or_else(|| {
            audio_render_error("host_output_fader", "the original audio Runtime is busy")
        })?;
        Ok((
            state.delivery.installed_native_child_settlement_requirement(),
            state.delivery.native_observations_supported(),
        ))
    }

    /// Keep the actual Runtime and permission metadata exclusive until canonical
    /// authoring has installed its original prepared ticket. The private caller
    /// retains the same &mut AuthoringSession across this entire operation.
    pub(in crate::app) fn publish_host_output_fader<C>(
        self: &Arc<Self>,
        next_program: Arc<mondrian_audio::CompiledAudioProgram>,
        playback: &mut AudioPlayback,
        install_author: impl FnOnce() -> (C, mondrian_editor_state::AuthorExecutionSnapshot),
    ) -> mondrian_core::Result<(C, mondrian_audio::HostOutputFaderHandoffReceipt)> {
        // Keep both callable/candidate and retired Plan owners outside the
        // guards, including prepublication errors. Neither owns a second author.
        let mut install_author = Some(install_author);
        let mut prepared = None;
        let mut state = self.state.try_lock().ok_or_else(|| {
            audio_render_error("host_output_fader", "the original audio Runtime is busy")
        })?;
        let mut metadata = self.author_snapshot.try_write().ok_or_else(|| {
            audio_render_error(
                "host_output_fader",
                "audio author permission metadata is busy",
            )
        })?;
        let admission = metadata
            .as_ref()
            .ok_or_else(|| audio_render_error("host_output_fader", "no current author snapshot"))?
            .enter_commit()
            .map_err(|error| audio_render_error("host_output_fader", error.to_string()))?;
        // The old author read guard MUST NOT span its own Session writer.
        drop(admission);
        let verifier = state.delivery.native_owner_installation_verifier().ok_or_else(|| {
            audio_render_error(
                "host_output_fader",
                "the Runtime has no actual Source registration",
            )
        })?;
        let erased: Arc<dyn AudioPcmRenderer> = self.clone();
        let expected = Arc::downgrade(&erased);
        let outcome = playback.with_host_output_fader_publication(
            &expected,
            &verifier,
            |destination, authorization, cancellation| {
                prepared = Some(
                    state
                        .delivery
                        .prepare_host_output_fader_handoff(
                            next_program,
                            destination.clone(),
                            cancellation,
                        )
                        .map_err(|error| {
                            audio_render_error("host_output_fader", error.to_string())
                        })?,
                );
                let receipt = state
                    .delivery
                    .install_host_output_fader_handoff(
                        prepared.as_mut().expect("this exact synchronous preparation"),
                        authorization,
                        cancellation,
                    )
                    .map_err(|error| audio_render_error("host_output_fader", error.to_string()))?;
                // The Source guard has ended. No Result/SDK work follows CAS.
                // This trusted closure calls only the original infallible
                // author install and takes that exact Session's fresh snapshot.
                let (commit, snapshot) =
                    install_author.take().expect("one prepared author install")();
                let retired_snapshot = metadata.replace(Arc::new(snapshot));
                let claim = receipt.claim.clone();
                let native_bindings = receipt.native_bindings;
                Ok::<_, mondrian_core::MondrianError>((
                    (commit, receipt, retired_snapshot),
                    claim,
                    native_bindings,
                ))
            },
        );
        drop(metadata);
        drop(state);
        // Prepared owns any retired Plan/scratch/lease owners after success.
        drop(prepared);
        drop(install_author);
        drop(erased);
        let (commit, receipt, retired_snapshot) = outcome
            .map_err(|error| audio_render_error("host_output_fader", error.to_string()))??;
        drop(retired_snapshot);
        Ok((commit, receipt))
    }
}
