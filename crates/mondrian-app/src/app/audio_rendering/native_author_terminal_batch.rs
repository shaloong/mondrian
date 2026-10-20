//! Concrete terminal-frontier adapter on Media's original producer/retirement lane.
//! The UI shares only its actually published author snapshot, never native owners.

use super::*;
use mondrian_audio::{NativeAudioTerminalBatchInstallReceipt, PreparedNativeAudioTerminalBatch};
use mondrian_core::{NativeAudioAuthorProposal, NativeAudioEditorTarget};
use mondrian_media::{
    AudioPcmNativeAuthorHandoff as Handoff, AudioPcmNativeAuthorHandoffContinuity as Continuity,
    AudioPcmNativeAuthorHandoffIntent as Intent, AudioPcmNativeAuthorHandoffMode as Mode,
    AudioPcmNativeAuthorHandoffOutcome as InlineOutcome,
    AudioPcmNativeAuthorHandoffRefusal as Refusal,
    AudioPcmNativeAuthorTerminalBatchOutcome as Outcome,
};

type Publication = Arc<Mutex<Option<Arc<mondrian_editor_state::AuthorExecutionSnapshot>>>>;

struct TerminalBatchRequest {
    // Retire native/Plan/scratch owners on the producer or its original
    // retirement lane, before this provider releases its renderer aliases.
    prepared: Option<PreparedNativeAudioTerminalBatch>,
    installed: Option<NativeAudioTerminalBatchInstallReceipt>,
    // Derived only from the same prepared original anchor after actual CAS.
    installed_anchor: Option<NativeAudioEditorTarget>,
    next_ack: usize,
    anchor: NativeAudioAuthorProposal,
    proposals: Arc<[NativeAudioAuthorProposal]>,
    programs: Arc<[mondrian_audio::NativeAudioAuthorProgram]>,
    publication: Publication,
    renderer: Arc<TimelineAudioPcmRenderer>,
    source: Arc<dyn AudioPcmRenderer>,
}

pub(super) fn request(
    renderer: &Arc<TimelineAudioPcmRenderer>,
    anchor: NativeAudioAuthorProposal,
    proposals: Arc<[NativeAudioAuthorProposal]>,
    programs: Arc<[mondrian_audio::NativeAudioAuthorProgram]>,
    publication: Publication,
) -> Box<dyn Handoff> {
    Box::new(TerminalBatchRequest {
        prepared: None,
        installed: None,
        installed_anchor: None,
        next_ack: 0,
        anchor,
        proposals,
        programs,
        publication,
        renderer: Arc::clone(renderer),
        source: renderer.clone(),
    })
}

impl TerminalBatchRequest {
    fn installed_pending(&self) -> Outcome {
        Outcome::InstalledAwaitingAcknowledgement {
            claim: self.installed.as_ref().expect("actual batch install receipt").claim.clone(),
            target: self.installed_anchor.expect("actual prepared anchor target"),
        }
    }

    fn acknowledge_remaining(&mut self, cancellation: &ExecutionCancellationToken) -> Outcome {
        if self.installed.is_none() {
            return Outcome::Refused(Refusal::Superseded);
        }
        if cancellation.is_canceled() || !self.renderer.native_author_snapshot_current() {
            return self.installed_pending();
        }
        while self.next_ack < self.proposals.len() {
            let result =
                self.renderer.state.lock().delivery.acknowledge_native_author_parameter_handoff(
                    &self.proposals[self.next_ack],
                    cancellation,
                );
            if result.is_err() {
                return self.installed_pending();
            }
            // Only a successful real Ack advances the prefix. A later failure
            // resumes here and never acknowledges an already consumed member.
            self.next_ack += 1;
        }
        // No Runtime/metadata/author guard survives this point. A destructor
        // panic reaches Media's original catch/Poison boundary, retaining its
        // credit rather than reporting Acknowledged or reusing this lane.
        drop(self.prepared.take());
        Outcome::Acknowledged {
            claim: self.installed.as_ref().expect("actual batch install receipt").claim.clone(),
            target: self.installed_anchor.expect("actual prepared anchor target"),
        }
    }
}

impl Handoff for TerminalBatchRequest {
    fn mode(&self) -> Mode {
        Mode::TerminalBatch
    }
    fn renderer(&self) -> &Arc<dyn AudioPcmRenderer> {
        &self.source
    }
    fn proposal(&self) -> &NativeAudioAuthorProposal {
        &self.anchor
    }
    fn expected_target(&self) -> NativeAudioEditorTarget {
        self.installed_anchor.unwrap_or_else(|| self.anchor.target())
    }
    fn intent(&self) -> Intent {
        if self.installed.is_some() {
            Intent::Acknowledge
        } else {
            Intent::Install
        }
    }

