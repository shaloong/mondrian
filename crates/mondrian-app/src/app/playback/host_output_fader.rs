//! App classification and consumption of the restricted retained-F capability.
use super::*;

/// Classification only. Audio independently proves the full compiled shape,
/// native obligations, resource overlap and actual processing frontier.
fn selected_static_output_fader_only(before: &Sequence, after: &Sequence) -> bool {
    let Some(old_output) = before.audio_program.outputs.first() else {
        return false;
    };
    let Some(new_output) = after.audio_program.outputs.first() else {
        return false;
    };
    if old_output.id != new_output.id
        || old_output.strip.fader_automation.is_some()
        || new_output.strip.fader_automation.is_some()
        || old_output.strip.fader_db.to_bits() == new_output.strip.fader_db.to_bits()
    {
        return false;
    }
    let mut normalized = after.clone();
    normalized.audio_program.outputs[0].strip.fader_db = old_output.strip.fader_db;
    before.author_state_eq_ignoring_revision(&normalized)
}

impl AppState {
    /// Keep the original generic policy only for an explicitly NotApplicable
    /// installed factory set with no native observation sources. Source presence
    /// and UnsupportedAdapter alone cannot prove that distinction.
    pub(in crate::app) fn host_output_fader_native_renderer(
        &self,
    ) -> mondrian_core::Result<Option<Arc<TimelineAudioPcmRenderer>>> {
        if !self.audio_playback.native_editor_source_available() {
            return Ok(None);
        }
        let derived = self.native_audio_renderer.as_ref().ok_or_else(|| {
            transport_action_error(
                "authoring_commit",
                "the original audio Runtime is unavailable",
            )
        })?;
        // Identity before upgrade: never acquire a retiring stale Runtime's last
        // strong owner on this control thread.
        let erased: std::sync::Weak<dyn AudioPcmRenderer> = derived.clone();
        if !self.audio_playback.matches_source_weak(&erased) {
            return Err(transport_action_error(
                "authoring_commit",
                "the original audio Source was superseded",
            ));
        }
        let renderer = derived.upgrade().ok_or_else(|| {
            transport_action_error(
                "authoring_commit",
                "the original audio Runtime is unavailable",
            )
        })?;
        let (requirement, observations) = renderer.host_output_fader_native_requirement()?;
        use mondrian_core::NativeChildSettlementRequirement as Requirement;
        match (requirement, observations) {
            (Requirement::Unavailable, _) => Err(transport_action_error(
                "authoring_commit",
                "the installed audio factory native ownership declaration is unavailable",
            )),
            (Requirement::NotApplicable, false) => Ok(None),
            // An in-process native adapter can truthfully have no child owner.
            (Requirement::NotApplicable, true) | (Requirement::Required, _) => Ok(Some(renderer)),
        }
    }

    /// None preserves the ordinary transaction policy. A matching live change
    /// must use retained-F publication or fail before its author ticket installs.
    pub(in crate::app) fn prepare_playing_host_output_fader(
        &self,
        candidates: &[Sequence],
    ) -> mondrian_core::Result<Option<(Arc<TimelineAudioPcmRenderer>, AudioPlaybackAuthorBinding)>>
    {
        if !self.is_playing() || !self.audio_playback.native_editor_source_available() {
            return Ok(None);
        }
        let Some((session_id, before)) = self.authoring_session_id().zip(self.active_sequence())
        else {
            return Ok(None);
        };
        let Some(after) = candidates.iter().find(|sequence| sequence.id == before.id) else {
            return Ok(None);
        };
        if !selected_static_output_fader_only(before, after) {
            return Ok(None);
        }
        let Some(renderer) = self.host_output_fader_native_renderer()? else {
            return Ok(None);
        };
        self.validate_audio_source_replacement_admission(
            "authoring_commit",
            "Host output fader publication",
        )?;
        // The original postcommit binding consumer may seek when these fields
        // disagree. Prove its non-seek branch before retaining this DSP owner;
        // the static fader edit and exclusive App borrow preserve these facts.
        let end_frame = self.last_content_frame()?;
        let engine = self.playback_engine.snapshot();
        if self.playback_engine.sequence_id() != Some(before.id)
            || engine.position.time_base != before.time_base()
            || engine.position.frame > end_frame
        {
            return Err(transport_action_error(
                "authoring_commit",
                "Playback binding must settle before retained Host fader publication",
            ));
        }
        let current = self.current_audio_playback_author_binding()?.ok_or_else(|| {
            transport_action_error("authoring_commit", "no actual audio author binding")
        })?;
        if self.audio_playback_author_binding.as_ref() != Some(&current) {
            return Err(transport_action_error(
                "authoring_commit",
                "the installed audio author binding must settle before Host fader publication",
            ));
        }
        let next_closure = self.compile_audio_author_closure(session_id, after, candidates)?;
        let next_binding = AudioPlaybackAuthorBinding { closure: next_closure, ..current };
        let audition = self.audio_monitoring.audition_overlay(session_id, before);
        if !renderer.same_presentation_execution(
            &audition,
            self.audio_sample_rate,
            AUDIO_OUTPUT_LAYOUT,
        ) {
            return Err(transport_action_error(
                "authoring_commit",
                "audio execution changed before Host fader publication",
            ));
        }
        Ok(Some((renderer, next_binding)))
    }

    pub(in crate::app) fn publish_prepared_host_output_fader(
        &mut self,
        prepared: mondrian_editor_state::session::PreparedSequenceEdit,
        renderer: Arc<TimelineAudioPcmRenderer>,
        binding: AudioPlaybackAuthorBinding,
    ) -> mondrian_core::Result<mondrian_editor_state::AuthoringCommit> {
        let program = Arc::clone(binding.closure.root_program());
        let author = self.authoring.as_mut().expect("prepared edit retains its Authoring Session");
        let AppAudioPlayback::Available(playback) = &mut self.audio_playback else {
            return Err(transport_action_error(
                "authoring_commit",
                "Audio Playback is unavailable",
            ));
        };
        // The exact &mut Session remains exclusively borrowed across dependent
        // Source publication; no author ABA or replacement is possible here.
        let (commit, receipt) = renderer
            .publish_host_output_fader(program, playback, || {
                let commit = author.install_prepared_sequence_edit(prepared);
                (commit, author.execution_snapshot())
            })
            .map_err(|error| transport_action_error("authoring_commit", error))?;
        self.audio_playback_author_binding = Some(binding);
        self.invalidate_native_audio_parameter_presentation();
        tracing::debug!(
            frontier = ?receipt.frontier,
            identity = ?receipt.identity,
            unresolved_factory_owners = receipt.unresolved_factory_owners,
            "published retained terminal Host fader at the actual DSP frontier"
        );
        Ok(commit)
    }
}
