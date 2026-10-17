//! Portable exact-Ack cancellation ordering; no SDK or Some-prefix permission.

use super::*;

#[derive(Debug, Default, PartialEq, Eq)]
struct AckCursors {
    sessions: Vec<AudioSessionCursorEvidence>,
    processors: Vec<AudioProcessorCursorEvidence>,
}
impl AudioContinuityEvidenceVisitor for AckCursors {
    fn session(&mut self, evidence: AudioSessionCursorEvidence) -> Result<(), AudioExecutionError> {
        self.sessions.push(evidence);
        Ok(())
    }
    fn processor(
        &mut self,
        evidence: AudioProcessorCursorEvidence,
    ) -> Result<(), AudioExecutionError> {
        self.processors.push(evidence);
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
struct AckOwners {
    root: AudioSessionCursorIdentity,
    target: mondrian_core::NativeAudioEditorTarget,
    generation: Option<NonZeroU64>,
    cursors: AckCursors,
    binding: Option<NativeAudioObjectBindRequest>,
    creates: usize,
    binds: usize,
    entries: usize,
    processed: Vec<(i64, f64)>,
    opens: usize,
    closes: usize,
}
fn owners(fixture: &HandoffFixture) -> AckOwners {
    let mut cursors = AckCursors::default();
    fixture
        .runtime
        .visit_continuity_evidence(&mut cursors)
        .expect("actual Session cursors");
    let probe = fixture.probe.lock().expect("actual test owner");
    AckOwners {
        root: fixture.runtime.resource_root_identity(),
        target: fixture.target,
        generation: fixture
            .source
            .current_claim()
            .expect("actual Source")
            .map(|claim| claim.generation()),
        cursors,
        binding: probe.binding,
        creates: probe.creates,
        binds: probe.binds,
        entries: probe.entries,
        processed: probe.processed.clone(),
        opens: probe.opens,
        closes: probe.closes,
    }
}

#[test]
fn native_author_ack_pre_cancel_retains_installed_receipt_without_backend_ack() {
    let mut fixture = HandoffFixture::new();
    fixture.block(0, 0.5);
    let proposal = fixture.value(0.75);
    fixture.install(&proposal, 0.75);
    let before = owners(&fixture);
    let retained = {
        let probe = fixture.probe.lock().expect("actual source");
        (
            probe.retained,
            probe.retained_configuration.clone(),
            probe.accepted_seed.clone(),
        )
    };
    let cancellation = ExecutionCancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        fixture
            .runtime
            .acknowledge_native_author_parameter_handoff(&proposal, &cancellation),
        Err(NativeAudioParameterHandoffError::Admission(
            NativeAudioAuthorAdmissionError::Cancelled
        ))
    ));
    assert_eq!(owners(&fixture), before);
    {
        let probe = fixture.probe.lock().expect("same held source");
        assert_eq!((probe.ack_attempts, probe.accepted_acks), (0, 0));
        assert_eq!(
            (
                probe.retained,
                probe.retained_configuration.clone(),
                probe.accepted_seed.clone()
            ),
            retained
        );
    }
    let mut untouched = [93.0];
    assert!(matches!(
        fixture.runtime.render_into(
            AudioRenderRequest { start_sample: 1, frames: 1 },
            &mut untouched
        ),
        Err(AudioExecutionError::NativeAuthorPending(_))
    ));
    assert_eq!(untouched, [93.0]);
    assert_eq!(owners(&fixture), before);
    fixture.ack(&proposal);
    assert_eq!(owners(&fixture), before);
    let probe = fixture.probe.lock().expect("one actual accepted Ack");
    assert_eq!((probe.ack_attempts, probe.accepted_acks), (1, 1));
    assert_eq!(probe.retained, None);
    drop(probe);
    fixture.block(1, 0.75);
}

#[test]
fn native_author_ack_callback_cancel_returns_confirmed_receipt_once_without_reinstall() {
    let mut fixture = HandoffFixture::new();
    fixture.block(0, 0.5);
    let proposal = fixture.value(0.75);
    fixture.install(&proposal, 0.75);
    let before = owners(&fixture);
    let cancellation = ExecutionCancellationToken::new();
    fixture
        .probe
        .lock()
        .expect("arm only after installation")
        .cancel_on_accepted_ack = Some(cancellation.clone());
    let receipt = fixture
        .runtime
        .acknowledge_native_author_parameter_handoff(&proposal, &cancellation)
        .expect("a valid irreversible Ack cannot become cancellation failure");
    assert!(
        cancellation.is_canceled(),
        "the actual Ack callback canceled this token"
    );
    assert_eq!(receipt.target, before.target);
    assert_eq!(Some(receipt.claim.generation()), before.generation);
    fixture
        .runtime
        .native_owner_installation_verifier()
        .expect("same registration")
        .validate(&receipt.claim)
        .expect("the returned original claim remains valid");
    assert_eq!(
        receipt.continuity,
        NativeAudioParameterHandoffContinuity::Preserved
    );
    assert_eq!(receipt.unresolved_factory_owners, None);
    assert_eq!(
        owners(&fixture),
        before,
        "no second install, create, Bind, Enter or DSP"
    );
    {
        let probe = fixture.probe.lock().expect("actual accepted Ack");
        assert_eq!((probe.ack_attempts, probe.accepted_acks), (1, 1));
        assert_eq!(probe.retained, None);
        assert_eq!(probe.retained_configuration, None);
        assert!(probe.cancel_on_accepted_ack.is_none());
    }
    assert!(matches!(
        fixture.runtime.acknowledge_native_author_parameter_handoff(
            &proposal,
            &ExecutionCancellationToken::new()
        ),
        Err(NativeAudioParameterHandoffError::Admission(
            NativeAudioAuthorAdmissionError::Owner(AudioNativeOwnerError::OwnerUnavailable)
        ))
    ));
    assert_eq!(owners(&fixture), before);
    let probe = fixture.probe.lock().expect("no second backend Ack");
    assert_eq!((probe.ack_attempts, probe.accepted_acks), (1, 1));
    drop(probe);
    fixture.block(1, 0.75);
}

#[test]
fn native_author_ack_revoked_claim_refuses_before_backend_and_retains_receipt() {
    let mut fixture = HandoffFixture::new();
    fixture.block(0, 0.5);
    let proposal = fixture.value(0.75);
    fixture.install(&proposal, 0.75);
    let retained = fixture.probe.lock().expect("actual source").retained;
    assert!(retained.is_some());
    fixture
        .source
        .revoke()
        .expect("the real owner revoked its current Source claim");
    let before = owners(&fixture);
    assert_eq!(before.generation, None);
    assert!(matches!(
        fixture.runtime.acknowledge_native_author_parameter_handoff(
            &proposal,
            &ExecutionCancellationToken::new()
        ),
        Err(NativeAudioParameterHandoffError::Owner(
            AudioNativeOwnerError::Installation(
                mondrian_core::AudioOwnerInstallationError::NotInstalled
            )
        ))
    ));
    assert_eq!(owners(&fixture), before);
    let probe = fixture.probe.lock().expect("old private receipt is not acknowledged");
    assert_eq!((probe.ack_attempts, probe.accepted_acks), (0, 0));
    assert_eq!(probe.retained, retained);
}