    fn prepare_terminal_batch(
        &mut self,
        destination: mondrian_core::AudioOwnerInstallationVerifier,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<(), Refusal> {
        if cancellation.is_canceled() {
            return Err(Refusal::Cancelled);
        }
        if self.prepared.is_some()
            || self.installed.is_some()
            || self.next_ack != 0
            || self.proposals.iter().filter(|p| p.same_retained_proposal(&self.anchor)).count() != 1
        {
            return Err(Refusal::Superseded);
        }
        let prepared = self
            .renderer
            .state
            .lock()
            .delivery
            .prepare_native_author_terminal_batch(
                &self.programs,
                &self.proposals,
                destination,
                cancellation,
            )
            .map_err(|error| native_handoff_refusal(&error))?;
        self.prepared = Some(prepared);
        Ok(())
    }

    fn apply_prepared_terminal_batch(
        &mut self,
        authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        cancellation: &ExecutionCancellationToken,
    ) -> Outcome {
        if self.installed.is_some() {
            return self.acknowledge_remaining(cancellation);
        }
        if cancellation.is_canceled() {
            return Outcome::Refused(Refusal::Cancelled);
        }
        let snapshot = match self.publication.lock().as_ref().cloned() {
            Some(snapshot) => snapshot,
            None => return Outcome::Refused(Refusal::Superseded),
        };
        let Some(prepared) = &mut self.prepared else {
            return Outcome::Refused(Refusal::Superseded);
        };
        // Prepare Arc ownership before the final guards. The caller writes this
        // unique host slot only after its actual candidate commit succeeds.
        let next_snapshot = Some(Arc::clone(&snapshot));
        {
            let mut state = self.renderer.state.lock();
            let installed = state.delivery.install_native_author_terminal_batch_with_commit_guard(
                prepared,
                authorization,
                cancellation,
                || {
                    // Audio invokes acquire only AFTER all native/resource/
                    // source checks. No metadata guard spans those SDK calls.
                    let metadata = self.renderer.author_snapshot.write();
                    let author = snapshot
                        .enter_commit()
                        .map_err(|_| mondrian_audio::AudioNativeOwnerError::OwnerUnavailable)?;
                    Ok((metadata, author))
                },
            );
            let (receipt, (mut metadata, author)) = match installed {
                Ok(installed) => installed,
                Err(error) => return Outcome::Refused(native_handoff_refusal(&error)),
            };
            // Actual CAS custody is recorded before any post-install operation.
            // It can never return to preparation or execute a second CAS.
            self.installed = Some(receipt);
            let retired = std::mem::replace(&mut *metadata, next_snapshot);
            drop(author);
            drop(metadata);
            drop(retired);
        }
        // Audio checked every member record before CAS; prepare checked that
        // the original anchor is exactly one of those members. A violated
        // internal invariant must poison the original lane, never fabricate a
        // target or return an uninstalled refusal after irreversible CAS.
        self.installed_anchor = Some(
            self.prepared
                .as_ref()
                .expect("retained batch owner")
                .installed_target(&self.anchor)
                .expect("installed original batch anchor"),
        );
        self.acknowledge_remaining(cancellation)
    }

    fn acknowledge_terminal_batch(&mut self, cancellation: &ExecutionCancellationToken) -> Outcome {
        self.acknowledge_remaining(cancellation)
    }

    // This provider cannot be driven by Inline's single-member protocol.
    fn continuity_policy(&self, _: &ExecutionCancellationToken) -> Result<Continuity, Refusal> {
        Err(Refusal::UnsupportedAdapter)
    }
    fn acknowledge(&self, _: &ExecutionCancellationToken) -> InlineOutcome {
        InlineOutcome::Refused(Refusal::UnsupportedAdapter)
    }
    fn apply(
        &self,
        _: mondrian_core::AudioOwnerInstallationVerifier,
        _: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        _: &ExecutionCancellationToken,
    ) -> InlineOutcome {
        InlineOutcome::Refused(Refusal::UnsupportedAdapter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;

    fn fixture() -> (
        tempfile::TempDir,
        Arc<TimelineAudioPcmRenderer>,
        NativeAudioAuthorProposal,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let library = AssetLibrary::open(directory.path().join("library")).unwrap();
        let sequence = Sequence::new("Terminal provider protocol control");
        let sequence_id = sequence.id;
        let output = sequence.audio_program.outputs[0].id;
        let renderer = Arc::new(
            TimelineAudioPcmRenderer::new(
                sequence,
                Vec::new(),
                library,
                Arc::new(AudioSourceCache::new(48_000)),
                AudioRuntimeResourceGrant::new(1, 64 * 1024 * 1024, 16 * 1024 * 1024),
                AudioAuditionOverlay::default(),
                48_000,
                AudioChannelLayout::Stereo,
            )
            .unwrap(),
        );
        // Shape-only Core metadata. No native processor or installed Source is
        // manufactured by these protocol/early-refusal ordinary controls.
        let root_plan = mondrian_core::AudioPreparedPlanId::new();
        let root_session = mondrian_core::AudioRenderSessionId::new();
        let target = NativeAudioEditorTarget {
            root_plan,
            root_session,
            local_plan: root_plan,
            local_session: root_session,
            instance: mondrian_core::AudioProcessorInstanceId::new(),
            owner: mondrian_core::NativeAudioEditorOccurrenceOwner::Output(output),
            insertion: mondrian_core::NativeAudioEditorInsertion::PreFader,
            continuity_epoch: None,
            binding: mondrian_core::AudioProcessorBindingId::new(),
            installed_generation: NonZeroU64::new(1).unwrap(),
        };
        let anchor = proposal(target, sequence_id);
        (directory, renderer, anchor)
    }

    fn proposal(
        target: NativeAudioEditorTarget,
        sequence: mondrian_core::SequenceId,
    ) -> NativeAudioAuthorProposal {
        NativeAudioAuthorProposal::project(
            target,
            sequence,
            1,
            mondrian_core::NativeAudioAuthorBoundaryFacts {
                completion: mondrian_core::NativeAudioAuthorCompletion::ParametersOnly,
                open_parameter_edits: 0,
                group_depth: 0,
            },
            vec![mondrian_core::NativeAudioAuthorCallback::GroupFinished],
            None,
        )
        .unwrap()
    }

    #[test]
    fn terminal_provider_keeps_original_source_and_rejects_inline_protocol() {
        let (_directory, renderer, anchor) = fixture();
        let source = mondrian_core::AudioOwnerInstallationSource::new();
        let (registration, verifier) = source.register().unwrap();
        let mut authorization = source
            .authorize_runtime_installation(source.reserve_install(&registration, None).unwrap())
            .unwrap();
        let publication = Arc::new(Mutex::new(None));
        let request = renderer.native_author_terminal_batch_request(
            anchor.clone(),
            vec![anchor.clone()].into(),
            Vec::new().into(),
            publication.clone(),
        );
        let erased: Arc<dyn AudioPcmRenderer> = renderer.clone();
        assert!(Arc::ptr_eq(request.renderer(), &erased));
        assert!(request.proposal().same_retained_proposal(&anchor));
        assert_eq!(request.expected_target(), anchor.target());
        assert_eq!(request.mode(), Mode::TerminalBatch);
        assert_eq!(request.intent(), Intent::Install);
        let cancellation = ExecutionCancellationToken::new();
        // These wrong-protocol calls must not acquire the Runtime at all.
        let _held = renderer.state.lock();
        assert_eq!(
            request.continuity_policy(&cancellation),
            Err(Refusal::UnsupportedAdapter)
        );
        assert!(matches!(
            request.apply(verifier, &mut authorization, &cancellation),
            InlineOutcome::Refused(Refusal::UnsupportedAdapter)
        ));
        assert!(matches!(
            request.acknowledge(&cancellation),
            InlineOutcome::Refused(Refusal::UnsupportedAdapter)
        ));
        assert!(publication.lock().is_none());
        assert!(source.current_claim().unwrap().is_none());
    }

    #[test]
    fn terminal_provider_refuses_cancelled_or_unpublished_and_wrong_anchor_before_runtime() {
        let (_directory, renderer, anchor) = fixture();
        let source = mondrian_core::AudioOwnerInstallationSource::new();
        let (registration, verifier) = source.register().unwrap();
        let mut authorization = source
            .authorize_runtime_installation(source.reserve_install(&registration, None).unwrap())
            .unwrap();
        let publication = Arc::new(Mutex::new(None));
        let mut request = renderer.native_author_terminal_batch_request(
            anchor.clone(),
            vec![anchor.clone()].into(),
            Vec::new().into(),
            publication.clone(),
        );
        let cancelled = ExecutionCancellationToken::new();
        cancelled.cancel();
        let cancellation = ExecutionCancellationToken::new();
        let _held = renderer.state.lock();
        assert_eq!(
            request.prepare_terminal_batch(verifier.clone(), &cancelled),
            Err(Refusal::Cancelled)
        );
        assert!(matches!(
            request.apply_prepared_terminal_batch(&mut authorization, &cancelled),
            Outcome::Refused(Refusal::Cancelled)
        ));
        assert!(matches!(
            request.apply_prepared_terminal_batch(&mut authorization, &cancellation),
            Outcome::Refused(Refusal::Superseded)
        ));
        assert!(matches!(
            request.acknowledge_terminal_batch(&cancellation),
            Outcome::Refused(Refusal::Superseded)
        ));
        let distinct = proposal(anchor.target(), anchor.sequence());
        assert!(!distinct.same_retained_proposal(&anchor));
        for members in [vec![], vec![distinct], vec![anchor.clone(), anchor.clone()]] {
            let mut wrong = renderer.native_author_terminal_batch_request(
                anchor.clone(),
                members.into(),
                Vec::new().into(),
                publication.clone(),
            );
            assert_eq!(
                wrong.prepare_terminal_batch(verifier.clone(), &cancellation),
                Err(Refusal::Superseded)
            );
        }
        assert!(publication.lock().is_none());
        assert!(source.current_claim().unwrap().is_none());
    }
}
